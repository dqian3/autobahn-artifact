// Copyright(C) Facebook, Inc. and its affiliates.
use crate::aggregators::{VotesAggregator, QCMaker, TCMaker};
//use crate::common::special_header;
use crate::error::{DagError, DagResult};
use crate::leader::LeaderElector;
use crate::messages::{Certificate, Header, Vote, TC, Ticket, ConsensusInstance, Timeout};
use crate::primary::{PrimaryMessage, Height, Slot, View};
use crate::synchronizer::Synchronizer;
use crate::timer::Timer;
use async_recursion::async_recursion;
use bytes::Bytes;
use config::{Committee, Stake};
use crypto::{Hash as _, Signature};
use crypto::{Digest, PublicKey, SignatureService};
use futures::{Future, StreamExt};
use futures::stream::FuturesUnordered;
use log::{debug, error, warn};
use network::{CancelHandler, ReliableSender};
//use tokio::time::error::Elapsed;
use std::collections::{HashMap, HashSet, BTreeMap, VecDeque};
use std::pin::Pin;
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
    /// Receives loopback instances from the 'HeaderWaiter'
    rx_header_waiter_instances: Receiver<ConsensusInstance>,
    /// Receives loopback certificates from the `CertificateWaiter`.
    rx_certificate_waiter: Receiver<Certificate>,
    /// Receives our newly created headers from the `Proposer`.
    rx_proposer: Receiver<Header>,
    /// Output special certificates to the consensus layer.
    tx_consensus: Sender<Certificate>,
    // Output all certificates to the consensus Dag view
    tx_committer: Sender<(Slot, Vec<Header>)>,

    /// Send valid a quorum of certificates' ids to the `Proposer` (along with their round).
    tx_proposer: Sender<Certificate>,
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
    //current_headers: HashMap<Height, HashMap<PublicKey, Header>>, ///HashMap<Digest, Header>, //Note, re-factored this map to do GC cleaner. 
    // Hashmap containing votes aggregators
    vote_aggregators: HashMap<Height, HashMap<Digest, Box<VotesAggregator>>>,  //HashMap<Digest, VotesAggregator>,
    // /// Aggregates votes into a certificate.
    votes_aggregator: VotesAggregator,

    //votes_aggregators: HashMap<Round, VotesAggregator>, //TODO: To accomodate all to all, the map should be map<round, map<publickey, VotesAggreagtor>>
    /// A network sender to send the batches to the other workers.
    network: ReliableSender,
    /// Keeps the cancel handlers of the messages we sent.
    cancel_handlers: HashMap<Height, Vec<CancelHandler>>,

    tips: HashMap<PublicKey, Header>,
    current_certs: HashMap<PublicKey, Certificate>,
    views: HashMap<Slot, View>,
    timers: HashSet<Slot>,
    timer_futures: FuturesUnordered<Pin<Box<dyn Future<Output = (Slot, View)> + Send>>>,
    qcs: HashMap<Slot, ConsensusInstance>,
    qc_makers: HashMap<Digest, QCMaker>,
    tc_makers: HashMap<(Slot, View), TCMaker>,
    current_consensus_instances: HashMap<Digest, ConsensusInstance>,
    tcs: HashMap<Slot, TC>,
    tickets: VecDeque<Ticket>,
    already_proposed_slots: HashSet<Slot>,
    committed: HashMap<Slot, ConsensusInstance>,
    tx_info: Sender<ConsensusInstance>,
    leader_elector: LeaderElector,
    timeout_delay: u64,


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
        rx_header_waiter_instances: Receiver<ConsensusInstance>,
        rx_certificate_waiter: Receiver<Certificate>,
        rx_proposer: Receiver<Header>,
        tx_consensus: Sender<Certificate>,
        tx_committer: Sender<(Slot, Vec<Header>)>,
        tx_proposer: Sender<Certificate>,
        tx_special: Sender<Header>,
        rx_pushdown_cert: Receiver<Certificate>,
        rx_request_header_sync: Receiver<Digest>,
        tx_info: Sender<ConsensusInstance>,
        leader_elector: LeaderElector,
        timeout_delay: u64,
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
                rx_header_waiter_instances,
                rx_certificate_waiter,
                rx_proposer,
                tx_consensus,
                tx_committer,
                tx_proposer,
                tx_special,
                rx_pushdown_cert,
                rx_request_header_sync,
                tx_info,
                leader_elector,
                gc_round: 0,
                last_voted: HashMap::with_capacity(2 * gc_depth as usize),
                processing: HashMap::with_capacity(2 * gc_depth as usize),
                current_header: Header::default(),
                votes_aggregator: VotesAggregator::new(),
                vote_aggregators: HashMap::with_capacity(2 * gc_depth as usize),
                network: ReliableSender::new(),
                cancel_handlers: HashMap::with_capacity(2 * gc_depth as usize),
                already_proposed_slots: HashSet::new(),
                tips: HashMap::with_capacity(2 * gc_depth as usize),
                current_certs: HashMap::with_capacity(2 * gc_depth as usize),
                views: HashMap::with_capacity(2 * gc_depth as usize),
                timers: HashSet::with_capacity(2 * gc_depth as usize),
                qcs: HashMap::with_capacity(2 * gc_depth as usize),
                qc_makers: HashMap::with_capacity(2 * gc_depth as usize),
                tc_makers: HashMap::with_capacity(2 * gc_depth as usize),
                tcs: HashMap::with_capacity(2 * gc_depth as usize),
                tickets: VecDeque::with_capacity(2 * gc_depth as usize),
                committed: HashMap::with_capacity(2 * gc_depth as usize),
                current_consensus_instances: HashMap::with_capacity(2 * gc_depth as usize),
                timeout_delay,
                timer_futures: FuturesUnordered::new(),
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
        self.process_header(header).await
    }

    #[async_recursion]
    async fn process_header(&mut self, header: Header) -> DagResult<()> {
        debug!("Processing {:?}", header);
        println!("Processing the header");

        // Check the parent certificate. Ensure the parents form a quorum and are all from the previous round.
        let stake: Stake = header.parent_cert.votes.iter().map(|(pk, _)| self.committee.stake(pk)).sum();
        ensure!(
            header.parent_cert.height() + 1 == header.height(),
            DagError::MalformedHeader(header.id.clone())
        );
        println!("height is correct");
        ensure!(
            stake >= self.committee.validity_threshold() || header.parent_cert == Certificate::genesis_cert(&self.committee),
            DagError::HeaderRequiresQuorum(header.id.clone())
        );
        println!("stake is correct");

        // Ensure we have the payload. If we don't, the synchronizer will ask our workers to get it, and then
        // reschedule processing of this header once we have it.
        if self.synchronizer.missing_payload(&header).await? {
            println!("Missing payload");
            debug!("Processing of {} suspended: missing payload", header);
            return Ok(());
        }

        // Store the header.
        let bytes = bincode::serialize(&header).expect("Failed to serialize header");
        self.store.write(header.id.to_vec(), bytes).await;

        if header.height() > self.tips.get(&header.origin()).unwrap().height() {
            self.tips.insert(header.origin(), header.clone());
        }

        // Process the parent certificate
        self.process_certificate(header.clone().parent_cert).await?;

        // Check if we can vote for this header.
        if self
            .last_voted
            .entry(header.height())
            .or_insert_with(HashSet::new)
            .insert(header.author)
        {
            let consensus_sigs = self.process_consensus_instances(&header, &header.consensus_instances).await?;
            let vote = Vote::new(&header, &self.name, &mut self.signature_service, consensus_sigs).await;
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
                let handler = self.network.send(address, Bytes::from(bytes)).await;
                self.cancel_handlers
                    .entry(header.height())
                    .or_insert_with(Vec::new)
                    .push(handler);
            }
        }
        Ok(())
    }

    #[async_recursion]
    async fn process_vote(&mut self, vote: Vote) -> DagResult<()> {
        debug!("Processing Vote {:?}", vote);
        println!("processing the vote");

        for (digest, sig) in vote.consensus_sigs.iter() {
            match self.qc_makers.get(&digest) {
                Some(_) => {},
                None => {
                    self.qc_makers.insert(digest.clone(), QCMaker::new());
                }
            }

            let qc_maker = self.qc_makers.get_mut(&digest).unwrap();

            if let Some(qc) = qc_maker.append(vote.origin, (digest.clone(), sig.clone()), &self.committee)? {
                let current_instance = self.current_consensus_instances.get(&digest).unwrap();
                match current_instance {
                    ConsensusInstance::Prepare { slot, view, ticket: _, proposals } => {
                        let new_instance = ConsensusInstance::Confirm { slot: *slot, view: *view, qc, proposals: proposals.clone() };

                        self.tx_info
                            .send(new_instance)
                            .await
                            .expect("Failed to send info");

                    },
                    ConsensusInstance::Confirm { slot, view, qc: _, proposals} => {
                        let new_instance = ConsensusInstance::Commit { slot: *slot, view: *view, qc, proposals: proposals.clone() };

                        self.tx_info
                            .send(new_instance)
                            .await
                            .expect("Failed to send info");

                    },
                    ConsensusInstance::Commit { slot: _, view: _, qc: _, proposals: _} => {},
                };
            }
        }

        let (dissemination_cert, consensus_cert) = self.votes_aggregator.append(vote, &self.committee, &self.current_header)?;
        let use_dissemination: bool = self.current_header.consensus_instances.is_empty() && dissemination_cert.is_some();
        let use_consensus: bool = !self.current_header.consensus_instances.is_empty() && consensus_cert.is_some();

        if use_dissemination {
            //debug!("Assembled {:?}", dissemination_cert.unwrap());
            self.process_certificate(dissemination_cert.unwrap())
                .await
                .expect("Failed to process valid certificate");
        }

        if use_consensus {
            //debug!("Assembled {:?}", consensus_cert.unwrap());
            self.process_certificate(consensus_cert.unwrap())
                .await
                .expect("Failed to process valid certificate");
        }

        // TODO: Handle invalidated case
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
                .send(certificate.clone())
                .await
                .expect("Failed to send certificate");
        }


        if certificate.height() > self.current_certs.get(&certificate.origin()).unwrap().height() {
            self.current_certs.insert(certificate.origin(), certificate.clone());

            if !self.tickets.is_empty() {
                let ticket = self.tickets.get(0).unwrap();
                if self.enough_coverage(ticket, &self.current_certs) && !self.already_proposed_slots.contains(&(ticket.slot + 1)) {
                    let new_prepare_instance = ConsensusInstance::Prepare { slot: ticket.slot + 1, view: 1, ticket: ticket.clone(), proposals: self.current_certs }; 
                    self.already_proposed_slots.insert(ticket.slot + 1);
                    self.tickets.pop_front();
                    self.tx_info
                        .send(new_prepare_instance)
                        .await
                        .expect("failed to send info to proposer");

                }
            }
        }


        //forward cert to Consensus Dag view. NOTE: Forwards cert with whatever special_valids are available. This might not be the ones we use for a special block that is passed up
        //Currently this is safe/compatible because neither votes, norspecial_valids are part of the cert digest and equality definition (I consider special_valids to be "extra" info of the signatures)
        
        //debug!("Committer Received {:?}", certificate);
        /*self.tx_committer
                .send(certificate.clone())
                .await
                .expect("Failed to send certificate to committer");*/
        
        Ok(())
    }

    #[async_recursion]
    async fn process_consensus_instances(&mut self, header: &Header, consensus_instances: &Vec<ConsensusInstance>) -> DagResult<Vec<(Digest, Signature)>> {
        //1) Header signature correct => If false, dont need to reply
        let mut consensus_sigs: Vec<(Digest, Signature)> = Vec::new();

        for consensus_instance in consensus_instances {
            println!("processing instance");
            match consensus_instance {
                ConsensusInstance::Prepare { slot, view: _, ticket: _, proposals: _ } => {
                    // TODO: Add validity checks
                    let has_proposed = self.already_proposed_slots.contains(&(slot + 1));
                    if self.name == self.leader_elector.get_leader(slot + 1, 1) && !has_proposed {
                        let ticket = Ticket::new(Some(header.clone()), None, 1, BTreeMap::new()).await;
                        if self.enough_coverage(&ticket, &self.current_certs) {
                            let new_prepare_instance = ConsensusInstance::Prepare { slot: slot + 1, view: 1, ticket, proposals: self.current_certs }; 
                            self.already_proposed_slots.insert(slot + 1);
                            self.tx_info
                                .send(new_prepare_instance)
                                .await
                                .expect("failed to send info to proposer");
                        } else {
                            self.tickets.push_back(ticket);
                        }

                        if !self.timers.contains(&(slot+1)) {
                            // TODO: also forward the ticket to other replicas
                            let timer = Timer::new(slot+1, 1, self.timeout_delay);
                            self.timer_futures.push(Box::pin(timer));
                            self.timers.insert(slot + 1);
                        }
                    }

                    let sig = self.signature_service.request_signature(consensus_instance.digest()).await;
                    consensus_sigs.push((consensus_instance.digest(), sig));
                },
                ConsensusInstance::Confirm { slot, view, qc: _, proposals: _ } => {
                    // TODO: Add validity checks
                    match self.qcs.get(slot) {
                        Some(consensus_metadata) => {
                            match consensus_metadata {
                                ConsensusInstance::Confirm { slot: _, view: other_view, qc: _, proposals: _ } => {
                                    if view > other_view {
                                        self.qcs.insert(*slot, consensus_metadata.clone());
                                    }
                                },
                                _ => {},
                            }
                        },
                        None => {
                            self.qcs.insert(*slot, consensus_instance.clone());
                        }
                    }

                    let sig = self.signature_service.request_signature(consensus_instance.digest()).await;
                    consensus_sigs.push((consensus_instance.digest(), sig));
                },
                ConsensusInstance::Commit { slot: _, view: _, qc: _, proposals: _ } => {
                    // TODO: Add validity checks
                    self.process_commit_instance(consensus_instance.clone());
                },
            }
        }

        Ok(consensus_sigs)
    }

    fn enough_coverage(&mut self, ticket: &Ticket, current_certs: &HashMap<PublicKey, Certificate>) -> bool {
        let new_tips: HashMap<&PublicKey, &Certificate> = current_certs.iter()
            .filter(|(pk, cert)| cert.height() > ticket.proposals.get(&pk).unwrap().height())
            .collect();
        
        new_tips.len() as u32 >= self.committee.quorum_threshold()
    }

    #[async_recursion]
    async fn process_commit_instance(&mut self, consensus_instance: ConsensusInstance) -> DagResult<()> {
        match &consensus_instance {
            ConsensusInstance::Prepare { slot: _, view: _, ticket: _, proposals: _ } => {},
            ConsensusInstance::Confirm { slot: _, view: _, qc: _, proposals: _ } => {},
            ConsensusInstance::Commit { slot, view: _, qc: _, proposals } => {
                let mut all_ancestors: Vec<Header> = Vec::new();
                for (pk, certificate) in proposals {
                    let mut ancestors = self.synchronizer.get_ancestors(certificate.header_digest.clone(), &pk, &certificate.height(), &consensus_instance).await?;
                    if ancestors.is_empty() {
                        debug!("Process suspended, missing ancestors");
                        return Ok(());
                    }
                    all_ancestors.append(&mut ancestors);
                }
                // Send headers to the committer
                self.tx_committer.send((*slot, all_ancestors)).await.expect("Failed to send headers");
            }
        }

        Ok(())
    }

    async fn local_timeout_round(&mut self, slot: Slot, view: View) -> DagResult<()> {
        warn!("Timeout reached for slot {}, view {}", slot, view);

        // Make a timeout message.
        let timeout = Timeout::new(
            slot,
            view,
            self.qcs.get(&slot).cloned(),
            self.name,
            self.signature_service.clone(),
        )
        .await;
        debug!("Created {:?}", timeout);

        // Broadcast the timeout message.
        debug!("Broadcasting {:?}", timeout);
        let addresses = self
            .committee
            .others_consensus(&self.name)
            .into_iter()
            .map(|(_, x)| x.consensus_to_consensus)
            .collect();
        let message = bincode::serialize(&PrimaryMessage::Timeout(timeout.clone()))
            .expect("Failed to serialize timeout message");
        self.network
            .broadcast(addresses, Bytes::from(message))
            .await;

        // Process our message.
        self.handle_timeout(&timeout).await
    }

    async fn handle_timeout(&mut self, timeout: &Timeout) -> DagResult<()> {
        debug!("Processing {:?}", timeout);
        match self.views.get(&timeout.slot) {
            Some(view) => {
                if timeout.view < *view {
                    return Ok(())
                } 
            },
            _ => {},
        };

        // Ensure the timeout is well formed.
        timeout.verify(&self.committee)?;

        if self.tc_makers.get(&(timeout.slot, timeout.view)).is_none() {
            self.tc_makers.insert((timeout.slot, timeout.view), TCMaker::new());
        }

        let tc_maker = self.tc_makers.get_mut(&(timeout.slot, timeout.view)).unwrap();

        // Add the new vote to our aggregator and see if we have a quorum.
        if let Some(tc) = tc_maker.append(timeout.clone(), &self.committee)? {
            debug!("Assembled {:?}", tc);

            // Try to advance the view
            self.views.insert(timeout.slot, timeout.view + 1);

            // Broadcast the TC.
            debug!("Broadcasting {:?}", tc);
            let addresses = self
                .committee
                .others_consensus(&self.name)
                .into_iter()
                .map(|(_, x)| x.consensus_to_consensus)
                .collect();
            let message = bincode::serialize(&PrimaryMessage::TC(tc.clone()))
                .expect("Failed to serialize timeout certificate");
            self.network
                .broadcast(addresses, Bytes::from(message))
                .await;

            // Make a new block if we are the next leader.
            if self.name == self.leader_elector.get_leader(timeout.slot, timeout.view + 1) {
                // TODO: Generate ticket
                let mut winning_proposals = HashMap::new();
                let mut winning_timeout = tc.timeouts.get(0).unwrap();

                for timeout in &tc.timeouts {
                    match &timeout.high_qc {
                        Some(qc) => {
                            match qc {
                                ConsensusInstance::Confirm { slot: _, view: other_view, qc: _, proposals } => {
                                    if other_view > &winning_timeout.view {
                                        winning_timeout = &timeout;
                                        winning_proposals = proposals.clone();
                                    }
                                },
                                _ => {},
                            }
                        },
                        None => {},
                    };
                }

                let ticket: Ticket = Ticket { header: None, tc: Some(tc), slot: timeout.slot, proposals: BTreeMap::new() };

                if winning_proposals.is_empty() {
                    winning_proposals = self.current_certs.clone();
                }

                let prepare_instance: ConsensusInstance = ConsensusInstance::Prepare { slot: timeout.slot, view: timeout.view + 1, ticket, proposals: winning_proposals };
                self.tx_info
                    .send(prepare_instance)
                    .await
                    .expect("Failed to send consensus instance");
            }
        }
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
                                Ok(()) => self.process_header(header).await,
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
                        PrimaryMessage::Timeout(timeout) => self.handle_timeout(&timeout).await,
                        PrimaryMessage::TC(tc) => self.process_tc(&tc).await,
                        _ => panic!("Unexpected core message")
                    }
                },

                // We also receive here our new headers created by the `Proposer`.
                Some(header) = self.rx_proposer.recv() => self.process_own_header(header).await,

                // We receive here loopback headers from the `HeaderWaiter`. Those are headers for which we interrupted
                // execution (we were missing some of their dependencies) and we are now ready to resume processing.
                Some(header) = self.rx_header_waiter.recv() => self.process_header(header).await,

                // Loopback for committed instance that hasn't had all of it ancestors yet
                Some(consensus_instance) = self.rx_header_waiter_instances.recv() => self.process_commit_instance(consensus_instance).await,
                //Loopback for special headers that were validated by consensus layer.
                //Some((header, consensus_sigs)) = self.rx_validation.recv() => self.create_vote(header, consensus_sigs).await,               
                //i.e. core requests validation from consensus (check if ticket valid; wait to receive ticket if we don't have it yet -- should arrive: using all to all or forwarding)

                Some(header_digest) = self.rx_request_header_sync.recv() => self.synchronizer.fetch_header(header_digest).await,
                
                // We receive here loopback certificates from the `CertificateWaiter`. Those are certificates for which
                // we interrupted execution (we were missing some of their ancestors) and we are now ready to resume
                // processing.
                Some(certificate) = self.rx_certificate_waiter.recv() => self.process_certificate(certificate).await,

                // We receive an event that timer expired
                Some((slot, view)) = self.timer_futures.next() => self.local_timeout_round(slot, view).await,

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

                //self.current_headers.retain(|k, _| k >= &gc_round);
                self.vote_aggregators.retain(|k, _| k >= &gc_round);

                //self.certificates_aggregators.retain(|k, _| k >= &gc_round);
                self.cancel_handlers.retain(|k, _| k >= &gc_round);
                self.gc_round = gc_round;
                debug!("GC round moved to {}", self.gc_round);
            }
        }
    }
}
