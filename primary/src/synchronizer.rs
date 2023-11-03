use crate::{DagError, Height};
// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::DagResult;
use crate::header_waiter::WaiterMessage;
use crate::messages::{Certificate, ConsensusMessage, Header, Proposal};
use config::Committee;
use crypto::Hash as _;
use crypto::{Digest, PublicKey};
use std::collections::HashMap;
use store::Store;
use tokio::sync::mpsc::{Receiver, Sender};

/// The `Synchronizer` checks if we have all batches and parents referenced by a header. If we don't, it sends
/// a command to the `Waiter` to request the missing data.
#[derive(Clone)]
pub struct Synchronizer {
    /// The public key of this primary.
    name: PublicKey,
    /// The persistent storage.
    store: Store,
    /// Send commands to the `HeaderWaiter`.
    tx_header_waiter: Sender<WaiterMessage>,
    /// Send commands to the `CertificateWaiter`.
    tx_certificate_waiter: Sender<Certificate>,
    /// The genesis and its digests.
    genesis: Vec<(Digest, Certificate)>,
    /// Genesis header
    genesis_header: Header,
}

impl Synchronizer {
    pub fn new(
        name: PublicKey,
        committee: &Committee,
        store: Store,
        tx_header_waiter: Sender<WaiterMessage>,
        tx_certificate_waiter: Sender<Certificate>,
    ) -> Self {
        Self {
            name,
            store,
            tx_header_waiter,
            tx_certificate_waiter,
            genesis: Certificate::genesis(committee)
                .into_iter()
                .map(|x| (x.digest(), x))
                .collect(),
            genesis_header: Header::genesis(committee),
        }
    }

    /// Returns `true` if we have all transactions of the payload. If we don't, we return false,
    /// synchronize with other nodes (through our workers), and re-schedule processing of the
    /// header for when we will have its complete payload.
    pub async fn missing_payload(&mut self, header: &Header) -> DagResult<bool> {
        // We don't store the payload of our own workers.
        if header.author == self.name {
            return Ok(false);
        }

        let mut missing = HashMap::new();
        for (digest, worker_id) in header.payload.iter() {
            // Check whether we have the batch. If one of our worker has the batch, the primary stores the pair
            // (digest, worker_id) in its own storage. It is important to verify that we received the batch
            // from the correct worker id to prevent the following attack:
            //      1. A Bad node sends a batch X to 2f good nodes through their worker #0.
            //      2. The bad node proposes a malformed block containing the batch X and claiming it comes
            //         from worker #1.
            //      3. The 2f good nodes do not need to sync and thus don't notice that the header is malformed.
            //         The bad node together with the 2f good nodes thus certify a block containing the batch X.
            //      4. The last good node will never be able to sync as it will keep sending its sync requests
            //         to workers #1 (rather than workers #0). Also, clients will never be able to retrieve batch
            //         X as they will be querying worker #1.
            let key = [digest.as_ref(), &worker_id.to_le_bytes()].concat();
            if self.store.read(key).await?.is_none() {
                missing.insert(digest.clone(), *worker_id);
            }
        }

        if missing.is_empty() {
            return Ok(false);
        }

        self.tx_header_waiter
            .send(WaiterMessage::SyncBatches(missing, header.clone()))
            .await
            .expect("Failed to send sync batch request");
        Ok(true)
    }

    pub async fn fetch_header(&mut self, header_digest: Digest) -> DagResult<()> {
        self.tx_header_waiter
            .send(WaiterMessage::SyncHeader(header_digest))
            .await
            .expect("Failed to send sync special parent request");
        Ok(())
    }

    pub async fn get_special_parent(
        &mut self,
        header: &Header,
        sync: bool,
    ) -> DagResult<(Option<Header>, bool)> {
        let digest = header.special_parent.as_ref().unwrap(); //header.parents.iter().next().unwrap().clone();
                                                              /*if digest == &self.genesis_header.id {  //Note: In practice the genesis case should never be triggered (just used for unit testing). In normal processing the first special block would have genesis certs as parents.
                                                                  //parents.push(self.genesis_header.clone());
                                                                  return Ok((Some(self.genesis_header.clone()), true))
                                                              }
                                                              else{
                                                                  match self.store.read(digest.to_vec()).await? {
                                                                      Some(parent_header) => return Ok((Some(bincode::deserialize(&parent_header)?), false)),
                                                                      None => {},
                                                                  };
                                                              }

                                                              if !sync {
                                                                  return Err(DagError::InvalidSpecialParent);
                                                              }

                                                              //if we dont have special parent: ignore request --> should be there when using FIFO channels ==> TCP is FIFO, and individual channels are FIFO (e.g. channel for receiving new header proposal)

                                                              //TODO: Start a waiter for it. FIXME: currently SyncParents requests certs. But we just want to request a header.

                                                              self.tx_header_waiter
                                                                  .send(WaiterMessage::SyncSpecialParent(digest.clone(), header.clone()))  //sends message to header.author to request previous header digest.
                                                                  .await
                                                                  .expect("Failed to send sync special parent request");*/
        Ok((None, false))
    }

