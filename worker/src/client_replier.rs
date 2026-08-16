//! Signs and sends one reply per committed client request.
//!
//! See `client_reply` for the wire format and why the client's address rides
//! inside the transaction. This is the replica half.
//!
//! A committed header names its batches by digest only, and the primary runs
//! in a separate process from the worker, so the primary forwards the
//! committed digests here (`PrimaryWorkerMessage::Committed`) and this task
//! reads the transactions back out of the worker's own store — the same
//! store, keyed the same way, that `Helper` already serves batches from.
//!
//! Signing is the point of the exercise, so it runs on the blocking pool in
//! chunks rather than through `SignatureService`, whose single task would cap
//! the whole node at one core's worth of signatures and turn a throughput
//! measurement into a measurement of that task.

use crate::batch_maker::Transaction;
use crate::client_reply::{
    decode_reply_addr, REPLY_ENTRY_LEN, SIG_LEN, TAG_LEN,
};
use crate::worker::WorkerMessage;
use bytes::Bytes;
use config::Committee;
use crypto::Hash as _;
use crypto::{Digest, PublicKey};
use ed25519_dalek as dalek;
use ed25519_dalek::Signer as _;
use log::{debug, error, info, warn};
use network::SimpleSender;
use std::cmp::min;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use store::Store;
use tokio::sync::mpsc::Receiver;

/// Transactions signed per blocking-pool task. Small enough that a batch of
/// a few hundred transactions spreads over several cores, large enough that
/// the per-task overhead stays well under the ~15 us an ed25519 signature
/// costs.
const SIGN_CHUNK: usize = 32;

/// Committed batch digests remembered so a redelivered commit does not make
/// us sign every one of its transactions a second time — which would show up
/// as reply-signing cost the protocol never actually had to pay.
const RECENT_BATCHES: usize = 100_000;

/// Replies to clients whose committed transactions this replica is on the
/// hook for.
pub struct ClientReplier {
    /// Our position in committee order, used to decide which batches we answer.
    our_index: usize,
    /// Committee order (the authorities map is sorted by public key).
    order: Vec<PublicKey>,
    /// How many replicas reply per batch.
    reply_count: usize,
    /// The persistent storage, holding batches keyed by digest.
    store: Store,
    /// This replica's signing key, shared with the blocking pool.
    keypair: Arc<dalek::Keypair>,
    /// Committed batch digests from our primary.
    rx_committed: Receiver<(Vec<Digest>, PublicKey)>,
    /// Network sender, one kept-alive connection per client.
    network: SimpleSender,
    /// Batches already answered, most recent last.
    answered: HashSet<Digest>,
    answered_order: VecDeque<Digest>,
    /// Replies sent, and the next count at which to log it.
    sent: u64,
    next_log: u64,
}

/// How often the reply count is logged.
const LOG_EVERY: u64 = 100_000;

impl ClientReplier {
    pub fn spawn(
        name: PublicKey,
        committee: Committee,
        store: Store,
        secret: [u8; 64],
        reply_count: usize,
        rx_committed: Receiver<(Vec<Digest>, PublicKey)>,
    ) {
        let keypair = dalek::Keypair::from_bytes(&secret)
            .expect("Failed to load our secret key for client replies");

        let order: Vec<PublicKey> = committee.authorities.keys().cloned().collect();
        let our_index = order
            .iter()
            .position(|key| key == &name)
            .expect("Our public key is not in the committee");

        tokio::spawn(async move {
            Self {
                our_index,
                order,
                reply_count,
                store,
                keypair: Arc::new(keypair),
                rx_committed,
                network: SimpleSender::new(),
                answered: HashSet::new(),
                answered_order: VecDeque::new(),
                sent: 0,
                next_log: LOG_EVERY,
            }
            .run()
            .await;
        });
    }

    /// Whether this replica replies for batches authored by `author`.
    ///
    /// The repliers are the `reply_count` replicas starting at the author, in
    /// committee order. With `reply_count = 1` that is the author itself, so
    /// each replica answers the clients it received from and the work spreads
    /// evenly; with `reply_count = f + 1` each batch draws a different f+1
    /// replicas, which keeps that spread.
    fn participates(&self, author: &PublicKey) -> bool {
        let n = self.order.len();
        if self.reply_count >= n {
            return true;
        }
        match self.order.iter().position(|key| key == author) {
            Some(author_index) => (self.our_index + n - author_index) % n < self.reply_count,
            None => false,
        }
    }

