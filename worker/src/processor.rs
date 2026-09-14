// Copyright(C) Facebook, Inc. and its affiliates.
use crate::worker::SerializedBatchDigestMessage;

use config::WorkerId;
use crypto::{Digest, PublicKey};
use ed25519_dalek::Digest as _;
use ed25519_dalek::Sha512;
use primary::WorkerPrimaryMessage;
use serde::Deserialize;
use tokio::sync::mpsc;
use std::convert::TryInto;
use std::ops::Range;
use std::sync::Arc;
use store::Store;
use tokio::sync::mpsc::{Receiver, Sender};
use log::debug;
use futures::future::join_all;
use futures::stream::{FuturesOrdered, StreamExt as _};

#[cfg(test)]
#[path = "tests/processor_tests.rs"]
pub mod processor_tests;

/// Indicates a serialized `WorkerMessage::Batch` message.
pub type SerializedBatchMessage = Vec<u8>;

/// Transactions per blocking verification task.
const VERIFY_CHUNK: usize = 128;

/// Batches from other workers whose signatures may be checked at once.
const MAX_BATCHES_IN_FLIGHT: usize = 16;

/// Borrowing view of a serialized `WorkerMessage` (same variants, same
/// order), so a batch's transactions can be located without copying them.
#[derive(Deserialize)]
enum WorkerMessageView<'a> {
    Batch(PublicKey, #[serde(borrow)] Vec<&'a [u8]>),
    #[allow(dead_code)]
    BatchRequest(Vec<Digest>, PublicKey),
}

/// Checks every client signature in a serialized batch from another worker,
/// in chunks on blocking threads. Returns the batch if all of them hold.
pub async fn verify_peer_batch(serialized: SerializedBatchMessage) -> Option<SerializedBatchMessage> {
    let serialized = Arc::new(serialized);
    let (author, ranges) = match bincode::deserialize::<WorkerMessageView>(&serialized) {
        Ok(WorkerMessageView::Batch(author, batch)) => {
            let base = serialized.as_ptr() as usize;
            let ranges: Vec<Range<usize>> = batch
                .iter()
                .map(|tx| {
                    let start = tx.as_ptr() as usize - base;
                    start..start + tx.len()
                })
                .collect();
            (author, ranges)
        }
        Ok(WorkerMessageView::BatchRequest(..)) => return None,
        Err(e) => {
            debug!("Failed to deserialize batch: {}", e);
            return None;
        }
    };

    let checks = ranges.chunks(VERIFY_CHUNK).map(|chunk| {
        let bytes = serialized.clone();
        let chunk = chunk.to_vec();
        tokio::task::spawn_blocking(move || {
            let txs: Vec<&[u8]> = chunk.into_iter().map(|range| &bytes[range]).collect();
            crypto::verify_transactions(&author, &txs)
        })
    });
    for result in join_all(checks).await {
        match result {
            Ok(passed) if passed.iter().all(|ok| *ok) => {}
            Ok(passed) => {
                // The batch is dropped: not stored and never sent to the primary.
                debug!(
                    "Dropping batch from {}: {} client signatures failed",
                    author,
                    passed.iter().filter(|ok| !**ok).count()
                );
                return None;
            }
            Err(e) => {
                debug!("A blocking task panicked or failed: {:?}", e);
                return None;
            }
        }
    }
    debug!("All client transactions verified");
    Some(Arc::try_unwrap(serialized).unwrap_or_else(|shared| (*shared).clone()))
}

/// Hashes and stores batches, it then outputs the batch's digest.
pub struct Processor;

impl Processor {
    pub fn spawn(
        // Our worker's id.
        id: WorkerId,
        // The persistent storage.
        mut store: Store,
        // Input channel to receive batches.
        mut rx_batch: Receiver<SerializedBatchMessage>,
        // Output channel to send out batches' digests.
        tx_digest: Sender<SerializedBatchDigestMessage>,    //sender channel connects to PrimaryConnector
        // Whether we are processing our own batches or the batches of other nodes.
        own_digest: bool,
    ) {
        let (tx_verified, mut rx_verified) = mpsc::channel(100);

        // Verifies several batches at once and forwards them in arrival order.
        tokio::spawn(async move {
            let mut pending = FuturesOrdered::new();
            loop {
                tokio::select! {
                    Some(serialized) = rx_batch.recv(), if pending.len() < MAX_BATCHES_IN_FLIGHT => {
                        // Own batches were checked by the batch maker. With
                        // crypto disabled the clients send zero signatures.
                        if own_digest || crypto::is_crypto_disabled() {
                            tx_verified
                                .send(serialized)
                                .await
                                .expect("Failed to send batch to be verified");
                        } else {
                            pending.push_back(verify_peer_batch(serialized));
                        }
                    },
                    Some(verified) = pending.next(), if !pending.is_empty() => {
                        if let Some(serialized) = verified {
                            tx_verified
                                .send(serialized)
                                .await
                                .expect("Failed to send batch to be verified");
                        }
                    },
                    else => break,
                }
            }
        });

        tokio::spawn(async move {
            while let Some(batch) = rx_verified.recv().await {
                let digest = Digest(Sha512::digest(&batch).as_slice()[..32].try_into().unwrap());
                debug!("Processor received batch {:?}", digest);

                // Store the batch.
                store.write(digest.to_vec(), batch).await;
                //store.write(digest.to_vec(), Vec::default()).await;

                // Deliver the batch's digest.
                let message = match own_digest {
                    true => WorkerPrimaryMessage::OurBatch(digest, id),
                    false => WorkerPrimaryMessage::OthersBatch(digest, id),
                };
                let message = bincode::serialize(&message)
                    .expect("Failed to serialize our own worker-primary message");
                tx_digest
                    .send(message)
                    .await
                    .expect("Failed to send digest");
            }
        });
    }
}