    /// Returns the parents of a header if we have them all. If at least one parent is missing,
    /// we return an empty vector, synchronize with other nodes, and re-schedule processing
    /// of the header for when we will have all the parents.
    /*pub async fn get_proposal_headers(&mut self, header_info: &Info) -> DagResult<Vec<Header>> {
        let mut missing: Vec<Digest> = Vec::new();
        let mut parents: Vec<Header> = Vec::new();

        let prepare_info = match header_info.prepare_info {
            Some(prepare) => prepare,
            None => return Ok(Vec::new()),
        };

        for (pk, cert) in &prepare_info.proposals {
            let digest = &cert.header_digest;
            if let Some(genesis) = self
                .genesis_headers
                .iter()
                .find(|(x, _)| x == digest)
                .map(|(_, x)| x)
            {
                parents.push(genesis.clone());
                continue;
            }

            match self.store.read(digest.to_vec()).await? {
                Some(certificate) => parents.push(bincode::deserialize(&certificate)?),
                None => missing.push(digest.clone()),
            };
        }

        if missing.is_empty() {
            return Ok(parents);
        }

        self.tx_header_waiter
            .send(WaiterMessage::SyncProposals(missing, header_info.clone()))
            .await
            .expect("Failed to send sync parents request");
        Ok(Vec::new())
    }*/

    /// Returns the parents of a header if we have them all. If at least one parent is missing,
    /// we return an empty vector, synchronize with other nodes, and re-schedule processing
    /// of the header for when we will have all the parents.
    /*pub async fn get_info(&mut self, info_digest: &Digest) -> DagResult<Option<Info>> {
        match self.store.read(info_digest.to_vec()).await? {
            Some(info) => Ok(Some(bincode::deserialize(&info)?)),
            None => {
                self.tx_header_waiter
                    .send(WaiterMessage::SyncInfo(info_digest.clone()))
                    .await
                    .expect("Failed to send sync info request");
                Ok(None)
            },
        }
    }*/

    /// Check whether we have all the ancestors of the certificate. If we don't, send the certificate to
    /// the `CertificateWaiter` which will trigger re-processing once we have all the missing data.
    pub async fn deliver_certificate(&mut self, certificate: &Certificate) -> DagResult<bool> {
        /*for digest in &certificate.header.parent_cert_digest {
            if self.genesis.iter().any(|(x, _)| x == digest) {
                continue;
            }

            if self.store.read(digest.to_vec()).await?.is_none() {
                self.tx_certificate_waiter
                    .send(certificate.clone())
                    .await
                    .expect("Failed to send sync certificate request");
                return Ok(false);
            };
        }*/

        if self
            .genesis
            .iter()
            .any(|(x, _)| x == &certificate.header_digest)
        {
            return Ok(true);
        }

        if self
            .store
            .read(certificate.header_digest.to_vec())
            .await?
            .is_none()
        {
            self.tx_certificate_waiter
                .send(certificate.clone())
                .await
                .expect("Failed to send sync certificate request");
            return Ok(false);
        };

        Ok(true)
    }

    //Checks whether we have special parent cert or header (based on header.id)
    // If we have special parent cert ==> do nothing.
    // If we have only special parent header ==> send dummy cert to committer
    // If we have neither, return error or sync.

