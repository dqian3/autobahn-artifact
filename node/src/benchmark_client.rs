// Copyright(C) Facebook, Inc. and its affiliates.
use anyhow::{Context, Result};
use bytes::BufMut as _;
use bytes::BytesMut;
use clap::{crate_name, crate_version, App, AppSettings};
use env_logger::Env;
use futures::channel;
use futures::future::join_all;
use futures::sink::SinkExt as _;
use futures::stream::StreamExt as _;
use log::{info, warn};
use rand::Rng;
use std::collections::HashMap;
use std::convert::TryInto;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{interval, sleep, Duration, Instant};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tokio::sync::mpsc;

use crypto::SignatureService;
use crypto::Hash;
use crypto::set_crypto_disabled;

use config::KeyPair;
use config::Import as _;

use worker::client_reply::{
    encode_reply_addr, KIND_SAMPLE, REPLY_ADDR_LEN, REPLY_ENTRY_LEN, TAG_LEN,
};

/// Send times of the sampled transactions still awaiting a reply.
///
/// Only the sampled transactions are tracked. Every committed transaction
/// gets a signed reply — that is the replica-side cost being measured — but
/// keeping a send time for all of them would put a hash-map insert and a
/// lock on the client's hot path at tens of thousands of transactions a
/// second, and turn the client into the thing under measurement. The
/// existing log-derived latency samples at exactly the same rate.
type SampleTimes = Arc<Mutex<HashMap<u64, Instant>>>;


#[tokio::main]
async fn main() -> Result<()> {
    let matches = App::new(crate_name!())
        .version(crate_version!())
        .about("Benchmark client for Sailfish.")
        .args_from_usage("<ADDR> 'The network address of the node where to send txs'")
        .args_from_usage("--size=<INT> 'The size of each transaction in bytes'")
        .args_from_usage("--rate=<INT> 'The rate (txs/s) at which to send the transactions'")
        .args_from_usage("--nodes=[ADDR]... 'Network addresses that must be reachable before starting the benchmark.'")
        .args_from_usage("--key=<FILE> 'The file containing the key information for the benchmark.'")
        .args_from_usage("--disable-crypto 'Skip ed25519 signing of submitted transactions (no-crypto baseline).'")
        .args_from_usage("--reply-addr=[ADDR] 'Address to listen on for committed-request replies. Omit to run send-only, as autobahn publishes it.'")
        .get_matches();

    env_logger::Builder::from_env(Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let target = matches
        .value_of("ADDR")
        .unwrap()
        .parse::<SocketAddr>()
        .context("Invalid socket address format")?;
    let size = matches
        .value_of("size")
        .unwrap()
        .parse::<usize>()
        .context("The size of transactions must be a non-negative integer")?;
    let rate = matches
        .value_of("rate")
        .unwrap()
        .parse::<u64>()
        .context("The rate of transactions must be a non-negative integer")?;
    let nodes = matches
        .values_of("nodes")
        .unwrap_or_default()
        .into_iter()
        .map(|x| x.parse::<SocketAddr>())
        .collect::<Result<Vec<_>, _>>()
        .context("Invalid socket address format")?;

    let reply_addr = match matches.value_of("reply-addr") {
        Some(value) => Some(
            value
                .parse::<SocketAddr>()
                .context("Invalid reply socket address format")?,
        ),
        None => None,
    };

    let key_file = matches.value_of("key").unwrap();
    let disable_crypto = matches.is_present("disable-crypto");
    set_crypto_disabled(disable_crypto);
    if disable_crypto {
        info!("Crypto disabled: tx signatures are zeros (no-crypto baseline).");
    }

    info!("Node address: {}", target);

    // NOTE: This log entry is used to compute performance.
    info!("Transactions size: {} B", size);

    // NOTE: This log entry is used to compute performance.
    info!("Transactions rate: {} tx/s", rate);
    info!("Key file provided: {}", key_file);
    

    let secret = KeyPair::import(key_file).context("Failed to load the node's keypair")?;
    let secret_key = secret.secret;

    let signature_service = SignatureService::new(secret_key);

    // The reply address travels inside every transaction, so a replica that
    // commits it can answer without having to have been the one that received
    // it. All-zero means "do not reply", which is what the published client's
    // zero-filled payload already says.
    let reply_bytes = match reply_addr {
        Some(address) => {
            // NOTE: This log entry is used to compute performance.
            info!("Listening for replies on {}", address);
            encode_reply_addr(&address).ok_or_else(|| {
                anyhow::Error::msg("The reply address must be IPv4: it has to fit in the transaction")
            })?
        }
        None => {
            info!("Send-only client: no reply address, replicas will not reply.");
            [0u8; REPLY_ADDR_LEN]
        }
    };

    let mut client = Client {
        target,
        size,
        rate,
        nodes,
        signature_service,
        reply_addr,
        reply_bytes,
    };


    // Wait for all nodes to be online and synchronized.
    client.wait().await;

    // Start the benchmark.
    client.send().await.context("Failed to submit transactions")



    // let mut tx = BytesMut::with_capacity(size + 64); // + 64 for signatures
    // let mut counter = 0;

    // let now = Instant::now();

    // for counter in 0..10000 {
    //     tx.put_u8(0u8); // Sample txs start with 0.
    //     tx.put_u64(counter); // This counter identifies the tx.
    //     tx.resize(size, 0u8);
    //     tx.extend_from_slice(&sign(&mut signature_service, &tx).await);
    //     tx.split().freeze();
    // }

    // let elapsed = now.elapsed().as_secs_f64();

    // info!("Time taken to sign 10000 transactions: {}, rate {}", elapsed, (10000 as f64)/elapsed);

    // Ok(())
}

struct Client {
    target: SocketAddr,  //specifies the worker to connect to
    size: usize,         //specifies the bit size of transactions
    rate: u64,
    nodes: Vec<SocketAddr>,
    // ========= Added for Evaluation purposes =========
    signature_service: SignatureService,
    /// Where we listen for replies, if we asked for any.
    reply_addr: Option<SocketAddr>,
    /// That address, in the form embedded in each transaction.
    reply_bytes: [u8; REPLY_ADDR_LEN],
}

/// Read replies off one replica's connection for as long as it stays open.
///
/// Replies arrive in batches — one frame per committed batch per client — of
/// fixed-width entries: the request's first 9 bytes followed by the
/// replica's signature over the request digest. The signature is carried but
/// not verified here. Verifying it is client-side work; loading the client
/// with per-request crypto is how a client-side ceiling gets mistaken for a
/// replica-side one, and the number this whole path exists to produce is a
/// replica-side one.
async fn read_replies(stream: TcpStream, samples: SampleTimes, replied: Arc<AtomicU64>) {
    let mut transport = Framed::new(stream, LengthDelimitedCodec::new());
    while let Some(frame) = transport.next().await {
        let frame = match frame {
            Ok(frame) => frame,
            Err(e) => {
                warn!("Reply connection closed: {}", e);
                return;
            }
        };

        for entry in frame.chunks_exact(REPLY_ENTRY_LEN) {
            replied.fetch_add(1, Ordering::Relaxed);

            if entry[0] != KIND_SAMPLE {
                continue;
            }
            let id = u64::from_be_bytes(
                entry[1..TAG_LEN].try_into().expect("tag carries a u64"),
            );
            // Take it out of the map: a second reply for the same request
            // (two replicas replying, or a redelivered commit) is counted but
            // only timed once, against its own send.
            let sent_at = samples.lock().unwrap().remove(&id);
            if let Some(sent_at) = sent_at {
                // NOTE: This log entry is used to compute performance.
                info!(
                    "Reply for sampled tx {} after {:.3} ms",
                    id,
                    sent_at.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
    }
}

/// Accept reply connections for the life of the run.
async fn serve_replies(listener: TcpListener, samples: SampleTimes, replied: Arc<AtomicU64>) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("Replica {} connected to send replies", peer);
                tokio::spawn(read_replies(stream, samples.clone(), replied.clone()));
            }
            Err(e) => {
                warn!("Failed to accept a reply connection: {}", e);
                return;
            }
        }
    }
}

