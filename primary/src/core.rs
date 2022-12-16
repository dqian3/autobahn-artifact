// Copyright(C) Facebook, Inc. and its affiliates.
use crate::aggregators::{CertificatesAggregator, VotesAggregator};
//use crate::common::special_header;
use crate::error::{DagError, DagResult};
use crate::messages::{Certificate, Header, Vote, QC, TC, matching_valids};
use crate::primary::{PrimaryMessage, Round};
use crate::synchronizer::Synchronizer;
use async_recursion::async_recursion;
use bytes::Bytes;
use config::Committee;
use crypto::Hash as _;
use crypto::{Digest, PublicKey, SignatureService};
use log::{debug, error, warn};
use network::{CancelHandler, ReliableSender};
//use tokio::time::error::Elapsed;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
//use std::task::Poll;
use store::Store;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{sleep, Duration, Instant};


//use crate::messages_consensus::{QC, TC};

#[cfg(test)]
#[path = "tests/core_tests.rs"]
pub mod core_tests;

pub struct Core {
    /// The public key of this primary.
    name: PublicKey,
    /// The committee information.
    committee: Committee,
    /// The persistent storage.
    store: Store,
    /// Handles synchronization with other nodes and our workers.
    synchronizer: Synchronizer,
    /// Service to sign headers.
    signature_service: SignatureService,
    /// The current consensus round (used for cleanup).
    consensus_round: Arc<AtomicU64>,
    /// The depth of the garbage collector.
    gc_depth: Round,

    /// Receiver for dag messages (headers, votes, certificates).
    rx_primaries: Receiver<PrimaryMessage>,
    /// Receives loopback headers from the `HeaderWaiter`.
    rx_header_waiter: Receiver<Header>,
    /// Receives loopback certificates from the `CertificateWaiter`.
    rx_certificate_waiter: Receiver<Certificate>,
    /// Receives our newly created headers from the `Proposer`.
    rx_proposer: Receiver<Header>,
    /// Output special certificates to the consensus layer.
    tx_consensus: Sender<Certificate>,
    // Output all certificates to the consensus Dag view
    tx_committer: Sender<Certificate>,

    /// Send valid a quorum of certificates' ids to the `Proposer` (along with their round).
    tx_proposer: Sender<(Vec<Digest>, Round)>,
    // Receives validated special Headers & proof from the consensus layer.
    rx_validation: Receiver<(Header, u8, Option<QC>, Option<TC>)>,
    tx_special: Sender<Header>,

    rx_pushdown_cert: Receiver<Certificate>,
    // Receive sync requests for headers required at the consensus layer
    rx_request_header_sync: Receiver<Digest>, 

    /// The last garbage collected round.
    gc_round: Round,

    /// The authors of the last voted headers. (Ensures only voting for one header per round)
    last_voted: HashMap<Round, HashSet<PublicKey>>,
    // /// The set of headers we are currently processing.
    processing: HashMap<Round, HashSet<Digest>>, //NOTE: Keep processing separate from current_headers ==> to allow us to process multiple headers from same replica (e.g. in case we first got a header that isnt the one that creates a cert)
    /// The last header we proposed (for which we are waiting votes).
    // current_header: Header,

    // Keeps track of current headers
    //TODO: Merge current_headers && processing.
    current_headers: HashMap<Round, HashMap<Digest, Header>>, ///HashMap<Digest, Header>, //Note, re-factored this map to do GC cleaner. 
    // Hashmap containing votes aggregators
    vote_aggregators: HashMap<Round, HashMap<Digest, Box<VotesAggregator>>>,  //HashMap<Digest, VotesAggregator>,
    // /// Aggregates votes into a certificate.
    // votes_aggregator: VotesAggregator,

    //votes_aggregators: HashMap<Round, VotesAggregator>, //TODO: To accomodate all to all, the map should be map<round, map<publickey, VotesAggreagtor>>
    /// Aggregates certificates to use as parents for new headers.
    certificates_aggregators: HashMap<Round, Box<CertificatesAggregator>>, //Keep the Set of Certs = Edges for multiple rounds. //TODO: for all-to-all need to store CertAggregators per node.
    /// A network sender to send the batches to the other workers.
    network: ReliableSender,
    /// Keeps the cancel handlers of the messages we sent.
    cancel_handlers: HashMap<Round, Vec<CancelHandler>>,

   
    // GC the vote aggregators and current headers
    // gc_map: HashMap<Round, Digest>,
}

