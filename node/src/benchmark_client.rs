// Copyright(C) Facebook, Inc. and its affiliates.
use anyhow::{Context, Result};
use bytes::BufMut as _;
use bytes::BytesMut;
use clap::{crate_name, crate_version, App, AppSettings};
use env_logger::Env;
use futures::channel;
use futures::future::join_all;
use futures::sink::SinkExt as _;
use log::{info, warn};
use rand::Rng;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::TcpStream;
use tokio::time::{interval, sleep, Duration, Instant};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tokio::sync::mpsc;

use crypto::SignatureService;
use crypto::Hash;
use crypto::set_crypto_disabled;

use config::KeyPair;
use config::Import as _;


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

    let mut client = Client {
        target,
        size,
        rate,
        nodes,
        signature_service
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

            tokio::spawn(async move {
                for x in 0..burst {
                    let msg = if x == counter_copy % burst {
                        // NOTE: This log entry is used to compute performance.
                        info!("Sending sample transaction {}", counter_copy);

                        tx.put_u8(0u8); // Sample txs start with 0.
                        tx.put_u64(counter_copy); // This counter identifies the tx.
                        tx.resize(size, 0u8);

                        tx.extend_from_slice(&sign(&mut sig_copy, &tx).await);

                        tx.split().freeze()
                    } else {
                        r_copy += 1;
                        tx.put_u8(1u8); // Standard txs start with 1.
                        tx.put_u64(r_copy); // Ensures all clients send different txs.
                        tx.resize(size, 0u8);

                        tx.extend_from_slice(&sign(&mut sig_copy, &tx).await);

                        tx.split().freeze()
                    };

                    produced_task.fetch_add(1, Ordering::Relaxed);
                    match channel_tx.try_send(msg) {
                        Ok(()) => {
                            dispatched_task.fetch_add(1, Ordering::Relaxed);
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
            if counter % PRECISION == 0 {
                info!(
                    "client_stats produced={} dispatched={} dropped={}",
                    produced.load(Ordering::Relaxed),
                    dispatched.load(Ordering::Relaxed),
                    dropped.load(Ordering::Relaxed),
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