    pub async fn deliver_special_certificate(
        &mut self,
        certificate: &Certificate,
        sync: bool,
    ) -> DagResult<(Option<Certificate>, bool)> {
        //bool value indicates that we have real_cert. No sync or error necessary

        let digest = certificate.header_digest.clone(); //.as_ref().unwrap();//header.parents.iter().next().unwrap().clone();

        if digest == self.genesis_header.digest() {
            //Note: In practice the genesis case should never be triggered (just used for unit testing). In normal processing the first special block would have genesis certs as parents.
            //parents.push(self.genesis_header.clone());
            //let dummy_parent_genesis_cert = Certificate::genesis_cert(&self.committee);
            //TODO: alternativey: pick first cert from genesis == genesis_cert
            let dummy_parent_cert = Certificate {
                header_digest: self.genesis_header.digest(),
                ..Certificate::default()
            };
            return Ok((Some(dummy_parent_cert), true));
        } else {
            match self.store.read(digest.to_vec()).await? {
                Some(special_parent_header) => {
                    //Create dummy cert for parent
                    let dummy_parent_cert = Certificate {
                        header_digest: bincode::deserialize(&special_parent_header)?,
                        ..Certificate::default()
                    };

                    //Lookup whether real cert exists. Abusing the fact that dummy_cert and real_cert have the same digest (since votes and special_valids are not part of digest.)
                    match self.store.read(dummy_parent_cert.digest().to_vec()).await? {
                        Some(_) => {
                            //Some(real_cert)
                            //let real_special_parent_cert = bincode::deserialize(&real_cert)?;
                            return Ok((None, true));
                        }
                        None => {
                            return Ok((Some(dummy_parent_cert), true));
                        }
                    }
                }

                None => {}
            };
        }

        if !sync {
            return Err(DagError::InvalidSpecialParent);
            //return Ok((None, false));
        }

        //if we dont have special parent: ignore request --> should be there when using FIFO channels ==> TCP is FIFO, and individual channels are FIFO (e.g. channel for receiving new header proposal)

        //TODO: Start a waiter for it. FIXME: currently SyncParents requests certs. But we just want to request a header. ==> certificate waiter needs to be modified to check special parent too.

        self.tx_certificate_waiter
            .send(certificate.clone()) //TODO: send message to header.author to request previous header digest.
            .await
            .expect("Failed to send sync special parent request");
        Ok((None, false))
    }

    pub async fn is_proposal_ready(&mut self, proposal: &Proposal) -> DagResult<bool> {
        Ok(self.store.read(proposal.header_digest.to_vec()).await?.is_some())
    }

    pub async fn start_proposal_sync(&mut self, proposal: Proposal, author: &PublicKey, consensus_message: ConsensusMessage) {
        let is_ready = self.is_proposal_ready(&proposal).await.expect("should return true or false");
        if !is_ready {
            if let Err(e) = self
                .tx_header_waiter
                .send(WaiterMessage::SyncProposalHeaders(
                    proposal,
                    *author,
                    consensus_message,
                ))
                .await
            {
                panic!("Failed to send request to header waiter: {}", e);
            }
        }
    }

    pub async fn get_all_headers_for_proposal(
        &mut self,
        proposal: Proposal,
        stop_height: Height,
    ) -> DagResult<Vec<Header>> {
        // The list of blocks for this proposal
        let mut ancestors: Vec<Header> = Vec::new();

        // NOTE: Before calling, must check if proposal is ready, assumes that proposal is ready
        // before calling
        ensure!(self.is_proposal_ready(&proposal).await.unwrap(), DagError::InvalidHeaderId);
        let mut header: Header = self.get_header(proposal.header_digest).await.expect("already synced should have header").unwrap();

        // Otherwise we have the header and all of its ancestors
        let mut current_height = proposal.height;
        while current_height < stop_height {
            ancestors.push(header.clone());
            header = self.get_parent_header(&header).await?.expect("should have parent by now");
            current_height = header.height();
        }

        Ok(ancestors)
    }

    pub async fn get_parent_header(&mut self, header: &Header) -> DagResult<Option<Header>> {
        if header.parent_cert.header_digest == self.genesis_header.digest() {
            return Ok(Some(self.genesis_header.clone()));
        }

        let parent = header.parent_cert.header_digest.clone();
        match self.store.read(parent.to_vec()).await? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => {
                Ok(None)
            }
        }
    }


    pub async fn get_header(&mut self, header_digest: Digest) -> DagResult<Option<Header>> {
        if header_digest == self.genesis_header.digest() {
            return Ok(Some(self.genesis_header.clone()));
        }

        match self.store.read(header_digest.to_vec()).await? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => {
                Ok(None)
            }
        }
    }

}
