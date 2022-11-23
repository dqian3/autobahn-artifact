use crate::consensus::{ConsensusMessage, CHANNEL_CAPACITY};
//use crate::error::ConsensusResult;
use primary::Header;
use primary::error::{ConsensusResult, ConsensusError};
use primary::messages::{Block, QC};
use bytes::Bytes;
use config::Committee;
use crypto::Hash as _;
use crypto::{Digest, PublicKey};
use futures::stream::futures_unordered::FuturesUnordered;
use futures::stream::StreamExt as _;
use log::{debug, error};
use network::SimpleSender;
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};
use store::Store;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::time::{sleep, Duration, Instant};

#[cfg(test)]
#[path = "tests/synchronizer_tests.rs"]
pub mod synchronizer_tests;

const TIMER_ACCURACY: u64 = 5_000;

pub struct Synchronizer {
    store: Store,
    inner_channel: Sender<(Header, Digest)>,
}

impl Synchronizer {
    pub fn new(
        name: PublicKey,
        committee: Committee,
        store: Store,
        tx_loopback: Sender<Header>,
        sync_retry_delay: u64,
    ) -> Self {
        let mut network = SimpleSender::new();
        let (tx_inner, mut rx_inner): (_, Receiver<(Header, Digest)>) = channel(CHANNEL_CAPACITY);

        let store_copy = store.clone();
        tokio::spawn(async move {
            let mut waiting = FuturesUnordered::new();
            let mut pending = HashSet::new();
            let mut requests = HashMap::new();

            let timer = sleep(Duration::from_millis(TIMER_ACCURACY));
            tokio::pin!(timer);
            loop {
                tokio::select! {
                    //TODO: Re-factor to headers.
                    Some((header, parent_dig)) = rx_inner.recv() => {
                        if pending.insert(header.digest()) {
                            let parent = parent_dig.clone();
                            let author = header.author;
                            let fut = Self::waiter(store_copy.clone(), parent.clone(), header);
                            waiting.push(fut);

                            if !requests.contains_key(&parent){
                                debug!("Requesting sync for header {}", parent);
                                let now = SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .expect("Failed to measure time")
                                    .as_millis();
                                requests.insert(parent.clone(), now);
                                let address = committee
                                    .consensus(&author)
                                    .expect("Author of valid header is not in the committee")
                                    .consensus_to_consensus;
                                let message = ConsensusMessage::SyncRequest(parent, name);
                                let message = bincode::serialize(&message)
                                    .expect("Failed to serialize sync request");
                                network.send(address, Bytes::from(message)).await;
                            }
                        }
                    },
                    // Some(block) = rx_inner.recv() => {
                    //     if pending.insert(block.digest()) {
                    //         let parent = block.parent().clone();
                    //         let author = block.author;
                    //         let fut = Self::waiter(store_copy.clone(), parent.clone(), block);
                    //         waiting.push(fut);

                    //         if !requests.contains_key(&parent){
                    //             debug!("Requesting sync for block {}", parent);
                    //             let now = SystemTime::now()
                    //                 .duration_since(UNIX_EPOCH)
                    //                 .expect("Failed to measure time")
                    //                 .as_millis();
                    //             requests.insert(parent.clone(), now);
                    //             let address = committee
                    //                 .consensus(&author)
                    //                 .expect("Author of valid block is not in the committee")
                    //                 .consensus_to_consensus;
                    //             let message = ConsensusMessage::SyncRequest(parent, name);
                    //             let message = bincode::serialize(&message)
                    //                 .expect("Failed to serialize sync request");
                    //             network.send(address, Bytes::from(message)).await;
                    //         }
                    //     }
                    // },
                    Some(result) = waiting.next() => match result {
                        Ok((header, parent)) => {
                            let _ = pending.remove(&header.digest());
                            let _ = requests.remove(&parent);
                            if let Err(e) = tx_loopback.send(header).await {
                                panic!("Failed to send message through core channel: {}", e);
                            }
                        },
                        Err(e) => error!("{}", e)
                    },
                    () = &mut timer => {
                        // This implements the 'perfect point to point link' abstraction.
                        for (digest, timestamp) in &requests {
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .expect("Failed to measure time")
                                .as_millis();
                            if timestamp + (sync_retry_delay as u128) < now {
                                debug!("Requesting sync for block {} (retry)", digest);
                                let addresses = committee
                                    .others_consensus(&name)
                                    .into_iter()
                                    .map(|(_, x)| x.consensus_to_consensus)
                                    .collect();
                                let message = ConsensusMessage::SyncRequest(digest.clone(), name);
                                let message = bincode::serialize(&message)
                                    .expect("Failed to serialize sync request");
                                network.broadcast(addresses, Bytes::from(message)).await;
                            }
                        }
                        timer.as_mut().reset(Instant::now() + Duration::from_millis(TIMER_ACCURACY));
                    }
                }
            }
        });
        Self {
            store,
            inner_channel: tx_inner,
        }
    }

