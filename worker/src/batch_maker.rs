#![allow(unused_variables)]
#![allow(unused_imports)]
// Copyright(C) Facebook, Inc. and its affiliates.
use crate::quorum_waiter::QuorumWaiterMessage;
use crate::worker::WorkerMessage;
use bytes::Bytes;
//#[cfg(feature = "benchmark")]
use crypto::Digest;
use crypto::PublicKey;
//#[cfg(feature = "benchmark")]
use ed25519_dalek::{Digest as _, Sha512};
use log::debug;
//#[cfg(feature = "benchmark")]
use log::info;
use network::{ReliableSender, SimpleSender};
use primary::{Primary, PrimaryWorkerMessage, WorkerPrimaryMessage};
use std::collections::{HashSet, VecDeque};
//#[cfg(feature = "benchmark")]

use std::net::SocketAddr;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{sleep, Duration, Instant};
use store::Store;
use std::convert::TryInto as _;

#[cfg(test)]
#[path = "tests/batch_maker_tests.rs"]
pub mod batch_maker_tests;

//The message type received by clients
pub type Transaction = Vec<u8>;
//The message type forwarded to quorum waiters
pub type Batch = Vec<Transaction>;

/// Assemble clients transactions into batches.
pub struct BatchMaker {
    /// The preferred batch size (in bytes).
    batch_size: usize,
    /// The maximum delay after which to seal the batch (in ms).
    max_batch_delay: u64,
    /// Channel to receive transactions from the network.
    rx_transaction: Receiver<Transaction>,
   
    //tx_message: Sender<QuorumWaiterMessage>,  /// Output channel to deliver sealed batches to the `QuorumWaiter`.
    tx_batch: Sender<Vec<u8>>,   // channel to forward batch digest to processor in order for primary to propose.

    /// The network addresses of the other workers that share our worker id.
    workers_addresses: Vec<(PublicKey, SocketAddr)>,
    /// Holds the current batch.
    current_batch: Batch,
    /// Holds the size of the current batch (in bytes).
    current_batch_size: usize,
    /// A network sender to broadcast the batches to the other workers.
    network: SimpleSender,
    // Receive async requests from primary core
    rx_async: Receiver<(bool, HashSet<PublicKey>)>,
    // Currently during asynchrony
    during_simulated_asynchrony: bool,
    // Partition public keys
    partition_public_keys: HashSet<PublicKey>,
    // Receive real async requests
    rx_async_real: Receiver<PrimaryWorkerMessage>,
    // Partition queue for batch requests
    partition_queue: VecDeque<WorkerMessage>,
    // Store
    store: Store,

}

impl BatchMaker {
    pub fn spawn(
        batch_size: usize,
        max_batch_delay: u64,
        rx_transaction: Receiver<Transaction>, //receiver channel from worker.TxReceiverHandler 
        //tx_message: Sender<QuorumWaiterMessage>, //sender channel to worker.QuorumWaiter
        tx_batch: Sender<Vec<u8>>,   // sender channel to worker.Processor
        workers_addresses: Vec<(PublicKey, SocketAddr)>,
        rx_async: Receiver<(bool, HashSet<PublicKey>)>,
        rx_async_real: Receiver<PrimaryWorkerMessage>,
        partition_public_keys: HashSet<PublicKey>,
        mut store: Store,
    ) {
        tokio::spawn(async move {
            Self {
                batch_size,
                max_batch_delay,
                rx_transaction,
                //tx_message, //previously forwarded batch to Quorum_waiter; now skipping this step.
                tx_batch,  
                workers_addresses,
                current_batch: Batch::with_capacity(batch_size * 2),
                current_batch_size: 0,
                network: SimpleSender::new(),
                rx_async,
                during_simulated_asynchrony: false,
                partition_public_keys,
                rx_async_real,
                partition_queue: VecDeque::new(),
                store,
            }
            .run()
            .await;
        });
    }

