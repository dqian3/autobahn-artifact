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

use crypto::Signer;
use crypto::Hash;
use crypto::set_crypto_disabled;

use config::KeyPair;
use config::Import as _;

use worker::client_reply::{
    encode_reply_addr, KIND_SAMPLE, MAX_REPLIERS, REPLY_ADDR_LEN, REPLY_ENTRY_LEN,
    REPLY_HEADER_LEN, TAG_LEN,
};

/// Send times of the sampled transactions still awaiting a reply.
///
/// Only the sampled transactions are timed. Keeping a send time for all of
/// them would put a hash-map insert and a lock on the client's send path at
/// tens of thousands of transactions a second, and turn the client into the
/// thing under measurement. The existing log-derived latency samples at the
/// same rate.
type SampleTimes = Arc<Mutex<HashMap<u64, Instant>>>;

/// How long a transaction may hold fewer than `quorum` replies before the
/// client gives up on it.
const PARTIAL_REPLY_TIMEOUT: Duration = Duration::from_secs(60);

/// Reply-side state shared by every replica's connection.
struct Replies {
    /// Distinct replicas that must reply before a transaction counts as
    /// committed.
    quorum: usize,
    samples: SampleTimes,
    /// Transactions with some but fewer than `quorum` replies, keyed by tag:
    /// a bit per replier, and when the first reply arrived. Built on the
    /// reply side only, so the send path stays untouched.
    partial: Mutex<HashMap<[u8; TAG_LEN], (u64, Instant)>>,
    /// Transactions that reached `quorum` distinct replies.
    completed: AtomicU64,
    /// Reply entries received, from every replier.
    entries: AtomicU64,
    /// Transactions dropped from `partial` after PARTIAL_REPLY_TIMEOUT.
    abandoned: AtomicU64,
}


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
        .args_from_usage("--tcp-nodelay 'Set TCP_NODELAY on the connection to the worker and on reply connections.'")
        .args_from_usage("--reply-addr=[ADDR] 'Address to listen on for committed-request replies. Omit to run send-only, as autobahn publishes it.'")
        .args_from_usage("--reply-quorum=[INT] 'Distinct replicas that must reply before a transaction counts as committed; match the replicas client_reply_count (default 1).'")
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
    let reply_quorum = matches
        .value_of("reply-quorum")
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("The reply quorum must be a positive integer")?
        .unwrap_or(1)
        .max(1)
        .min(MAX_REPLIERS);

    let key_file = matches.value_of("key").unwrap();
    let disable_crypto = matches.is_present("disable-crypto");
    set_crypto_disabled(disable_crypto);
    if disable_crypto {
        info!("Crypto disabled: tx signatures are zeros (no-crypto baseline).");
    }
    let tcp_nodelay = matches.is_present("tcp-nodelay");
    network::set_tcp_nodelay(tcp_nodelay);
    info!("TCP_NODELAY enabled? {}", tcp_nodelay);

    info!("Node address: {}", target);

    // NOTE: This log entry is used to compute performance.
    info!("Transactions size: {} B", size);

    // NOTE: This log entry is used to compute performance.
    info!("Transactions rate: {} tx/s", rate);
    info!("Key file provided: {}", key_file);
    

    let secret = KeyPair::import(key_file).context("Failed to load the node's keypair")?;
    let signer = Arc::new(Signer::new(&secret.secret));

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
        signer,
        reply_addr,
        reply_bytes,
        reply_quorum,
    };


    // Wait for all nodes to be online and synchronized.
    client.wait().await;

    // Start the benchmark.
    client.send().await.context("Failed to submit transactions")
}

struct Client {
    target: SocketAddr,  //specifies the worker to connect to
    size: usize,         //specifies the bit size of transactions
    rate: u64,
    nodes: Vec<SocketAddr>,
    // ========= Added for Evaluation purposes =========
    signer: Arc<Signer>,
    /// Where we listen for replies, if we asked for any.
    reply_addr: Option<SocketAddr>,
    /// That address, in the form embedded in each transaction.
    reply_bytes: [u8; REPLY_ADDR_LEN],
    /// Distinct replicas that must reply before a transaction counts.
    reply_quorum: usize,
}