    /// Remember a batch as answered; returns false if it already was.
    fn mark_answered(&mut self, digest: &Digest) -> bool {
        if !self.answered.insert(digest.clone()) {
            return false;
        }
        self.answered_order.push_back(digest.clone());
        if self.answered_order.len() > RECENT_BATCHES {
            if let Some(old) = self.answered_order.pop_front() {
                self.answered.remove(&old);
            }
        }
        true
    }

    async fn run(&mut self) {
        while let Some((digests, author)) = self.rx_committed.recv().await {
            if !self.participates(&author) {
                continue;
            }
            for digest in digests {
                if !self.mark_answered(&digest) {
                    debug!("Already replied for batch {:?}", digest);
                    continue;
                }
                self.reply_for_batch(digest).await;
            }
        }
    }

    async fn reply_for_batch(&mut self, digest: Digest) {
        let serialized = match self.store.read(digest.to_vec()).await {
            Ok(Some(data)) => data,
            Ok(None) => {
                // We are on the hook for this batch but never stored it. The
                // clients that sent it will time out rather than get a reply;
                // nothing else in the protocol depends on this path.
                debug!("Cannot reply for batch {:?}: not in store", digest);
                return;
            }
            Err(e) => {
                error!("Cannot reply for batch {:?}: {}", digest, e);
                return;
            }
        };

        let batch: Vec<Transaction> = match bincode::deserialize(&serialized) {
            Ok(WorkerMessage::Batch(_, batch)) => batch,
            Ok(_) => return,
            Err(e) => {
                warn!("Cannot reply for batch {:?}: {}", digest, e);
                return;
            }
        };

        // Sign across the blocking pool: one signature per transaction is the
        // cost this whole path exists to pay, and it has to come off more than
        // one core to be a fair comparison.
        let batch = Arc::new(batch);
        let mut handles = Vec::new();
        for start in (0..batch.len()).step_by(SIGN_CHUNK) {
            let end = min(start + SIGN_CHUNK, batch.len());
            let batch = batch.clone();
            let keypair = self.keypair.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                sign_range(&keypair, &batch[start..end])
            }));
        }

        // One frame per client per batch. Requests are what we sign; batches
        // are what we send, so the network does not become the bottleneck
        // before the signing does.
        let mut by_client: HashMap<SocketAddr, Vec<u8>> = HashMap::new();
        for handle in handles {
            match handle.await {
                Ok(entries) => {
                    for (address, entry) in entries {
                        by_client
                            .entry(address)
                            .or_insert_with(Vec::new)
                            .extend_from_slice(&entry);
                    }
                }
                Err(e) => warn!("Reply signing task failed: {}", e),
            }
        }

        for (address, bytes) in by_client {
            self.sent += (bytes.len() / REPLY_ENTRY_LEN) as u64;
            self.network.send(address, Bytes::from(bytes)).await;
        }

        // NOTE: This log entry is used to compute performance.
        if self.sent >= self.next_log {
            info!("Sent {} client replies", self.sent);
            self.next_log = self.sent + LOG_EVERY;
        }
    }
}

/// Sign a slice of a batch, returning one addressed reply per transaction
/// whose sender asked for one.
fn sign_range(
    keypair: &dalek::Keypair,
    transactions: &[Transaction],
) -> Vec<(SocketAddr, [u8; REPLY_ENTRY_LEN])> {
    let skip_crypto = crypto::is_crypto_disabled();
    let mut out = Vec::with_capacity(transactions.len());

    for transaction in transactions {
        if transaction.len() < SIG_LEN + TAG_LEN {
            continue;
        }
        let address = match decode_reply_addr(transaction) {
            Some(address) => address,
            None => continue,
        };

        let mut entry = [0u8; REPLY_ENTRY_LEN];
        entry[..TAG_LEN].copy_from_slice(&transaction[..TAG_LEN]);
        if !skip_crypto {
            // Sign the request digest -- the same bytes the client signed,
            // which is what the client can check the reply against.
            let request = &transaction[..transaction.len() - SIG_LEN];
            let digest = request.digest();
            entry[TAG_LEN..].copy_from_slice(&keypair.sign(&digest.0).to_bytes());
        }
        out.push((address, entry));
    }

    out
}