impl Core {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        name: PublicKey,
        committee: Committee,
        store: Store,
        synchronizer: Synchronizer,
        signature_service: SignatureService,
        consensus_round: Arc<AtomicU64>,
        gc_depth: Round,
        rx_primaries: Receiver<PrimaryMessage>,
        rx_header_waiter: Receiver<Header>,
        rx_certificate_waiter: Receiver<Certificate>,
        rx_proposer: Receiver<Header>,
        tx_consensus: Sender<Certificate>,
        tx_committer: Sender<Certificate>,
        tx_proposer: Sender<(Vec<Digest>, Round)>,
        rx_validation: Receiver<(Header, u8, Option<QC>, Option<TC>)>,
        tx_special: Sender<Header>,
        rx_pushdown_cert: Receiver<Certificate>,
        rx_request_header_sync: Receiver<Digest>,
    ) {
        tokio::spawn(async move {
            Self {
                name,
                //current_header: Header::genesis(&committee),
                committee,
                store,
                synchronizer,
                signature_service,
                consensus_round,
                gc_depth,
                rx_primaries,
                rx_header_waiter,
                rx_certificate_waiter,
                rx_proposer,
                tx_consensus,
                tx_committer,
                tx_proposer,
                rx_validation,
                tx_special,
                rx_pushdown_cert,
                rx_request_header_sync,
                gc_round: 0,
                last_voted: HashMap::with_capacity(2 * gc_depth as usize),
                processing: HashMap::with_capacity(2 * gc_depth as usize),
                //current_header: Header::default(),
                current_headers: HashMap::with_capacity(2 * gc_depth as usize),
                //votes_aggregator: VotesAggregator::new(),
                vote_aggregators: HashMap::with_capacity(2 * gc_depth as usize),
                certificates_aggregators: HashMap::with_capacity(2 * gc_depth as usize),
                network: ReliableSender::new(),
                cancel_handlers: HashMap::with_capacity(2 * gc_depth as usize),
               
                //gc_map: HashMap::with_capacity(2 * gc_depth as usize),
            }
            .run()
            .await;
        });
    }

    async fn process_own_header(&mut self, header: Header) -> DagResult<()> {
    
        // Reset the votes aggregator.
        // self.current_header = header.clone();
        // self.votes_aggregator = VotesAggregator::new();

        //TODO: for all-to-all: Don't let other replicas insert into current_headers twice.

        self.current_headers
            .entry(header.round)
            .or_insert_with(HashMap::new)
            .entry(header.id.clone())
            .or_insert(header.clone());

        self.processing
        .entry(header.round)
        .or_insert_with(HashSet::new)
        .insert(header.id.clone());


        // Broadcast the new header in a reliable manner.
        let addresses = self
            .committee
            .others_primaries(&self.name)
            .iter()
            .map(|(_, x)| x.primary_to_primary)
            .collect();
        let bytes = bincode::serialize(&PrimaryMessage::Header(header.clone()))
            .expect("Failed to serialize our own header");
        let handlers = self.network.broadcast(addresses, Bytes::from(bytes)).await;
        self.cancel_handlers
            .entry(header.round)
            .or_insert_with(Vec::new)
            .extend(handlers);

        // Process the header.
        self.process_header(header).await
    }

    #[async_recursion]
    async fn process_header(&mut self, header: Header) -> DagResult<()> {
        debug!("Processing {:?}", header);
        // Indicate that we are processing this header.
        self.processing
            .entry(header.round)
            .or_insert_with(HashSet::new)
            .insert(header.id.clone());

        
        // Ensure we have the parents. If at least one parent is missing, the synchronizer returns an empty
        // vector; it will gather the missing parents (as well as all ancestors) from other nodes and then
        // reschedule processing of this header.

        //TODO: Modify so we don't have to wait for parent cert if the block is special -- but need to wait for parent.

        if header.is_special && header.special_parent.is_some() {
            //TODO: Check that parent header has been received. // IF so, then when a cert for this special block forms, also form a dummy cert of the parent.
            //1) Read from store to confirm parent exists.
            
             // let (special_parent, is_genesis) = self.synchronizer.get_special_parent(&header, false).await?;  
            let sync_if_missing = false;
            match self.synchronizer.get_special_parent(&header, sync_if_missing).await? {
                (None, _) => return Ok(()),
                (Some(special_parent_header), is_genesis) => { 
                  //TODO: FIXME: In practice the genesis case should never be triggered (just used for unit testing). In normal processing the first special block would have genesis certs as parents.
                   
                  //TODO: Want to check that proposes round is justified by EITHER ticket or parents.
                  debug!("special parent round: {}, header.special_parent_round: {}", special_parent_header.round, header.special_parent_round);
                  ensure!( //check that special parent round matches claimed round
                        special_parent_header.round == header.special_parent_round, // && header.special_parent_round + 1 == header.round,  //FIXME: this is not true -> we might have skipped rounds
                        DagError::MalformedHeader(header.id.clone())
                    );
                    debug!("special parent author: {}, header.author: {}. is_genesis {}", special_parent_header.author, header.author, is_genesis,);
                    ensure!( //check that special parent and special header have same parent //TODO: OR parent = genesis header.
                         special_parent_header.author == header.author || is_genesis,
                         DagError::MalformedHeader(header.id.clone())
                    );
                }
                
            };
        }
        
        let (parents, is_missing_parents) = self.synchronizer.get_parents(&header).await?;
        if is_missing_parents {
            debug!("Processing of {} suspended: missing parent(s)", header.id);
            return Ok(());
        }

            // Check the parent certificates. Ensure the parents form a quorum and are all from the previous round.
            //Note: Does not apply to special blocks who have a special edge -> if they skip rounds can have long edges.
        if !header.is_special || header.special_parent.is_none(){
            debug!("Checking parent certificates");
            let mut ticket_bound_round = false;
            if header.is_special {
                let ticket = header.ticket.as_ref().expect("special header must have ticket");
                ticket_bound_round = header.round == ticket.round + 1;
            }

            let mut stake = 0;
            for x in parents {
                ensure!(
                    x.round() + 1 == header.round || ticket_bound_round, // header.is_special, //FIXME: TODO: Want to check that proposed round is justified by EITHER ticket or parents.                                                                 
                    DagError::MalformedHeader(header.id.clone())
                );
                stake += self.committee.stake(&x.origin());

                
            }
            
            ensure!(
                stake >= self.committee.quorum_threshold() || (header.is_special && header.special_parent.is_some()), 
                DagError::HeaderRequiresQuorum(header.id.clone())
            );
        }
      
            
    

        // Ensure we have the payload. If we don't, the synchronizer will ask our workers to get it, and then
        // reschedule processing of this header once we have it.
        if self.synchronizer.missing_payload(&header).await? {
            debug!("Processing of {} suspended: missing payload", header);
            return Ok(());
        }


        // Store the header.
        let bytes = bincode::serialize(&header).expect("Failed to serialize header");
        self.store.write(header.id.to_vec(), bytes).await;

        // Check if we can vote for this header.
        if self
            .last_voted
            .entry(header.round)
            .or_insert_with(HashSet::new)
            .insert(header.author)  //checks that we have not already voted.
        {
        
           // println!("Processing Last voted.  vote for origin: {}, header id {}, round {}. Called on replica {}", header.author.clone(), header.id.clone(), header.round.clone(), self.name.clone());
           //  For special headers: Upcall to consensus layer to confirm whether the special header is valid for consensus
            if header.is_special {
                self.tx_special
                .send(header)
                .await
                .expect("Failed to send special header");
            }
            else{
                 // Make a vote and send it to the header's creator.
                 
                return self.create_vote(header, 0, None, None).await;
            }
        }
        else{
            debug!("have already voted for header from author {} in round {}", header.author, header.round);
        }
           
        Ok(())
    }

    #[async_recursion]
    async fn create_vote(&mut self, header: Header, special_valid: u8, qc: Option<QC>, tc: Option<TC>) -> DagResult<()>{ 
        //Argument "special_valid" confirms whether a special header should be considered for consensus or not. Invalid votes must contain a TC or QC proving the view is outdated. (All of this should be passed down by the consensus layer)
        //Note: Normal headers have special_valid = 0, and no QC/TC
        
         // Make a vote and send it to the header's creator.

         let vote = Vote::new(&header, &self.name, &mut self.signature_service, special_valid, qc, tc).await;
         debug!("Created {:?}", vote);

         if vote.origin == self.name {
             //println!("Calling process vote on our own header. Replica {}, Header id {}", header.author.clone(), header.id.clone());
             self.process_vote(vote)
                 .await
                 .expect("Failed to process our own vote");
         } else {
             let address = self
                 .committee
                 .primary(&header.author)
                 .expect("Author of valid header is not in the committee")
                 .primary_to_primary;
             let bytes = bincode::serialize(&PrimaryMessage::Vote(vote))
                 .expect("Failed to serialize our own vote");
             let handler = self.network.send(address, Bytes::from(bytes)).await;    // TODO: For special block: May want to use all to all broadcast for replies.
             self.cancel_handlers
                 .entry(header.round)
                 .or_insert_with(Vec::new)
                 .push(handler);
         }
         Ok(())
    }

    #[async_recursion]
    async fn process_vote(&mut self, vote: Vote) -> DagResult<()> {
        debug!("Processing {:?}", vote);

        //TODO: For all to all communication: curr_header might exist --> must add to current_headers inside process_header, and require a sync here on current_header being in store.
       let current_header = self.current_headers
        .entry(vote.round)
        .or_insert_with(HashMap::new)
        .get(&vote.id);

        //println!("Processing vote for origin: {}, header id {}, round {}. Vote sent by replica {}", vote.origin.clone(), vote.id.clone(), vote.round.clone(), vote.author.clone());

        if current_header == None {
            panic!("Currently should only receive votes for our own headers -- which are the only ones in current_header for now. Not yet logged");
            return Ok(());
        }

        let current_header = current_header.unwrap();
   
        let vote_aggregator = self
            .vote_aggregators
            .entry(current_header.round)
            .or_insert_with(HashMap::new)
            .entry(current_header.id.clone())
            .or_insert_with(|| Box::new(VotesAggregator::new()));

        if let (Some(certificate), special_ready) = vote_aggregator.append(vote, &self.committee, current_header)?
        { 
            //Only broadcast and process_cert upon generation of initial cert, and first valid cert (Note: these might be the same). //TODO: broadcast again for Fast-Cert
            //Note: Is it fine for process_cert to be called multiple times?
            // ==> should be. Just replaces cert in all data structures. Sync won't be triggered redundantly. CertAggregator won't add twice
            debug!("Assembled {:?}", certificate);

            // if current_header.is_special {
            //      //If current_header == special && whole quorum is valid ==> pass forward to consensus layer  
            //     if special_ready {
            //         debug!("Assembled complete special certificate: {:?}", certificate);
            //         //Send it to the consensus layer. ==> all replicas will vote.
            //         self.try_upcall_process_special_certificate(&certificate).await;
            //     }
            // }
           
            // Broadcast the certificate.  ==> Note: This will re-broadcast if a special_cert gets formed later.
            let addresses = self
                   .committee
                   .others_primaries(&self.name)
                   .iter()
                   .map(|(_, x)| x.primary_to_primary)
                   .collect();
            let bytes = bincode::serialize(&PrimaryMessage::Certificate(certificate.clone()))
                   .expect("Failed to serialize our own certificate");
            let handlers = self.network.broadcast(addresses, Bytes::from(bytes)).await;
            self.cancel_handlers
                   .entry(certificate.round())
                   .or_insert_with(Vec::new)
                   .extend(handlers);


            //Process the new certificate.
            self.process_certificate(certificate)
                   .await
                   .expect("Failed to process valid certificate");
           
        }

        Ok(())
    }

    
    async fn try_upcall_process_special_certificate(&mut self, certificate: &Certificate){

        debug!("Try upcalling cert {:?}. Is_special: {}, Is_special_valid: {}", certificate, certificate.header.is_special, certificate.is_special_valid(&self.committee));
        //Only upcall if cert is special and valid. Don't upcall for invalid (Note: dummy certs are invalid by default, since valid_weight = 0)
        if certificate.header.is_special && certificate.is_special_valid(&self.committee){
            debug!("Success! Upcalling cert {:?}", certificate);
            if let Err(e) = self.tx_consensus.send(certificate.clone()).await {    //NOTE: Process_cert will be called after this, which will start DAG parent sync. committer.CertificateWaiter will wait for the DAG parents.
                warn!("Failed to deliver certificate {} to the consensus: {}", certificate.header.id.clone(), e);
            }
        }
    }

    // #[async_recursion]
    // async fn receive_certificate(&mut self, certificate: Certificate) -> DagResult<()> {
    //     //NOTE: Could remove this function, and call try_upcall directly; 

    //     if certificate.header.is_special && certificate.is_special_valid(&self.committee){
    //         self.try_upcall_process_special_certificate(&certificate).await;
    //     }
    //     self.process_certificate(certificate).await
    // }

    #[async_recursion]
    async fn process_certificate(&mut self, certificate: Certificate) -> DagResult<()> {
        debug!("Processing Cert: {:?}", certificate);

        //Try upcalling to Consensus layer.
        //Note: Calling this before parent sync could result in a duplicate call if sync puts us to sleep and re-execs ==> However, I took care that process_special_cert does not vote twice.
        self.try_upcall_process_special_certificate(&certificate).await; 

    
        // Process the header embedded in the certificate if we haven't already voted for it (if we already
        // voted, it means we already processed it). Since this header got certified, we are sure that all
        // the data it refers to (ie. its payload and its parents) are available. We can thus continue the
        // processing of the certificate even if we don't have them in store right now.
        if !self
            .processing
            .get(&certificate.header.round)
            .map_or_else(|| false, |x| x.contains(&certificate.header.id))
        {
            // This function may still throw an error if the storage fails.
            self.process_header(certificate.header.clone()).await?;
        }
        
        // Ensure we have all the DAG ancestors of this certificate yet. If we don't, the synchronizer will gather
        // them and trigger re-processing of this certificate.
        if !self.synchronizer.deliver_certificate(&certificate).await? {
            debug!(
                "Processing of {:?} suspended: missing ancestors",
                 certificate
            );
            return Ok(());
        }

         //Additional special block processing
         // //Note: If it is a special block, then don't need to wait for the special edge parent. ==> generate a special cert for the parent to pass to the DAG
        if certificate.header.is_special && certificate.header.special_parent.is_some() {
            let sync_if_missing = false;
            match self.synchronizer.deliver_special_certificate(&certificate, sync_if_missing).await? {
                //If we have parent cert => do nothing
                (None, true) => {},
                // If we have parent header => create cert for committer
                (Some(special_parent_dummy_cert), _) => {
                    //TESTING: Make sure we have logged this header if it is our own (could happen if child parent is processed first due to Tokio async)
                    //This should not be necessary ==> process_own_header should be called in order.
                    if special_parent_dummy_cert.header.author == self.name {
                        let parent_header = special_parent_dummy_cert.header.clone();
                        if None == self.current_headers.entry(parent_header.round).or_insert_with(HashMap::new).get(&parent_header.id) {
                            panic!("This should never happen --> process_own_header should occur in order!");
                        }
                    }
                    self.process_certificate(special_parent_dummy_cert).await?; //TODO: if the parent header is our own, call process_own_header, and add an aggregator to the map.
                                                                                    //Alternatively, if we want to support all-to-all, then we want to declare the aggregator in process_header

                        //FIXME: this process_cert call will result in a dummy cert being stored. Future sync requests will retrieve the dummy cert and fail sanitization....
                            //-> Can fix this by only writing to store if not empty
                        //FIXME: Likewise, when we store a quorum of invalids problems can arise. If we sync on a consensus parent, and downcall to the Dag, the Dag won't upcall to consensus again.
                            // -> can fix this by adding a waiter that retriggers Dag processing upcon being added to store.

                     //Note: This shouldn't be directly sent to committer; it should recursively check for all of its parents as well. 
                            // Just call process_cert, don't need to sanitize - transitively validated via child already)
                            //NOTE: Digest of dummy_cert equivalent to real cert ==> just missing signatures & special_valids
                    //self.tx_committer.send(special_parent_dummy_cert).await.expect("Failed to send special parent certificate to committer");
                   
                    //
                },
                // If we don't have parent => create a waiter. (or just return -- since process_header should have added it.)
                _ => return Ok(())
            }
        }
       
        // Store the certificate.
        let bytes = bincode::serialize(&certificate).expect("Failed to serialize certificate");
        self.store.write(certificate.digest().to_vec(), bytes).await;

        // Check if we have enough certificates to enter a new dag round and propose a header.
        //TODO: Change this into streaming certs. And let the proposer determine when n-f have been collected.
        if let Some(parents) = self
            .certificates_aggregators
            .entry(certificate.round())
            .or_insert_with(|| Box::new(CertificatesAggregator::new()))
            .append(certificate.clone(), &self.committee)?
        {
            //panic!("made it here");
            // Send it to the `Proposer`.
            self.tx_proposer
                .send((parents, certificate.round().clone()))
                .await
                .expect("Failed to send certificate");

        }


        //Committer Invariant: In order to commit, must have stored all parent certs
        //For normal blocks, this is guaranteed by synchronizer which forces a wait on processing until all parent certs have been processed
        //For special blocks this is not guaranteed, since they may have a special parent edge whose cert is not available. In this case, we want to confirm that the header is available, and then 
        //generate a special cert for it to pass  pass on. (for consistency, always make this cert have empty votes)
       
    

        //forward cert to Consensus Dag view. NOTE: Forwards cert with whatever special_valids are available. This might not be the ones we use for a special block that is passed up
        //Currently this is safe/compatible because neither votes, norspecial_valids are part of the cert digest and equality definition (I consider special_valids to be "extra" info of the signatures)
        
        debug!("Committer Received {:?}", certificate);
        self.tx_committer
                .send(certificate.clone())
                .await
                .expect("Failed to send certificate to committer");
        
        Ok(())
    }

    fn sanitize_header(&mut self, header: &Header) -> DagResult<()> {
        ensure!(
            self.gc_round <= header.round,
            DagError::HeaderTooOld(header.id.clone(), header.round)
        );

        // Verify the header's signature.
        header.verify(&self.committee)?;

        // TODO [issue #3]: Prevent bad nodes from sending junk headers with high round numbers.

        Ok(())
    }

    fn sanitize_vote(&mut self, vote: &Vote) -> DagResult<()> {

        //println!("Received vote for origin: {}, header id {}, round {}. Vote sent by replica {}", vote.origin.clone(), vote.id.clone(), vote.round.clone(), vote.author.clone());
        ensure!(
            self.current_headers.get(&vote.round) != None,
            DagError::VoteTooOld(vote.digest(), vote.round)
        );
        // ensure!(
        //     self.current_header.round <= vote.round,
        //     DagError::VoteTooOld(vote.digest(), vote.round)
        // );
       
        // Ensure we receive a vote on the expected header.
        let current_header = self.current_headers.entry(vote.round).or_insert_with(HashMap::new).get(&vote.id);
        ensure!(
            current_header != None && current_header.unwrap().author == vote.origin,
            DagError::UnexpectedVote(vote.id.clone())
        );
        // ensure!(
        //     vote.id == self.current_header.id
        //         && vote.origin == self.current_header.author
        //         && vote.round == self.current_header.round,
        //     DagError::UnexpectedVote(vote.id.clone())
        // );
        
    
        //Deprecated code for Invalid vote proofs 
        // if false && self.current_header.is_special && vote.special_valid == 0 {
        //     match &vote.tc {
        //         Some(tc) => { //invalidation proof = a TC that formed for the current view (or a future one). Implies one cannot vote in this view anymore.
        //             ensure!( 
        //                 tc.view >= self.current_header.view,
        //                 DagError::InvalidVoteInvalidation
        //             );
        //             match tc.verify(&self.committee) {
        //                 Ok(()) => {},
        //                 _ => return Err(DagError::InvalidVoteInvalidation)
        //             }
                    
        //          }, 
        //         None => { 
        //             match &vote.qc { 
        //                 Some(qc) => { //invalidation proof = a QC that formed for a future view (i.e. an extension of some TC in current view or future)
        //                     ensure!( //proof is actually showing a conflict.
        //                         qc.view > self.current_header.view,
        //                         DagError::InvalidVoteInvalidation
        //                     );
        //                     match qc.verify(&self.committee) {
        //                         Ok(()) => {},
        //                         _ => return Err(DagError::InvalidVoteInvalidation)
        //                     }
        //                 }
        //                 None => { return Err(DagError::InvalidVoteInvalidation)}
        //             }
        //         }, 
        //     }
        // } 
       
        // Verify the vote.
        vote.verify(&self.committee).map_err(DagError::from)
    }

    fn sanitize_certificate(&mut self, certificate: &Certificate) -> DagResult<()> {
        ensure!(
            self.gc_round <= certificate.round(),
            DagError::CertificateTooOld(certificate.digest(), certificate.round())
        );
        
        // Verify the certificate (and the embedded header).
        certificate.verify(&self.committee).map_err(DagError::from)
    }

    // Main loop listening to incoming messages.
    pub async fn run(&mut self) {
        //write starting header to store --> just to support Special unit testing
        // let bytes = bincode::serialize(&Header::default()).expect("Failed to serialize header");
        // self.store.write(Header::default().id.to_vec(), bytes).await;
        //
        let genesis_header = Header::genesis(&self.committee);
        self.current_headers.entry(0).or_insert_with(HashMap::new).insert(genesis_header.id.clone(), genesis_header);

        loop {
            let result = tokio::select! {
                // We receive here messages from other primaries.
                Some(message) = self.rx_primaries.recv() => {
                    match message {
                        PrimaryMessage::Header(header) => {
                            match self.sanitize_header(&header) {
                                Ok(()) => self.process_header(header).await,
                                error => error
                            }

                        },
                        PrimaryMessage::Vote(vote) => {
                            match self.sanitize_vote(&vote) {
                                Ok(()) => self.process_vote(vote).await,
                                error => error
                            }
                        },
                        PrimaryMessage::Certificate(certificate) => {
                            match self.sanitize_certificate(&certificate) {
                                Ok(()) => self.process_certificate(certificate).await, //self.receive_certificate(certificate).await, 
                                error => error
                            }
                        },
                        _ => panic!("Unexpected core message")
                    }
                },

                // We also receive here our new headers created by the `Proposer`.
                Some(header) = self.rx_proposer.recv() => self.process_own_header(header).await,

                // We receive here loopback headers from the `HeaderWaiter`. Those are headers for which we interrupted
                // execution (we were missing some of their dependencies) and we are now ready to resume processing.
                Some(header) = self.rx_header_waiter.recv() => self.process_header(header).await,

                //Loopback for special headers that were validated by consensus layer.
                Some((header, special_valid, qc, tc)) = self.rx_validation.recv() => self.create_vote(header, special_valid, qc, tc).await,               
                //i.e. core requests validation from consensus (check if ticket valid; wait to receive ticket if we don't have it yet -- should arrive: using all to all or forwarding)

                Some(header_digest) = self.rx_request_header_sync.recv() => self.synchronizer.fetch_header(header_digest).await,
                
                
                // We receive here loopback certificates from the `CertificateWaiter`. Those are certificates for which
                // we interrupted execution (we were missing some of their ancestors) and we are now ready to resume
                // processing.
                Some(certificate) = self.rx_certificate_waiter.recv() => self.process_certificate(certificate).await,

                // DEPRECATED: Loopback certificates from Consensus layer. Those are certificates of consensus parents. //TODO: This might be redundant sync with the Dag layers sync.
                Some(certificate) = self.rx_pushdown_cert.recv() => self.process_certificate(certificate).await,

            
                


            };
            match result {
                Ok(()) => (),
                Err(DagError::StoreError(e)) => {
                    error!("{}", e);
                    panic!("Storage failure: killing node.");
                }
                Err(e @ DagError::HeaderTooOld(..)) => debug!("{}", e),
                Err(e @ DagError::VoteTooOld(..)) => debug!("{}", e),
                Err(e @ DagError::CertificateTooOld(..)) => debug!("{}", e),
                Err(e) => warn!("{}", e),
            }

            // Cleanup internal state.
            let round = self.consensus_round.load(Ordering::Relaxed);
            if round > self.gc_depth {
                let gc_round = round - self.gc_depth;
                self.last_voted.retain(|k, _| k >= &gc_round);
                self.processing.retain(|k, _| k >= &gc_round);

                self.current_headers.retain(|k, _| k >= &gc_round);
                self.vote_aggregators.retain(|k, _| k >= &gc_round);

                self.certificates_aggregators.retain(|k, _| k >= &gc_round);
                self.cancel_handlers.retain(|k, _| k >= &gc_round);
                self.gc_round = gc_round;
                debug!("GC round moved to {}", self.gc_round);
            }
        }
    }
}
