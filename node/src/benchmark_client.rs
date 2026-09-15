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
use std::collections::hash_map::Entry;
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
use crypto::PublicKey;
use crypto::set_crypto_disabled;

use config::Committee;
use config::KeyPair;
use config::Import as _;

use worker::client_reply::{
    encode_reply_addr, verify_reply_entries, KIND_SAMPLE, MAX_REPLIERS, REPLY_ADDR_LEN,
    REPLY_ENTRY_LEN, REPLY_HEADER_LEN, SIGNED_REPLY_ENTRY_LEN, TAG_LEN,
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

/// Most frames read off one connection and verified together.
const VERIFY_GROUP: usize = 64;

/// Most signed reply entries batch-checked in one blocking task.
const VERIFY_ENTRIES: usize = 256;

/// One transaction still expecting replies.
struct Pending {
    /// A bit per replica that has answered.
    repliers: u64,
    /// Whether it has already reached the quorum and been counted.
    done: bool,
    /// When its first reply arrived.
    first: Instant,
}

/// Counts distinct repliers per transaction. A transaction completes once,
/// on its `quorum`-th distinct replier, and its entry stays until all `count`
/// repliers have answered or PARTIAL_REPLY_TIMEOUT passes, so late replies do
/// not complete it again. Built on the reply side only, so the send path
/// stays untouched.
struct ReplyTracker {
    /// Distinct replicas that must reply before a transaction counts.
    quorum: usize,
    /// Distinct replicas that answer each transaction; at least `quorum`.
    count: usize,
    pending: HashMap<[u8; TAG_LEN], Pending>,
}

impl ReplyTracker {
    fn new(quorum: usize, count: usize) -> Self {
        let quorum = quorum.clamp(1, MAX_REPLIERS);
        Self {
            quorum,
            count: count.clamp(quorum, MAX_REPLIERS),
            pending: HashMap::new(),
        }
    }

    /// Record `bit`'s reply to `tag`; true if this reply completes it.
    fn record(&mut self, tag: [u8; TAG_LEN], bit: u64, now: Instant) -> bool {
        if self.count <= 1 {
            return true;
        }
        match self.pending.entry(tag) {
            Entry::Occupied(mut slot) => {
                let pending = slot.get_mut();
                if pending.repliers & bit != 0 {
                    return false;
                }
                pending.repliers |= bit;
                let seen = pending.repliers.count_ones() as usize;
                let complete = !pending.done && seen >= self.quorum;
                pending.done |= complete;
                if seen >= self.count {
                    slot.remove();
                }
                complete
            }
            Entry::Vacant(slot) => {
                let complete = self.quorum <= 1;
                slot.insert(Pending { repliers: bit, done: complete, first: now });
                complete
            }
        }
    }

    /// Drop entries older than PARTIAL_REPLY_TIMEOUT; returns how many of
    /// them had not reached the quorum.
    fn sweep(&mut self, now: Instant) -> u64 {
        let mut abandoned = 0;
        self.pending.retain(|_, pending| {
            let keep = now.saturating_duration_since(pending.first) < PARTIAL_REPLY_TIMEOUT;
            if !keep && !pending.done {
                abandoned += 1;
            }
            keep
        });
        abandoned
    }
}

/// Reply-side state shared by every replica's connection.
struct Replies {
    /// Replica public keys in committee order, when replies are signed and
    /// must verify before they count.
    verify_keys: Option<Arc<Vec<PublicKey>>>,
    samples: SampleTimes,
    tracker: Mutex<ReplyTracker>,
    /// Transactions that reached the quorum, each counted once.
    completed: AtomicU64,
    /// Reply entries received, from every replier.
    entries: AtomicU64,
    /// Transactions dropped short of the quorum after PARTIAL_REPLY_TIMEOUT.
    abandoned: AtomicU64,
    /// Signed reply entries that did not verify, and so did not count.
    bad_sig: AtomicU64,
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
        .args_from_usage("--reply-quorum=[INT] 'Distinct replicas that must reply before a transaction counts as committed (default 1).'")
        .args_from_usage("--reply-count=[INT] 'Distinct replicas that answer each transaction, the replicas client_reply_count capped at n (default: the quorum).'")
        .args_from_usage("--committee=[FILE] 'The committee file the nodes run with; supplies replica public keys for --verify-replies.'")
        .args_from_usage("--verify-replies 'Expect a signature on every reply (client_reply_signed) and count only replies that verify against the replier key; needs --committee.'")
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
    let reply_count = matches
        .value_of("reply-count")
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("The reply count must be a positive integer")?
        .unwrap_or(reply_quorum)
        .max(reply_quorum)
        .min(MAX_REPLIERS);

    let verify_keys = if matches.is_present("verify-replies") {
        let committee_file = matches
            .value_of("committee")
            .ok_or_else(|| anyhow::Error::msg("--verify-replies needs --committee"))?;
        let committee = Committee::import(committee_file)
            .context("Failed to load the committee information")?;
        // Committee order: the index each replier puts at the head of a frame.
        let keys: Vec<PublicKey> = committee.authorities.keys().cloned().collect();
        Some(Arc::new(keys))
    } else {
        None
    };

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
    match &verify_keys {
        Some(keys) => info!("Verifying signed replies against {} replica keys", keys.len()),
        None => info!("Verifying signed replies? false"),
    }

    let mut client = Client {
        target,
        size,
        rate,
        nodes,
        signer,
        reply_addr,
        reply_bytes,
        reply_quorum,
        reply_count,
        verify_keys,
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
    /// Distinct replicas that answer each transaction.
    reply_count: usize,
    /// Replica keys in committee order, when replies must verify.
    verify_keys: Option<Arc<Vec<PublicKey>>>,
}

/// Read replies off one replica's connection for as long as it stays open.
///
/// Replies arrive in batches — one frame per committed batch per client — led
/// by the replier's committee index and followed by fixed-width entries, each
/// the request's own first 9 bytes echoed back so it can be matched. Unsigned
/// by default; with `verify_keys` each tag is followed by the replier's
/// signature, which is checked before the tag counts (see
/// `worker::client_reply`).
///
/// Frames already buffered are taken up to VERIFY_GROUP at a time and
/// verified together rather than one task per frame.
async fn read_replies(stream: TcpStream, replies: Arc<Replies>) {
    let mut transport =
        Framed::new(stream, LengthDelimitedCodec::new()).ready_chunks(VERIFY_GROUP);
    while let Some(chunk) = transport.next().await {
        let mut frames = Vec::with_capacity(chunk.len());
        let mut closed = None;
        for frame in chunk {
            match frame {
                Ok(frame) => frames.push(frame),
                Err(e) => {
                    closed = Some(e);
                    break;
                }
            }
        }

        match &replies.verify_keys {
            Some(keys) => {
                for frame in verify_entries(frames, keys.clone(), &replies).await {
                    record_frame(&replies, &frame);
                }
            }
            None => {
                for frame in &frames {
                    record_frame(&replies, frame);
                }
            }
        }

        if let Some(e) = closed {
            warn!("Reply connection closed: {}", e);
            return;
        }
    }
}

/// Keep the signed reply entries that verify against their replier's committee
/// key, counting the rest in `bad_sig`. Returns one frame per input frame: its
/// replier index followed by the tags that verified.
///
/// Each replier's entries are batch-checked VERIFY_ENTRIES at a time on
/// parallel blocking threads. A trailing partial entry counts as one bad
/// entry, and so does every entry from an unknown replier index.
async fn verify_entries(
    frames: Vec<BytesMut>,
    keys: Arc<Vec<PublicKey>>,
    replies: &Replies,
) -> Vec<Vec<u8>> {
    let mut bad = 0u64;
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(frames.len());
    // Per replier: the output frame of each entry, and the entries themselves.
    let mut by_replier: HashMap<u8, (Vec<usize>, Vec<u8>)> = HashMap::new();
    for (i, frame) in frames.iter().enumerate() {
        if frame.len() < REPLY_HEADER_LEN {
            out.push(Vec::new());
            continue;
        }
        let replier = frame[0];
        out.push(vec![replier]);
        let body = &frame[REPLY_HEADER_LEN..];
        let whole = body.len() / SIGNED_REPLY_ENTRY_LEN;
        if body.len() % SIGNED_REPLY_ENTRY_LEN != 0 {
            bad += 1;
        }
        if keys.get(replier as usize).is_none() {
            bad += whole as u64;
            continue;
        }
        let (owners, entries) = by_replier.entry(replier).or_default();
        owners.extend(std::iter::repeat(i).take(whole));
        entries.extend_from_slice(&body[..whole * SIGNED_REPLY_ENTRY_LEN]);
    }

    let mut tasks = Vec::new();
    for (replier, (owners, entries)) in by_replier {
        let key = keys[replier as usize];
        let groups = owners
            .chunks(VERIFY_ENTRIES)
            .zip(entries.chunks(VERIFY_ENTRIES * SIGNED_REPLY_ENTRY_LEN));
        for (owners, entries) in groups {
            let owners = owners.to_vec();
            let entries = entries.to_vec();
            tasks.push(tokio::task::spawn_blocking(move || {
                let group: Vec<&[u8]> = entries.chunks_exact(SIGNED_REPLY_ENTRY_LEN).collect();
                let good = verify_reply_entries(&key, replier, &group);
                (owners, entries, good)
            }));
        }
    }

    for result in join_all(tasks).await {
        let (owners, entries, good) = match result {
            Ok(checked) => checked,
            Err(e) => {
                warn!("Reply verification task failed: {}", e);
                continue;
            }
        };
        let checked = owners
            .iter()
            .zip(entries.chunks_exact(SIGNED_REPLY_ENTRY_LEN))
            .zip(good);
        for ((&owner, entry), ok) in checked {
            if ok {
                out[owner].extend_from_slice(&entry[..TAG_LEN]);
            } else {
                bad += 1;
            }
        }
    }
    replies.bad_sig.fetch_add(bad, Ordering::Relaxed);
    out
}

/// Count one frame's entries toward their transactions' quorums.
///
/// `frame` is the replier index followed by tags, with any signatures already
/// stripped. See `ReplyTracker` for when a transaction completes.
fn record_frame(replies: &Replies, frame: &[u8]) {
    if frame.len() < REPLY_HEADER_LEN {
        return;
    }
    let bit = 1u64 << (frame[0] as usize % MAX_REPLIERS);
    let entries = &frame[REPLY_HEADER_LEN..];

    let mut completed = 0u64;
    let mut sampled = Vec::new();
    {
        let now = Instant::now();
        let mut tracker = replies.tracker.lock().unwrap();
        for entry in entries.chunks_exact(REPLY_ENTRY_LEN) {
            let tag: [u8; TAG_LEN] = entry.try_into().expect("entry is one tag");
            if !tracker.record(tag, bit, now) {
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
        return;
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

/// Drop transactions still waiting on replies after PARTIAL_REPLY_TIMEOUT,
/// so a lost reply does not hold memory for the run.
async fn sweep_partial_replies(replies: Arc<Replies>) {
    let mut ticker = interval(Duration::from_secs(10));
    loop {
        ticker.tick().await;
        let abandoned = replies.tracker.lock().unwrap().sweep(Instant::now());
        replies.abandoned.fetch_add(abandoned, Ordering::Relaxed);
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
            verify_keys: self.verify_keys.clone(),
            samples: Arc::new(Mutex::new(HashMap::new())),
            tracker: Mutex::new(ReplyTracker::new(self.reply_quorum, self.reply_count)),
            completed: AtomicU64::new(0),
            entries: AtomicU64::new(0),
            abandoned: AtomicU64::new(0),
            bad_sig: AtomicU64::new(0),
        });
        if let Some(address) = self.reply_addr {
            // NOTE: This log entry is used to compute performance.
            info!("Reply quorum: {} distinct replicas", self.reply_quorum);
            info!("Reply count: {} replicas answer each transaction", self.reply_count);
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
            if self.reply_count > 1 {
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
            // distinct replicas, once each. `reply_entries` counts every
            // verified reply received, including those past the quorum,
            // and `reply_abandoned` the transactions given up on short of the
            // quorum. `reply_bad_sig` counts signed reply entries that failed
            // verification; they are in none of the other counts.
            if counter % PRECISION == 0 {
                info!(
                    "client_stats produced={} dispatched={} dropped={} replied={} reply_entries={} reply_abandoned={} reply_bad_sig={}",
                    produced.load(Ordering::Relaxed),
                    dispatched.load(Ordering::Relaxed),
                    dropped.load(Ordering::Relaxed),
                    replies.completed.load(Ordering::Relaxed),
                    replies.entries.load(Ordering::Relaxed),
                    replies.abandoned.load(Ordering::Relaxed),
                    replies.bad_sig.load(Ordering::Relaxed),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(n: u8) -> [u8; TAG_LEN] {
        [n; TAG_LEN]
    }

    #[test]
    fn quorum_of_six_completes_once() {
        let mut tracker = ReplyTracker::new(2, 6);
        let now = Instant::now();
        let completions: Vec<bool> =
            (0..6).map(|r| tracker.record(tag(1), 1 << r, now)).collect();
        assert_eq!(completions, [false, true, false, false, false, false]);
        assert!(tracker.pending.is_empty());
    }

    #[test]
    fn duplicate_replier_ignored() {
        let mut tracker = ReplyTracker::new(2, 6);
        let now = Instant::now();
        assert!(!tracker.record(tag(1), 1, now));
        assert!(!tracker.record(tag(1), 1, now));
        assert!(tracker.record(tag(1), 2, now));
        assert!(!tracker.record(tag(1), 2, now));
    }

    #[test]
    fn timed_out_incomplete_is_abandoned() {
        let mut tracker = ReplyTracker::new(2, 6);
        let now = Instant::now();
        tracker.record(tag(1), 1, now);
        assert_eq!(tracker.sweep(now), 0);
        assert_eq!(tracker.sweep(now + PARTIAL_REPLY_TIMEOUT), 1);
        assert!(tracker.pending.is_empty());
    }

    #[test]
    fn timed_out_done_is_not_abandoned() {
        let mut tracker = ReplyTracker::new(2, 6);
        let now = Instant::now();
        tracker.record(tag(1), 1, now);
        assert!(tracker.record(tag(1), 2, now));
        assert_eq!(tracker.sweep(now + PARTIAL_REPLY_TIMEOUT), 0);
        assert!(tracker.pending.is_empty());
    }

    #[test]
    fn quorum_one_of_many_completes_on_first() {
        let mut tracker = ReplyTracker::new(1, 3);
        let now = Instant::now();
        assert!(tracker.record(tag(1), 1, now));
        assert!(!tracker.record(tag(1), 2, now));
        assert!(!tracker.record(tag(1), 4, now));
        assert!(tracker.pending.is_empty());
    }
}
