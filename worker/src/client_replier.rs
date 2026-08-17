//! Answers the clients whose requests just committed.
//!
//! See `client_reply` for the wire format, why the client's address rides
//! inside the transaction, and why the reply is a bare ack rather than a
//! signature. This is the replica half.
//!
//! A committed header names its batches by digest only, and the primary runs
//! in a separate process from the worker, so the primary forwards the
//! committed digests here (`PrimaryWorkerMessage::Committed`) and this task
//! reads the transactions back out of the worker's own store — the same
//! store, keyed the same way, that `Helper` already serves batches from.

use crate::batch_maker::Transaction;
use crate::client_reply::{decode_reply_addr, TAG_LEN};
use crate::worker::WorkerMessage;
use bytes::Bytes;
use config::Committee;
use crypto::{Digest, PublicKey};
use log::{debug, error, info, warn};
use network::SimpleSender;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use store::Store;
use tokio::sync::mpsc::Receiver;

/// Committed batch digests remembered so a redelivered commit does not make
/// us answer every one of its requests a second time — which would show up as
/// reply traffic the protocol never actually had to send.
const RECENT_BATCHES: usize = 100_000;

/// How often the reply count is logged.
const LOG_EVERY: u64 = 100_000;

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

impl ClientReplier {
    pub fn spawn(
        name: PublicKey,
        committee: Committee,
        store: Store,
        reply_count: usize,
        rx_committed: Receiver<(Vec<Digest>, PublicKey)>,
    ) {
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
    /// evenly; raising it draws a different set per batch, which keeps that
    /// spread.
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
                // clients that sent it get no reply; nothing else in the
                // protocol depends on this path.
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

        // One frame per client per batch. Requests are what we answer;
        // batches are what we send, so a reply per request does not become a
        // packet per request.
        let mut by_client: HashMap<SocketAddr, Vec<u8>> = HashMap::new();
        for transaction in &batch {
            let address = match decode_reply_addr(transaction) {
                Some(address) => address,
                None => continue,
            };
            by_client
                .entry(address)
                .or_insert_with(Vec::new)
                .extend_from_slice(&transaction[..TAG_LEN]);
        }

        for (address, bytes) in by_client {
            self.sent += (bytes.len() / TAG_LEN) as u64;
            self.network.send(address, Bytes::from(bytes)).await;
        }

        // NOTE: This log entry is used to compute performance.
        if self.sent >= self.next_log {
            info!("Sent {} client replies", self.sent);
            self.next_log = self.sent + LOG_EVERY;
        }
    }
}
