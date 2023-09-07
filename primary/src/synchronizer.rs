use crate::DagError;
// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::DagResult;
use crate::header_waiter::WaiterMessage;
use crate::messages::{Certificate, Header};
use config::Committee;
use crypto::Hash as _;
use crypto::{Digest, PublicKey};
use std::collections::HashMap;
use store::Store;
use tokio::sync::mpsc::Sender;

/// The `Synchronizer` checks if we have all batches and parents referenced by a header. If we don't, it sends
/// a command to the `Waiter` to request the missing data.
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

    pub async fn get_special_parent(&mut self, header: &Header, sync: bool) -> DagResult<(Option<Header>, bool)> {

        let digest = header.special_parent.as_ref().unwrap();//header.parents.iter().next().unwrap().clone();
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
    pub async fn get_parent(&mut self, header: &Header) -> DagResult<(Option<Header>, bool)> {

        /*let mut missing = Vec::new();
        let mut parents = Vec::new();*/

        let mut missing = None;
        let mut parent = None;

        //if *header == self.genesis_header { return Ok((parents, true))} //Just for unit testing...

        /*for digest in &header.parent_cert_digest {
            if let Some(genesis) = self
                .genesis
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
        }*/

        let parent_digest = &header.parent_cert.header_digest;

        if header.parent_cert.header_digest == self.genesis_header.digest() {
            println!("Parent is genesis");
            return Ok((parent, false));
        }

        /*if let Some(_genesis) = self
                .genesis
                .iter()
                .find(|(x, _)| x == parent_digest)
                .map(|(_, x)| x)
        {
            parent = Some(self.genesis_header.clone());
        }*/

        match self.store.read(parent_digest.to_vec()).await? {
            Some(parent_header) => parent = bincode::deserialize(&parent_header)?,
            None => missing = Some(parent_digest.clone()),
        };

        if missing.is_none() {
            return Ok((parent, false));
        }

        self.tx_header_waiter
            .send(WaiterMessage::SyncParent(missing.unwrap(), header.clone()))
            .await
            .expect("Failed to send sync parents request");
        Ok((None, true))
    }

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

        if self.genesis.iter().any(|(x, _)| x == &certificate.header_digest) {
            return Ok(true);
        }

        if self.store.read(certificate.header_digest.to_vec()).await?.is_none() {
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
    
    pub async fn deliver_special_certificate(&mut self, certificate: &Certificate, sync: bool) -> DagResult<(Option<Certificate>, bool)> { //bool value indicates that we have real_cert. No sync or error necessary

        let digest = certificate.header_digest.clone();//.as_ref().unwrap();//header.parents.iter().next().unwrap().clone();

        if digest == self.genesis_header.digest() {  //Note: In practice the genesis case should never be triggered (just used for unit testing). In normal processing the first special block would have genesis certs as parents.
            //parents.push(self.genesis_header.clone());
            //let dummy_parent_genesis_cert = Certificate::genesis_cert(&self.committee);
            //TODO: alternativey: pick first cert from genesis == genesis_cert
            let dummy_parent_cert = Certificate {
                header_digest: self.genesis_header.digest(),
                ..Certificate::default()
            };
            return Ok((Some(dummy_parent_cert), true));
        }
        else{
            match self.store.read(digest.to_vec()).await? {
                Some(special_parent_header) => {
                    //Create dummy cert for parent 
                    let dummy_parent_cert = Certificate {
                        header_digest: bincode::deserialize(&special_parent_header)?,
                        ..Certificate::default()
                    };

                    //Lookup whether real cert exists. Abusing the fact that dummy_cert and real_cert have the same digest (since votes and special_valids are not part of digest.)
                    match self.store.read(dummy_parent_cert.digest().to_vec()).await? {
                        Some(_) => {  //Some(real_cert)
                            //let real_special_parent_cert = bincode::deserialize(&real_cert)?;
                            return Ok((None, true));
                        },
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
            .send(certificate.clone())  //TODO: send message to header.author to request previous header digest.
            .await
            .expect("Failed to send sync special parent request");
        Ok((None, false))
    }
}