/// Read replies off one replica's connection for as long as it stays open.
///
/// Replies arrive in batches — one frame per committed batch per client — led
/// by the replier's committee index and followed by fixed-width entries, each
/// the request's own first 9 bytes echoed back so it can be matched. Nothing
/// else: the reply is a bare ack, unsigned and carrying no proof (see
/// `worker::client_reply` for why).
///
/// A transaction completes on its `quorum`-th distinct replier. A replica
/// answering twice (a redelivered commit) does not count again.
async fn read_replies(stream: TcpStream, replies: Arc<Replies>) {
    let mut transport = Framed::new(stream, LengthDelimitedCodec::new());
    while let Some(frame) = transport.next().await {
        let frame = match frame {
            Ok(frame) => frame,
            Err(e) => {
                warn!("Reply connection closed: {}", e);
                return;
            }
        };
        if frame.len() < REPLY_HEADER_LEN {
            continue;
        }
        let bit = 1u64 << (frame[0] as usize % MAX_REPLIERS);
        let entries = &frame[REPLY_HEADER_LEN..];

        let mut completed = 0u64;
        let mut sampled = Vec::new();
        {
            let mut partial = replies.partial.lock().unwrap();
            for entry in entries.chunks_exact(REPLY_ENTRY_LEN) {
                let complete = if replies.quorum <= 1 {
                    true
                } else {
                    let tag: [u8; TAG_LEN] = entry.try_into().expect("entry is one tag");
                    let complete = match partial.get_mut(&tag) {
                        Some(slot) => {
                            if slot.0 & bit != 0 {
                                false
                            } else {
                                slot.0 |= bit;
                                slot.0.count_ones() as usize >= replies.quorum
                            }
                        }
                        None => {
                            partial.insert(tag, (bit, Instant::now()));
                            false
                        }
                    };
                    if complete {
                        partial.remove(&tag);
                    }
                    complete
                };
                if !complete {
                    continue;
                }
                completed += 1;
                if entry[0] == KIND_SAMPLE {
                    sampled.push(u64::from_be_bytes(
                        entry[1..TAG_LEN].try_into().expect("tag carries a u64"),
                    ));
                }
            }
        }
        replies
            .entries
            .fetch_add((entries.len() / REPLY_ENTRY_LEN) as u64, Ordering::Relaxed);
        replies.completed.fetch_add(completed, Ordering::Relaxed);

        if sampled.is_empty() {
            continue;
        }
        let mut samples = replies.samples.lock().unwrap();
        for id in sampled {
            // Taken out of the map, so a later reply is not timed again.
            if let Some(sent_at) = samples.remove(&id) {
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

/// Give up on transactions that have held fewer than `quorum` replies for
/// PARTIAL_REPLY_TIMEOUT, so a lost reply does not hold memory for the run.
async fn sweep_partial_replies(replies: Arc<Replies>) {
    let mut ticker = interval(Duration::from_secs(10));
    loop {
        ticker.tick().await;
        let mut partial = replies.partial.lock().unwrap();
        let before = partial.len();
        partial.retain(|_, (_, first)| first.elapsed() < PARTIAL_REPLY_TIMEOUT);
        replies
            .abandoned
            .fetch_add((before - partial.len()) as u64, Ordering::Relaxed);
    }
}

/// Accept reply connections for the life of the run.
async fn serve_replies(listener: TcpListener, replies: Arc<Replies>) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                if network::tcp_nodelay() {
                    if let Err(e) = stream.set_nodelay(true) {
                        warn!("Failed to set TCP_NODELAY for {}: {}", peer, e);
                    }
                }
                info!("Replica {} connected to send replies", peer);
                tokio::spawn(read_replies(stream, replies.clone()));
            }
            Err(e) => {
                warn!("Failed to accept a reply connection: {}", e);
                return;
            }
        }
    }
}

