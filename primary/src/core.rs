// Copyright(C) Facebook, Inc. and its affiliates.
use crate::aggregators::{CertificatesAggregator, VotesAggregator};
//use crate::common::special_header;
use crate::error::{DagError, DagResult};
use crate::messages::{Certificate, Header, Vote, QC, TC, Ticket, Info, PrepareInfo, ConfirmInfo};
use crate::primary::{PrimaryMessage, Height};
use crate::synchronizer::Synchronizer;
use async_recursion::async_recursion;
use bytes::Bytes;
use config::Committee;
use crypto::Hash as _;
use crypto::{Digest, PublicKey, SignatureService};
use log::{debug, error, warn};
use network::{CancelHandler, ReliableSender};
//use tokio::time::error::Elapsed;
use std::collections::{HashMap, HashSet, BTreeMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
//use std::task::Poll;
use store::Store;
use tokio::sync::mpsc::{Receiver, Sender};
//use tokio::time::{sleep, Duration, Instant};


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
    gc_depth: Height,

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
    tx_proposer: Sender<Certificate>,
    // Receives validated special Headers & proof from the consensus layer.
    rx_validation: Receiver<(Header, BTreeMap<Info, bool>)>,
    tx_special: Sender<Header>,

    rx_pushdown_cert: Receiver<Certificate>,
    // Receive sync requests for headers required at the consensus layer
    rx_request_header_sync: Receiver<Digest>, 

    /// The last garbage collected round.
    gc_round: Height,

    /// The authors of the last voted headers. (Ensures only voting for one header per round)
    last_voted: HashMap<Height, HashSet<PublicKey>>,
    // /// The set of headers we are currently processing.
    processing: HashMap<Height, HashSet<Digest>>, //NOTE: Keep processing separate from current_headers ==> to allow us to process multiple headers from same replica (e.g. in case we first got a header that isnt the one that creates a cert)
    /// The last header we proposed (for which we are waiting votes).
    current_header: Header,

    // Keeps track of current headers
    //TODO: Merge current_headers && processing.
    current_headers: HashMap<Height, HashMap<PublicKey, Header>>, ///HashMap<Digest, Header>, //Note, re-factored this map to do GC cleaner. 
    // Hashmap containing votes aggregators
    vote_aggregators: HashMap<Height, HashMap<Digest, Box<VotesAggregator>>>,  //HashMap<Digest, VotesAggregator>,
    // /// Aggregates votes into a certificate.
    votes_aggregator: VotesAggregator,

    //votes_aggregators: HashMap<Round, VotesAggregator>, //TODO: To accomodate all to all, the map should be map<round, map<publickey, VotesAggreagtor>>
    /// Aggregates certificates to use as parents for new headers.
    certificates_aggregators: HashMap<Height, Box<CertificatesAggregator>>, //Keep the Set of Certs = Edges for multiple rounds. //TODO: for all-to-all need to store CertAggregators per node.
    /// A network sender to send the batches to the other workers.
    network: ReliableSender,
    /// Keeps the cancel handlers of the messages we sent.
    cancel_handlers: HashMap<Height, Vec<CancelHandler>>,

   
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
        gc_depth: Height,
        rx_primaries: Receiver<PrimaryMessage>,
        rx_header_waiter: Receiver<Header>,
        rx_certificate_waiter: Receiver<Certificate>,
        rx_proposer: Receiver<Header>,
        tx_consensus: Sender<Certificate>,
        tx_committer: Sender<Certificate>,
        tx_proposer: Sender<Certificate>,
        rx_validation: Receiver<(Header, bool)>,
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
                current_header: Header::default(),
                current_headers: HashMap::with_capacity(2 * gc_depth as usize),
                votes_aggregator: VotesAggregator::new(),
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
        self.current_header = header.clone();
        self.votes_aggregator = VotesAggregator::new();

        //TODO: for all-to-all: Don't let other replicas insert into current_headers twice.

        self.processing
        .entry(header.height)
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
            .entry(header.height)
            .or_insert_with(Vec::new)
            .extend(handlers);

        // Process the header.
        self.process_header(&header).await
    }

    #[async_recursion]
    async fn process_header(&mut self, header: &Header) -> DagResult<()> {
        debug!("Processing {:?}", header);

        // Check the parent certificate. Ensure the parents form a quorum and are all from the previous round.
        let mut stake = header.parent_cert.votes.iter().map(|(pk, _)| self.committee.stake(pk)).sum();
        ensure!(
            header.parent_cert.height() + 1 == header.height(),
            DagError::MalformedHeader(header.id.clone())
        );
        ensure!(
            stake >= self.committee.validity_threshold(),
            DagError::HeaderRequiresQuorum(header.id.clone())
        );

        // Ensure we have the payload. If we don't, the synchronizer will ask our workers to get it, and then
        // reschedule processing of this header once we have it.
        if self.synchronizer.missing_payload(header).await? {
            debug!("Processing of {} suspended: missing payload", header);
            return Ok(());
        }

        // Store the header.
        let bytes = bincode::serialize(header).expect("Failed to serialize header");
        self.store.write(header.id.to_vec(), bytes).await;

        // Check if we can vote for this header.
        if self
            .last_voted
            .entry(header.height())
            .or_insert_with(HashSet::new)
            .insert(header.author)
        {
            self.tx_consensus
                .send(header)
                .await
                .expect("Failed to send header");
        }
        Ok(())
    }

    #[async_recursion]
    async fn create_vote(&mut self, header: Header, prepare_valids: BTreeMap<PrepareInfo, bool>, confirm_valids: BTreeMap<ConfirmInfo, bool>) -> DagResult<()>{ 
         // Make a vote and send it to the header's creator.
         let vote = Vote::new(&header, &self.name, &mut self.signature_service, prepare_valids, confirm_valids).await;
         debug!("Created Vote {:?}", vote);

         if vote.origin == self.name {
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
                 .entry(header.height())
                 .or_insert_with(Vec::new)
                 .push(handler);
         }

         Ok(())
    }

    #[async_recursion]
    async fn process_vote(&mut self, vote: Vote) -> DagResult<()> {
        debug!("Processing Vote {:?}", vote);

        // TODO: Handle invalidated case
        if let (Some(certificate), invalidated_certificate) = self.votes_aggregator.append(vote, &self.committee, &self.current_header)? { 
            debug!("Assembled {:?}", certificate);
            self.process_certificate(certificate)
                .await
                .expect("Failed to process valid certificate");
        }

        Ok(())
    }
    

    #[async_recursion]
    async fn process_certificate(&mut self, certificate: Certificate) -> DagResult<()> {
        debug!("Processing {:?}", certificate);

        // Store the certificate.
        let bytes = bincode::serialize(&certificate).expect("Failed to serialize certificate");
        self.store.write(certificate.digest().to_vec(), bytes).await;

        // Check if we have enough certificates to enter a new dag round and propose a header.
        if certificate.origin() == self.name {
            // Send it to the `Proposer`.
            self.tx_proposer
                .send(certificate)
                .await
                .expect("Failed to send certificate");
        }

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
            self.gc_round <= header.height,
            DagError::HeaderTooOld(header.id.clone(), header.height)
        );

        // Verify the header's signature.
        header.verify(&self.committee)?;

        // TODO [issue #3]: Prevent bad nodes from sending junk headers with high round numbers.

        Ok(())
    }

    fn sanitize_vote(&mut self, vote: &Vote) -> DagResult<()> {

        //println!("Received vote for origin: {}, header id {}, round {}. Vote sent by replica {}", vote.origin.clone(), vote.id.clone(), vote.round.clone(), vote.author.clone());
        /*ensure!(
            self.current_headers.get(&vote.height) != None,
            DagError::VoteTooOld(vote.digest(), vote.height)
        );*/
        // ensure!(
        //     self.current_header.round <= vote.round,
        //     DagError::VoteTooOld(vote.digest(), vote.round)
        // );
       
        // Ensure we receive a vote on the expected header.
        /*let current_header = self.current_headers.entry(vote.height).or_insert_with(HashMap::new).get(&vote.author);
        ensure!(
            current_header != None && current_header.unwrap().author == vote.origin,
            DagError::UnexpectedVote(vote.id.clone())
        );*/
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
            self.gc_round <= certificate.height(),
            DagError::CertificateTooOld(certificate.digest(), certificate.height())
        );

        println!("Past first ensure");
        
        // Verify the certificate (and the embedded header).
        certificate.verify(&self.committee).map_err(DagError::from)
    }

    // Main loop listening to incoming messages.
    pub async fn run(&mut self) {
        loop {
            let result = tokio::select! {
                // We receive here messages from other primaries.
                Some(message) = self.rx_primaries.recv() => {
                    match message {
                        PrimaryMessage::Header(header) => {
                            match self.sanitize_header(&header) {
                                Ok(()) => self.process_header(&header).await,
                                error => error
                            }

                        },
                        PrimaryMessage::Vote(vote) => {
                            match self.sanitize_vote(&vote) {
                                Ok(()) => {
                                    self.process_vote(vote).await
                                },
                                error => {
                                    error
                                }
                            }
                        },
                        PrimaryMessage::Certificate(certificate) => {
                            match self.sanitize_certificate(&certificate) {
                                Ok(()) => self.process_certificate(certificate).await, //self.receive_certificate(certificate).await, 
                                error => {
                                    error
                                }
                            }
                        },
                        _ => panic!("Unexpected core message")
                    }
                },

                // We also receive here our new headers created by the `Proposer`.
                Some(header) = self.rx_proposer.recv() => self.process_own_header(header).await,

                // We receive here loopback headers from the `HeaderWaiter`. Those are headers for which we interrupted
                // execution (we were missing some of their dependencies) and we are now ready to resume processing.
                Some(header) = self.rx_header_waiter.recv() => self.process_header(&header).await,

                //Loopback for special headers that were validated by consensus layer.
                Some((header, prepare_valids, confirm_valids)) = self.rx_validation.recv() => self.create_vote(header, prepare_valids, confirm_valids).await,               
                //i.e. core requests validation from consensus (check if ticket valid; wait to receive ticket if we don't have it yet -- should arrive: using all to all or forwarding)

                Some(header_digest) = self.rx_request_header_sync.recv() => self.synchronizer.fetch_header(header_digest).await,
                
                // We receive here loopback certificates from the `CertificateWaiter`. Those are certificates for which
                // we interrupted execution (we were missing some of their ancestors) and we are now ready to resume
                // processing.
                Some(certificate) = self.rx_certificate_waiter.recv() => self.process_certificate(certificate).await,

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