async fn sign(signature_service: &mut SignatureService, tx: &BytesMut) -> [u8; 64]
{
    let digest = tx.as_ref().digest();
    let signature = signature_service.request_signature(digest).await;
    signature.flatten()
}


impl Client {
    pub async fn send(&mut self) -> Result<()> {
        const PRECISION: u64 = 20; // Sample precision.
        const BURST_DURATION: u64 = 1000 / PRECISION;

        // The transaction size must be at least 16 bytes to ensure all txs are different.
        if self.size < 16 {
            return Err(anyhow::Error::msg(
                "Transaction size must be at least 16 bytes",
            ));
        }

        // Start listening for replies before the first transaction goes out,
        // so a reply cannot arrive at a closed port.
        let samples: SampleTimes = Arc::new(Mutex::new(HashMap::new()));
        let replied = Arc::new(AtomicU64::new(0));
        if let Some(address) = self.reply_addr {
            // Advertise the routable address to the replicas, but bind the
            // wildcard, the way every node in this codebase does: on a cloud
            // VM the advertised address is the internal one and binding it
            // directly is a needless dependency on how the NIC is configured.
            let mut bind_addr = address;
            bind_addr.set_ip("0.0.0.0".parse().unwrap());
            let listener = TcpListener::bind(bind_addr)
                .await
                .context(format!("failed to listen for replies on {}", bind_addr))?;
            tokio::spawn(serve_replies(listener, samples.clone(), replied.clone()));
        }

        // Connect to the mempool.
        let stream = TcpStream::connect(self.target)
            .await
            .context(format!("failed to connect to {}", self.target))?;

        // ~1s of offered load. Signer uses try_send; never blocks.
        let buf_size = (self.rate as usize).max(4096);
        let (channel_tx, mut channel_rx) = mpsc::channel(buf_size);
        let dropped = Arc::new(AtomicU64::new(0));
        // Send-side counters, mirroring the aspen/flutter clients so the same
        // offered-load analysis applies here. `produced` counts txs the burst
        // loop generated, `dispatched` counts the ones that made it into the
        // channel — their difference is `dropped`. This client is
        // fire-and-forget (no per-tx completion tracking), so there is no
        // `completed` to report; delivered throughput still comes from the
        // node-side commit logs.
        let produced = Arc::new(AtomicU64::new(0));
        let dispatched = Arc::new(AtomicU64::new(0));

        // Submit all transactions.
        let burst = self.rate / PRECISION;
        let tx = BytesMut::with_capacity(self.size + 64); // + 64 for signatures
        let mut counter = 0;
        let mut r :u64 = rand::thread_rng().gen();
        let mut transport = Framed::new(stream, LengthDelimitedCodec::new());
        let interval = interval(Duration::from_millis(BURST_DURATION));
        tokio::pin!(interval);


        // Spawn a task to read from channel and send signed transactions
        tokio::spawn(async move {
            while let Some(message) = channel_rx.recv().await {
                if let Err(e) = transport.send(message).await { //Uses TCP connection to send request to assigned worker. Note: Optimistically only sending to one worker.
                    warn!("Failed to send transaction: {}", e);
                    return;
                }
            }
        });

        // NOTE: This log entry is used to compute performance.
        info!("Start sending transactions");

        'main: loop {
            interval.as_mut().tick().await;
            // Sender task exits on TCP write error (worker disconnected /
            // backpressured under saturation); when it does, channel_rx is
            // dropped and channel_tx becomes closed. Bail cleanly here so
            // we don't keep spawning doomed producers (which would each
            // hit a SendError and panic via .unwrap()).
            if channel_tx.is_closed() {
                warn!("Send channel closed (worker likely overloaded); stopping client");
                break 'main;
            }
            let now = Instant::now();

            let mut tx = tx.clone();
            let counter_copy = counter.clone();
            let mut r_copy = r.clone();
            let size = self.size;
            let mut sig_copy = self.signature_service.clone();

            let channel_tx = channel_tx.clone();
            let dropped_task = dropped.clone();
            let produced_task = produced.clone();
            let dispatched_task = dispatched.clone();
            let reply_bytes = self.reply_bytes;
            let samples_task = samples.clone();

            tokio::spawn(async move {
                for x in 0..burst {
                    // The sampled transaction of this burst, if this is it.
                    // Its send time is recorded once it is actually handed to
                    // the sender, so a dropped transaction is not timed.
                    let mut sampled = None;

                    let msg = if x == counter_copy % burst {
                        // NOTE: This log entry is used to compute performance.
                        info!("Sending sample transaction {}", counter_copy);

                        sampled = Some(counter_copy);

                        tx.put_u8(KIND_SAMPLE); // Sample txs start with 0.
                        tx.put_u64(counter_copy); // This counter identifies the tx.
                        tx.put_slice(&reply_bytes); // Where to send the reply.
                        tx.resize(size, 0u8);

                        tx.extend_from_slice(&sign(&mut sig_copy, &tx).await);

                        tx.split().freeze()
                    } else {
                        r_copy += 1;
                        tx.put_u8(1u8); // Standard txs start with 1.
                        tx.put_u64(r_copy); // Ensures all clients send different txs.
                        tx.put_slice(&reply_bytes); // Where to send the reply.
                        tx.resize(size, 0u8);

                        tx.extend_from_slice(&sign(&mut sig_copy, &tx).await);

                        tx.split().freeze()
                    };

                    produced_task.fetch_add(1, Ordering::Relaxed);
                    match channel_tx.try_send(msg) {
                        Ok(()) => {
                            dispatched_task.fetch_add(1, Ordering::Relaxed);
                            if let Some(id) = sampled {
                                samples_task.lock().unwrap().insert(id, Instant::now());
                            }
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            dropped_task.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                    }
                }
            });
            
            if now.elapsed().as_millis() > BURST_DURATION as u128 {
                // NOTE: This log entry is used to compute performance.
                warn!("Transaction rate too high for this client");
            }

            // Send-side counters once per second. Emitted unconditionally
            // (not only when drops occur) because the offered-load analysis
            // differences consecutive samples: a run that never drops still
            // needs the produced/dispatched series to show whether the client
            // actually kept up with the configured rate.
            // NOTE: This log entry is used to compute performance.
            //
            // `replied` counts the signed replies that came back. With one
            // replier per request it tracks committed throughput; with f+1 it
            // is that many times larger. It is a cross-check on the
            // commit-log throughput, not a replacement for it.
            if counter % PRECISION == 0 {
                info!(
                    "client_stats produced={} dispatched={} dropped={} replied={}",
                    produced.load(Ordering::Relaxed),
                    dispatched.load(Ordering::Relaxed),
                    dropped.load(Ordering::Relaxed),
                    replied.load(Ordering::Relaxed),
                );
            }

            r += burst;
            counter += 1;
        }
        Ok(())
    }

    pub async fn wait(&self) {
        // Wait for all nodes to be online.
        info!("Waiting for all nodes to be online...");
        join_all(self.nodes.iter().cloned().map(|address| {
            tokio::spawn(async move {
                while TcpStream::connect(address).await.is_err() {
                    sleep(Duration::from_millis(10)).await;
                }
            })
        }))
        .await;
    }
}