    async fn waiter(mut store: Store, wait_on: Digest, deliver: Header) -> ConsensusResult<(Header, Digest)> {
        let _ = store.notify_read(wait_on.to_vec()).await?;
        Ok((deliver, wait_on))
    }

    // pub async fn get_parent_block(&mut self, block: &Block) -> ConsensusResult<Option<Block>> {
    //     if block.qc == QC::genesis() {
    //         return Ok(Some(Block::genesis()));
    //     }
    //     let parent = block.parent();
    //     match self.store.read(parent.to_vec()).await? {
    //         Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
    //         None => {
    //             if let Err(e) = self.inner_channel.send(block.clone()).await {
    //                 panic!("Failed to send request to synchronizer: {}", e);
    //             }
    //             Ok(None)
    //         }
    //     }
    // }

    pub async fn get_parent_header(&mut self, header: &Header) -> ConsensusResult<Option<Header>> {
        //TODO: Replace all this with dedicated consensus parent edge (digest of header ordered before) -> that can be a header ordered by the preceding Qc or Tc
        //use parent digest = Ticket Qc.hash
        //TODO: Need to find a similar mechanism for TC tickets. //For now this hack suffices.

        let ticket = header.ticket.clone().unwrap();
        let parent: Digest; //Header digest!
        match ticket.qc {
            Some(qc) => {
               // check if qc for prev view.
                parent = Header::default().digest(); //FIXME: THis is just a placeholder for compilation. ; Either QC and TC should hold hash of the header they commit; or that parent_header needs to be part of Header 
                                                                                                            //But to set it, would need QC or TC to contain header... ==> so just edit that.
            },
            None => {
                match ticket.tc {
                    Some(tc) => {
                       parent = Header::default().digest(); //FIXME: THis is just a placeholder for compilation. 
                    },
                    None => return Err(ConsensusError::InvalidTicket),
                }
            }

        }
            
        //TODO: Genesis cutoff    let parent = qc.hash
        // if block.qc == QC::genesis() {
        //     return Ok(Some(Block::genesis()));
        // }

        match self.store.read(parent.to_vec()).await? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => {
                if let Err(e) = self.inner_channel.send((header.clone(), parent)).await {
                    panic!("Failed to send request to synchronizer: {}", e);
                }
                Ok(None)
            }
        }
    }

    // pub async fn get_ancestors(
    //     &mut self,
    //     block: &Block,
    // ) -> ConsensusResult<Option<(Block, Block)>> {
    //     let b1 = match self.get_parent_block(block).await? {
    //         Some(b) => b,
    //         None => return Ok(None),
    //     };
    //     let b0 = self
    //         .get_parent_block(&b1)
    //         .await?
    //         .expect("We should have all ancestors of delivered blocks");
    //     Ok(Some((b0, b1)))
    // }
}