    /// Main loop receiving incoming transactions and creating batches.
    async fn run(&mut self) {
        let timer = sleep(Duration::from_millis(self.max_batch_delay));
        tokio::pin!(timer);
        let timer1 = sleep(Duration::from_secs(10));
        tokio::pin!(timer1);
        let timer2 = sleep(Duration::from_secs(20));
        tokio::pin!(timer2);
        let mut current_time = Instant::now();

        loop {
            tokio::select! {
                // Assemble client transactions into batches of preset size.
                Some(transaction) = self.rx_transaction.recv() => {
                    self.current_batch_size += transaction.len();
                    self.current_batch.push(transaction);
                    if self.current_batch_size >= self.batch_size {
                        self.seal().await;

                        debug!("batch ready it took {:?} ms", current_time.elapsed().as_millis());
                        current_time = Instant::now();

                        timer.as_mut().reset(Instant::now() + Duration::from_millis(self.max_batch_delay));
                    }
                },

                Some((during_simulated_asynchrony, partition_public_keys)) = self.rx_async.recv() => {
                    debug!("BatchMaker: received async request");
                    self.during_simulated_asynchrony = during_simulated_asynchrony;
                    self.partition_public_keys = partition_public_keys;
                },

                Some(async_request) = self.rx_async_real.recv() => {
                    debug!("BatchMaker: received real async request");
                    match async_request {
                        PrimaryWorkerMessage::Async(during_simulated_asynchrony, partition_public_keys) => {
                            self.during_simulated_asynchrony = during_simulated_asynchrony;
                            self.partition_public_keys = partition_public_keys;
                        },
                        _ => {},
                    }
                },

                // If the timer triggers, seal the batch even if it contains few transactions.
                () = &mut timer => {
                    debug!("BatchMaker: max batch delay timer triggered");
                    if !self.current_batch.is_empty() {
                        self.seal().await;
                    }

                    current_time = Instant::now();
                    timer.as_mut().reset(Instant::now() + Duration::from_millis(self.max_batch_delay));
                },

                // If the timer triggers, seal the batch even if it contains few transactions.
                () = &mut timer1 => {
                    debug!("BatchMaker: partition delay timer 1 triggered");
                    self.during_simulated_asynchrony = true;
                    timer1.as_mut().reset(Instant::now() + Duration::from_secs(100));
                },

                // If the timer triggers, seal the batch even if it contains few transactions.
                () = &mut timer2 => {
                    debug!("BatchMaker: partition delay timer 2 triggered");
                    debug!("partition queue size is {:?}", self.partition_queue.len());
                    self.during_simulated_asynchrony = false;
                    /*while !self.partition_queue.is_empty() {
                        let message = self.partition_queue.pop_front().unwrap();
                        let serialized = bincode::serialize(&message).expect("Failed to serialize our own batch");
                        let bytes = Bytes::from(serialized.clone());
                        let new_addresses: Vec<_> = self.workers_addresses.iter().filter(|(pk, _)| !self.partition_public_keys.contains(pk)).map(|(_, addr)| addr).cloned().collect();
                        //let (_, addresses) = new_addresses.iter().cloned().unzip();
                        debug!("addresses is {:?}", new_addresses);
                        self.partition_queue.push_back(message);
                        self.network.broadcast(new_addresses, bytes).await; 
                    }*/
                    timer2.as_mut().reset(Instant::now() + Duration::from_secs(100));
                },

            }

            // Give the change to schedule other tasks.
            tokio::task::yield_now().await;
        }
    }

    /// Seal and broadcast the current batch.
    async fn seal(&mut self) {
        #[cfg(feature = "benchmark")]
        let size = self.current_batch_size;

        // Look for sample txs (they all start with 0) and gather their txs id (the next 8 bytes).
        #[cfg(feature = "benchmark")]
        let tx_ids: Vec<_> = self
            .current_batch
            .iter()
            .filter(|tx| tx[0] == 0u8 && tx.len() > 8)
            .filter_map(|tx| tx[1..9].try_into().ok())
            .collect();

        // Serialize the batch.
        self.current_batch_size = 0;
        let batch: Vec<_> = self.current_batch.drain(..).collect();
        let message = WorkerMessage::Batch(batch);
        let serialized = bincode::serialize(&message).expect("Failed to serialize our own batch");

        #[cfg(feature = "benchmark")]
        {
            // NOTE: This is one extra hash that is only needed to print the following log entries.
            let digest = Digest(
                Sha512::digest(&serialized).as_slice()[..32]
                    .try_into()
                    .unwrap(),
            );

            for id in tx_ids {
                // NOTE: This log entry is used to compute performance.
                info!(
                    "Batch {:?} contains sample tx {}",
                    digest,
                    u64::from_be_bytes(id)
                );
            }

            // NOTE: This log entry is used to compute performance.
            info!("Batch {:?} contains {} B", digest, size);
        }

        // Broadcast the batch through the network.

        //NEW:
        //Best-effort broadcast only. Any failure is correlated with the primary operating this node (running on same machine)
        
        let bytes = Bytes::from(serialized.clone());
        let digest = Digest(Sha512::digest(&serialized).as_slice()[..32].try_into().unwrap());

        // Store the batch.
        self.store.write(digest.to_vec(), serialized.clone()).await;
        self.tx_batch.send(serialized).await.expect("Failed to deliver batch");
        if self.during_simulated_asynchrony {
            debug!("BatchMaker: Simulated asynchrony enabled. Only sending to partitioned keys from broadcast");
            let new_addresses: Vec<_> = self.workers_addresses.iter().filter(|(pk, _)| self.partition_public_keys.contains(pk)).map(|(_, addr)| addr).cloned().collect();
            //let (_, addresses) = new_addresses.iter().cloned().unzip();
            //debug!("addresses is {:?}", new_addresses);
            self.partition_queue.push_back(message);
            debug!("partition queue size is {:?}", self.partition_queue.len());
            self.network.broadcast(new_addresses, bytes).await; 
        } else {
            //debug!("sending batch normally");
            let (_, addresses): (Vec<_>, _) = self.workers_addresses.iter().cloned().unzip();
            self.network.broadcast(addresses, bytes).await; 
        }
        
        

        //OLD:
        //This uses reliable sender. The receiver worker will reply with an ack. The Reply Handler is passed to Quorum Waiter.
        // let (names, addresses): (Vec<_>, _) = self.workers_addresses.iter().cloned().unzip();
        // let bytes = Bytes::from(serialized.clone());
        // let handlers = self.network.broadcast(addresses, bytes).await; 

        // // Send the batch through the deliver channel for further processing.
        // self.tx_message
        //     .send(QuorumWaiterMessage {
        //         batch: serialized,
        //         handlers: names.into_iter().zip(handlers.into_iter()).collect(),
        //     })
        //     .await
        //     .expect("Failed to deliver batch");
    }
}