fn sign(signer: &Signer, tx: &BytesMut) -> [u8; 64] {
    signer.sign(&tx.as_ref().digest()).flatten()
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
        let replies = Arc::new(Replies {
            quorum: self.reply_quorum,
            samples: Arc::new(Mutex::new(HashMap::new())),
            partial: Mutex::new(HashMap::new()),
            completed: AtomicU64::new(0),
            entries: AtomicU64::new(0),
            abandoned: AtomicU64::new(0),
        });
        if let Some(address) = self.reply_addr {
            // NOTE: This log entry is used to compute performance.
            info!("Reply quorum: {} distinct replicas", self.reply_quorum);
            // Advertise the routable address to the replicas, but bind the
            // wildcard, the way every node in this codebase does: on a cloud
            // VM the advertised address is the internal one and binding it
            // directly is a needless dependency on how the NIC is configured.
            let mut bind_addr = address;
            bind_addr.set_ip("0.0.0.0".parse().unwrap());
            let listener = TcpListener::bind(bind_addr)
                .await
                .context(format!("failed to listen for replies on {}", bind_addr))?;
            tokio::spawn(serve_replies(listener, replies.clone()));
            if self.reply_quorum > 1 {
                tokio::spawn(sweep_partial_replies(replies.clone()));
            }
        }

        // Connect to the mempool.
        let stream = TcpStream::connect(self.target)
            .await
            .context(format!("failed to connect to {}", self.target))?;
        // Without TCP_NODELAY a small transaction waits for the previous
        // segment's ACK, so each send can cost a round trip instead of a hop.
        if network::tcp_nodelay() {
            if let Err(e) = stream.set_nodelay(true) {
                warn!("Failed to set TCP_NODELAY for {}: {}", self.target, e);
            }
        }

        // Sized so it never binds: a whole run's worth of offered load, with a
        // floor for slow clients. The previous `max(rate, 4096)` was about one
        // second at a fast client and six at a slow one, so the cap tightened
        // as the committee grew and per-client rate fell -- the client shed
        // load at a rate that depended on committee size. aspen's clients run
        // uncapped (`max_in_flight: 0`), so a cap here measured the two
        // protocols by different rules under overload. `dropped` stays, and
        // should now read zero; a non-zero value means this is still binding.
        let buf_size = (self.rate as usize * 120).max(200_000);
        let (channel_tx, mut channel_rx) = mpsc::channel(buf_size);
        let dropped = Arc::new(AtomicU64::new(0));
        // Send-side counters, mirroring the aspen/flutter clients so the same
        // offered-load analysis applies here. `produced` counts txs the burst
        // loop generated, `dispatched` counts the ones that made it into the
        // channel — their difference is `dropped`. Sends do not wait on
        // replies; `replied` in the stats line is what completed.
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
            let signer = self.signer.clone();

            let channel_tx = channel_tx.clone();
            let dropped_task = dropped.clone();
            let produced_task = produced.clone();
            let dispatched_task = dispatched.clone();
            let reply_bytes = self.reply_bytes;
            let samples_task = replies.samples.clone();

            // Signing runs on a blocking thread, off the async runtime.
            tokio::task::spawn_blocking(move || {
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

                        tx.extend_from_slice(&sign(&signer, &tx));

                        tx.split().freeze()
                    } else {
                        r_copy += 1;
                        tx.put_u8(1u8); // Standard txs start with 1.
                        tx.put_u64(r_copy); // Ensures all clients send different txs.
                        tx.put_slice(&reply_bytes); // Where to send the reply.
                        tx.resize(size, 0u8);

                        tx.extend_from_slice(&sign(&signer, &tx));

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
            // `replied` counts transactions that reached `reply_quorum`
            // distinct replicas. `reply_entries` counts every reply received,
            // and `reply_abandoned` the transactions given up on short of the
            // quorum.
            if counter % PRECISION == 0 {
                info!(
                    "client_stats produced={} dispatched={} dropped={} replied={} reply_entries={} reply_abandoned={}",
                    produced.load(Ordering::Relaxed),
                    dispatched.load(Ordering::Relaxed),
                    dropped.load(Ordering::Relaxed),
                    replies.completed.load(Ordering::Relaxed),
                    replies.entries.load(Ordering::Relaxed),
                    replies.abandoned.load(Ordering::Relaxed),
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
