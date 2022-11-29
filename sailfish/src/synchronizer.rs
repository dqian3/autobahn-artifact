use crate::consensus::{ConsensusMessage, CHANNEL_CAPACITY};
//use crate::error::ConsensusResult;
use primary::Header;
use primary::error::{ConsensusResult, ConsensusError};
use primary::messages::{QC, Certificate};
use bytes::Bytes;
use config::Committee;
use crypto::Hash as _;
use crypto::{Digest, PublicKey};
use ed25519_dalek::Digest as _;
use ed25519_dalek::Sha512;
use futures::stream::futures_unordered::FuturesUnordered;
use futures::stream::StreamExt as _;
use log::{debug, error};
use network::SimpleSender;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
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
    inner_channel_cert: Sender<Certificate>,
}

impl Synchronizer {
    pub fn new(
        name: PublicKey,
        committee: Committee,
        store: Store,
        tx_loopback_header: Sender<Header>,
        tx_loopback_certs: Sender<Certificate>,
        sync_retry_delay: u64,
    ) -> Self {
        let mut network = SimpleSender::new();
        let (tx_inner, mut rx_inner): (_, Receiver<(Header, Digest)>) = channel(CHANNEL_CAPACITY);
        let (tx_cert, mut rx_cert): (_, Receiver<Certificate>) = channel(CHANNEL_CAPACITY);

        let store_copy = store.clone();
        tokio::spawn(async move {

            let mut waiting_headers = FuturesUnordered::new();
            let mut waiting_certs = FuturesUnordered::new();

            let mut pending = HashSet::new();
            let mut requests = HashMap::new();
            //let mut cert_requests = HashMap::new();

            let timer = sleep(Duration::from_millis(TIMER_ACCURACY));
            tokio::pin!(timer);
            loop {
                tokio::select! {
                    
                    Some((header, parent_dig)) = rx_inner.recv() => {
                        if pending.insert(header.digest()) {
                            let parent = parent_dig.clone();
                            let author = header.author;
                            let fut = Self::header_waiter(store_copy.clone(), parent.clone(), header);
                            waiting_headers.push(fut);

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

                    Some(result) = waiting_headers.next() => match result {
                        Ok((header, parent)) => {
                            let _ = pending.remove(&header.digest());
                            let _ = requests.remove(&parent);
                            if let Err(e) = tx_loopback_header.send(header).await {
                                panic!("Failed to send message through core channel: {}", e);
                            }
                        },
                        Err(e) => error!("{}", e)
                    },

                    Some(cert) = rx_cert.recv() => {
                        if pending.insert(cert.digest()) {
                            let parent_cert_digest = cert.header.consensus_parent.clone().unwrap();
                            let fut = Self::cert_waiter(store_copy.clone(), parent_cert_digest.clone(), cert.clone());
                            waiting_certs.push(fut);

                            if !requests.contains_key(&parent_cert_digest){    
                                debug!("Requesting sync for certificate {}", parent_cert_digest);
                                let now = SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .expect("Failed to measure time")
                                    .as_millis();
                                requests.insert(parent_cert_digest.clone(), now);
                                let address = committee
                                    .consensus(&cert.header.author)
                                    .expect("Author of valid header is not in the committee")
                                    .consensus_to_consensus;
                                let message = ConsensusMessage::SyncRequestCert(parent_cert_digest, name);
                                let message = bincode::serialize(&message)
                                    .expect("Failed to serialize sync request");
                                network.send(address, Bytes::from(message)).await;
                            }
                        }
                    },
                    Some(result) = waiting_certs.next() => match result {
                        Ok((cert, parent)) => {
                            let _ = pending.remove(&cert.digest());
                            let _ = requests.remove(&parent);
                            if let Err(e) = tx_loopback_certs.send(cert).await {
                                panic!("Failed to send message through core channel: {}", e);
                            }
                        },
                        Err(e) => error!("{}", e)
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
            inner_channel_cert: tx_cert,
        }
    }

    async fn header_waiter(mut store: Store, wait_on: Digest, deliver: Header) -> ConsensusResult<(Header, Digest)> {
        let _ = store.notify_read(wait_on.to_vec()).await?;
        Ok((deliver, wait_on))
    }

    async fn cert_waiter(mut store: Store, wait_on: Digest, deliver: Certificate) -> ConsensusResult<(Certificate, Digest)> {
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

    pub async fn get_parent_cert(&mut self, cert: &Certificate) -> ConsensusResult<Option<Certificate>> {
        
        match self.store.read(cert.header.consensus_parent.as_ref().unwrap().to_vec()).await? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => {
                Ok(None)
            }
        }
    }

    //FIXME: 
    pub async fn get_parent_header(&mut self, header: &Header) -> ConsensusResult<Option<Header>> {
        //TODO: Replace all this with dedicated consensus parent edge (digest of header ordered before) -> that can be a header ordered by the preceding Qc or Tc
        //use parent digest = Ticket Qc.hash
        //TODO: Need to find a similar mechanism for TC tickets. //For now this hack suffices.
        let ticket = header.ticket.clone().unwrap();
        let parent: Digest = ticket.qc.clone().hash; //Header digest!

        // If QC not in previous view then there must be a TC
        if ticket.qc.clone().view + 1 != header.view && ticket.tc.is_none() {
            return Err(ConsensusError::InvalidTicket);
        }

        if ticket.qc == QC::genesis() {
            return Ok(None)
        }
            
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

    pub async fn get_special_parent_header(&mut self, header: &Header) -> ConsensusResult<Option<Header>> {
        let parent = header.special_parent.clone();
        let mut return_value = Ok(None);

        if parent.is_some() {
            match self.store.read(parent.clone().unwrap().to_vec()).await? {
                Some(bytes) => return_value = Ok(Some(bincode::deserialize(&bytes)?)),
                None  => {
                    if let Err(e) = self.inner_channel.send((header.clone(), parent.unwrap())).await {
                        panic!("Failed to send request to synchronizer: {}", e);
                    }
                }
            }
        }
        return_value
    }

    pub async fn get_cert(&mut self, header: &Header) -> ConsensusResult<Option<Certificate>> {

        // let cert_dig: Digest = Certificate {
        //     header: header.clone(),
        //     ..Certificate::default()
        // }.digest();

         //directly generating the hash avoids copying the header.
        let mut hasher = Sha512::new();
        hasher.update(&header.id); //== parent_header.id
        hasher.update(&header.round().to_le_bytes()); 
        hasher.update(&header.origin()); //parent_header.origin = child_header_origin
        let cert_digest = Digest(hasher.finalize().as_slice()[..32].try_into().unwrap());


        match self.store.read(cert_digest.to_vec()).await? {   
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None  => {
                panic!("Ancestor cert not available");
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

    pub async fn deliver_consensus_ancestor(&mut self, cert: &Certificate) -> ConsensusResult<bool>{

        if cert.header.special_parent_round == 0 {
            return Ok(true);
        }

        if self.store.read(cert.header.consensus_parent.as_ref().unwrap().to_vec()).await?.is_none(){
            if let Err(e) = self.inner_channel_cert.send(cert.clone()).await {
                panic!("Failed to send request to synchronizer: {}", e);
           }
           return Ok(false);
        }

        Ok(true)
    }

    pub async fn deliver_self(&mut self, cert: &Certificate) -> ConsensusResult<bool>{

        if self.store.read(cert.header.id.to_vec()).await?.is_none(){
        //     if let Err(e) = self.inner_channel_cert.send(cert.clone()).await {
        //         panic!("Failed to send request to synchronizer: {}", e);
        //    }
           return Ok(false);
        }

        Ok(true)
    }

   

    pub async fn get_commit_header(header_digest){
        //read digest
        //just send sync request
    }

    pub async fn deliver_parent_ticket(header, ticket_digest){
        //if ticket == genesis, return true.

        //read digest
        //start a waiter that restarts header

    }
}
