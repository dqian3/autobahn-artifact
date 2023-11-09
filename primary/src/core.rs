// Copyright(C) Facebook, Inc. and its affiliates.
use crate::aggregators::{QCMaker, TCMaker, VotesAggregator};
//use crate::common::special_header;
use crate::error::{DagError, DagResult};
use crate::leader::LeaderElector;
use crate::messages::{
    Certificate, ConsensusMessage, Header, Proposal, Timeout, Vote, TC,
};
use crate::primary::{Height, PrimaryMessage, Slot, View};
use crate::synchronizer::Synchronizer;
use crate::timer::Timer;
use async_recursion::async_recursion;
use bytes::Bytes;
use config::{Committee, Stake};
use crypto::{Digest, PublicKey, SignatureService};
use crypto::{Hash as _, Signature};
use futures::stream::FuturesUnordered;
use futures::{Future, StreamExt};
use log::{debug, error, warn};
use network::{CancelHandler, ReliableSender};
//use tokio::time::error::Elapsed;
use std::collections::{HashMap, HashSet, VecDeque};
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
    rx_header_waiter_instances: Receiver<(ConsensusMessage, Header)>,
    /// Receives our newly created headers from the `Proposer`.
    rx_proposer: Receiver<Header>,
    // Output all certificates to the consensus Dag view
    tx_committer: Sender<ConsensusMessage>,

    /// Send a valid parent certificate to the `Proposer` 
    tx_proposer: Sender<Certificate>,
    // Receive sync requests for headers required at the consensus layer
    rx_request_header_sync: Receiver<Digest>,

    /// The last garbage collected round.
    gc_round: Height,

    /// The authors of the last voted headers. (Ensures only voting for one header per round)
    last_voted: HashMap<Height, HashSet<PublicKey>>,
    /// The last header we proposed (for which we are waiting votes).
    current_header: Header,
    // Whether we have already sent certificate to proposer
    sent_cert_to_proposer: bool,

    // /// Aggregates votes into a certificate.
    votes_aggregator: VotesAggregator,

    network: ReliableSender,
    /// Keeps the cancel handlers of the messages we sent.
    cancel_handlers: HashMap<Height, Vec<CancelHandler>>,

    current_proposal_tips: HashMap<PublicKey, Proposal>,
    views: HashMap<Slot, View>,
    timers: HashSet<(Slot, View)>,
    last_voted_consensus: HashSet<(Slot, View)>,
    timer_futures: FuturesUnordered<Pin<Box<dyn Future<Output = (Slot, View)> + Send>>>,
    // TODO: Add garbage collection, related to how deep pipeline (parameter k)
    qcs: HashMap<Slot, ConsensusMessage>, // NOTE: Store the latest QC for each slot
    qc_makers: HashMap<Digest, QCMaker>,
    current_qcs_formed: usize,
    tc_makers: HashMap<(Slot, View), TCMaker>,
    prepare_tickets: VecDeque<ConsensusMessage>,
    already_proposed_slots: HashSet<Slot>,
    tx_info: Sender<ConsensusMessage>,
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
        rx_header_waiter_instances: Receiver<(ConsensusMessage, Header)>,
        rx_proposer: Receiver<Header>,
        tx_committer: Sender<ConsensusMessage>,
        tx_proposer: Sender<Certificate>,
        rx_request_header_sync: Receiver<Digest>,
        tx_info: Sender<ConsensusMessage>,
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
                rx_proposer,
                tx_committer,
                tx_proposer,
                rx_request_header_sync,
                tx_info,
                leader_elector,
                gc_round: 0,
                current_qcs_formed: 0,
                sent_cert_to_proposer: false,
                last_voted: HashMap::with_capacity(2 * gc_depth as usize),
                current_header: Header::default(),
                votes_aggregator: VotesAggregator::new(),
                network: ReliableSender::new(),
                cancel_handlers: HashMap::with_capacity(2 * gc_depth as usize),
                already_proposed_slots: HashSet::new(),
                current_proposal_tips: HashMap::with_capacity(2 * gc_depth as usize),
                views: HashMap::with_capacity(2 * gc_depth as usize),
                timers: HashSet::with_capacity(2 * gc_depth as usize),
                last_voted_consensus: HashSet::with_capacity(2 * gc_depth as usize),
                qcs: HashMap::with_capacity(2 * gc_depth as usize),
                qc_makers: HashMap::with_capacity(2 * gc_depth as usize),
                tc_makers: HashMap::with_capacity(2 * gc_depth as usize),
                prepare_tickets: VecDeque::with_capacity(2 * gc_depth as usize),
                timeout_delay,
                timer_futures: FuturesUnordered::new(),
                //gc_map: HashMap::with_capacity(2 * gc_depth as usize),
            }
            .run()
            .await;
        });
    }

    async fn process_own_header(&mut self, header: Header) -> DagResult<()> {
        println!("Received own header");
        // Update the current header we are collecting votes for
        self.current_header = header.clone();
        // Indicate that we haven't sent a cert yet for this header
        self.sent_cert_to_proposer = false;

        // Reset the votes aggregator.
        self.votes_aggregator = VotesAggregator::new();

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
        println!("Processing the header with height {:?}", header.height);

        // Check the parent certificate. Ensure the certificate contains a quorum of votes and is
        // at the preivous height
        let stake: Stake = header
            .parent_cert
            .votes
            .iter()
            .map(|(pk, _)| self.committee.stake(pk))
            .sum();
        println!("Before first ensure");
        ensure!(
            header.parent_cert.height() + 1 == header.height(),
            DagError::MalformedHeader(header.id.clone())
        );
        println!("Before second ensure");
        ensure!(
            stake >= self.committee.validity_threshold() || header.parent_cert.height() == 0,
            DagError::HeaderRequiresQuorum(header.id.clone())
        );
        println!("After second ensure");

        // Ensure we have the payload. If we don't, the synchronizer will ask our workers to get it, and then
        // reschedule processing of this header once we have it.
        if self.synchronizer.missing_payload(&header).await? {
            println!("Missing payload");
            debug!("Processing of {} suspended: missing payload", header);
            return Ok(());
        }

        // By FIFO should have parent of this header (and recursively all ancestors), reschedule for processing if we don't
        if self
            .synchronizer
            .get_parent_header(&header)
            .await?
            .is_none()
        {
            println!("The parent is missing");
            return Ok(());
        }

        // Check whether we can seamlessly vote for all consensus messages, if not reschedule
        if !self.is_consensus_ready(&header).await {
            // TODO: Keep track of stats of sync
            // NOTE: This blocks if prepare tips are not available, the leader of the prepare takes
            // on the responsibility of possible blocking i.e. its lane won't continue
            // TODO: Use reputation
            println!("Need to sync on missing tips, reschedule");
            return Ok(());
        }

        println!("storing the header");

        // Store the header since we have the parents (recursively).
        let bytes = bincode::serialize(&header).expect("Failed to serialize header");
        self.store.write(header.digest().to_vec(), bytes).await;

        // If the header received is at a greater height then add it to our local tips and
        // proposals
        if header.height() > self.current_proposal_tips.get(&header.origin()).unwrap().height {
            self.current_proposal_tips.insert(
                header.origin(),
                Proposal {
                    header_digest: header.digest(),
                    height: header.height(),
                },
            );
            println!("updating tip");

            // TODO: Move to a function try_prepare_next_slot()
            // Since we received a new tip, check if any of our pending tickets are ready
            if !self.prepare_tickets.is_empty() {
                println!("checking prepare ticket");
                // Get the first buffered prepare ticket
                let prepare_msg = self.prepare_tickets.pop_front().unwrap();
                let ticket_ready = self.is_prepare_ticket_ready(&prepare_msg).await.unwrap();

                // If the ticket is not ready rebuffer it to the front
                if !ticket_ready {
                    self.prepare_tickets.push_front(prepare_msg);
                }
            }
        }

        println!("after height check");

        // Process the parent certificate
        self.process_certificate(header.clone().parent_cert).await?;

        // Check if we can vote for this header.
        if self
            .last_voted
            .entry(header.height())
            .or_insert_with(HashSet::new)
            .insert(header.author)
        {
            println!("voting for header");
            // Process the consensus instances contained in the header (if any)
            let consensus_sigs = self
                .process_consensus_messages(&header)
                .await?;

            println!("Consensus sigs length {:?}", consensus_sigs.len());

            // Create a vote for the header and any valid consensus instances
            let vote = Vote::new(
                &header,
                &self.name,
                &mut self.signature_service,
                consensus_sigs,
            )
            .await;
            println!("Created vote");
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

        // NOTE: If sending externally then need map of open consensus instances

        // Only process votes for the current header
        if vote.id != self.current_header.id {
            println!("Wrong header");
            return Ok(())
        }

        // Iterate through all votes for each consensus instance
        for (digest, sig) in vote.consensus_sigs.iter() {
            // If not already a qc maker for this consensus instance message, create one
            match self.qc_makers.get(&digest) {
                Some(_) => {
                    println!("QC Maker already exists");
                }
                None => {
                    self.qc_makers.insert(digest.clone(), QCMaker::new());
                }
            }

            println!("digest is {:?}", digest);

            // Otherwise get the qc maker for this instance
            let qc_maker = self.qc_makers.get_mut(&digest).unwrap();

            println!("qc maker weight {:?}", qc_maker.votes.len());

            // Add vote to qc maker, if a QC forms then create a new consensus instance
            // TODO: Put fast path logic in qc maker (decide whether to wait timeout etc.), add
            // external messages
            if let Some(qc) =
                qc_maker.append(vote.author, (digest.clone(), sig.clone()), &self.committee)?
            {
                println!("QC formed");
                self.current_qcs_formed += 1;

                let current_instance = self
                    .current_header
                    .consensus_messages
                    .get(&digest)
                    .unwrap();
                match current_instance {
                    ConsensusMessage::Prepare {
                        slot,
                        view,
                        tc: _,
                        proposals,
                    } => {
                        // Create a tip proposal for the header which contains the prepare message,
                        // so that it can be committed as part of the proposals
                        let leader_tip_proposal: Proposal = Proposal {
                            header_digest: self.current_header.digest(),
                            height: self.current_header.height(),
                        };
                        // Add this cert to the proposals for this instance
                        let mut new_proposals = proposals.clone();
                        new_proposals.insert(self.name, leader_tip_proposal);

                        let new_consensus_message = ConsensusMessage::Confirm {
                            slot: *slot,
                            view: *view,
                            qc,
                            proposals: new_proposals,
                        };

                        // Send this new instance to the proposer
                        self.tx_info
                            .send(new_consensus_message)
                            .await
                            .expect("Failed to send info");
                    }
                    ConsensusMessage::Confirm {
                        slot,
                        view,
                        qc: _,
                        proposals,
                    } => {
                        let new_consensus_message = ConsensusMessage::Commit {
                            slot: *slot,
                            view: *view,
                            qc,
                            proposals: proposals.clone(),
                        };

                        // Send this new instance to the proposer
                        self.tx_info
                            .send(new_consensus_message)
                            .await
                            .expect("Failed to send info");
                    }
                    ConsensusMessage::Commit {
                        slot: _,
                        view: _,
                        qc: _,
                        proposals: _,
                    } => {}
                };
            }
        }

        // Add the vote to the votes aggregator for the actual header
        let dissemination_cert =
            self.votes_aggregator
                .append(vote, &self.committee, &self.current_header)?;
        // If there are no consensus instances in the header then only wait for the dissemination
        // cert (f+1) votes
        let dissemination_ready: bool =
            self.current_header.consensus_messages.is_empty() && dissemination_cert.is_some();
        // If there are some consensus instances in the header then wait for 2f+1 votes to form QCs
        let consensus_ready: bool = !self.current_header.consensus_messages.is_empty() && self.current_qcs_formed == self.current_header.consensus_messages.len();

        if !self.sent_cert_to_proposer && (dissemination_ready || consensus_ready) {
            //debug!("Assembled {:?}", dissemination_cert.unwrap());
            println!("diss ready {:?}, consensus ready {:?}", dissemination_ready, consensus_ready);

            self.tx_proposer
                .send(dissemination_cert.unwrap())
                .await
                .expect("Failed to send certificate");

            self.sent_cert_to_proposer = true;
            println!("after sending to proposer");
            self.current_qcs_formed = 0;
        }

        // TODO: Handle invalidated case where possibly want to send consensus message externally,
        // will add this when the fast path is added
        Ok(())
    }


    #[async_recursion]
    async fn process_certificate(&mut self, certificate: Certificate) -> DagResult<()> {
        debug!("Processing {:?}", certificate);

        // Store the certificate.
        let bytes = bincode::serialize(&certificate).expect("Failed to serialize certificate");
        self.store.write(certificate.digest().to_vec(), bytes).await;

        println!("Stored the certificate: {:?}", certificate.digest());

        // If we receive a new certificate from ourself, then send to the proposer, so it can make
        // a new header
        // TODO: For certified tips need to keep this check and add to list of certified tips
        /*let latest_tip = self.current_proposal_tips.get(&certificate.origin()).unwrap();
        if certificate.origin() == self.name && certificate.height() == latest_tip.height - 1 {
            println!("Sending to proposer");
            // Send it to the `Proposer`.
            self.tx_proposer
                .send(certificate.clone())
                .await
                .expect("Failed to send certificate");
        }*/

        println!("Certificate is {:?}, {:?}", certificate.header_digest, certificate.height);
        Ok(())
    }

    async fn is_prepare_ticket_ready(&mut self, prepare_message: &ConsensusMessage) -> DagResult<bool> {
        match prepare_message {
            ConsensusMessage::Prepare { slot, view: _, tc: _, proposals } => {
                let new_proposals = self.current_proposal_tips.clone();

                // If not the next leader forward the prepare message to the appropriate 
                // leader
                // TODO: Turn off to maximize perf in gracious intervals
                if self.name != self.leader_elector.get_leader(slot + 1, 1) {
                    let address = self
                        .committee
                        .primary(&self.leader_elector.get_leader(slot + 1, 1))
                        .expect("Author of valid header is not in the committee")
                        .primary_to_primary;
                    let bytes = bincode::serialize(&PrimaryMessage::ConsensusMessage(prepare_message.clone()))
                        .expect("Failed to serialize prepare message");
                    let handler = self.network.send(address, Bytes::from(bytes)).await;
                    self.cancel_handlers
                        .entry(self.current_header.height())
                        .or_insert_with(Vec::new)
                        .push(handler);
                    println!("forwarding to the leader");
                    return Ok(true)
                }

                // If we are the leader of the next slot, view 1, and have already proposed in the next slot
                // then don't process the prepare ticket, just return true
                if self.already_proposed_slots.contains(&(slot + 1)) && self.name == self.leader_elector.get_leader(slot + 1, 1) {
                    return Ok(true)
                }

                // If there is enough coverage and we haven't already proposed in the next slot then create a new
                // prepare message if we are the leader of view 1 in the next slot
                if self.enough_coverage(&proposals, &new_proposals) && self.name == self.leader_elector.get_leader(slot + 1, 1) {
                    println!("have enough coverage");
                    let new_prepare_instance = ConsensusMessage::Prepare {
                        slot: slot + 1,
                        view: 1,
                        tc: None,
                        proposals: new_proposals,
                    };
                    self.already_proposed_slots.insert(slot + 1);
                    self.prepare_tickets.pop_front();

                    self.tx_info
                        .send(new_prepare_instance)
                        .await
                        .expect("failed to send info to proposer");
                    return Ok(true);
                } else {
                    // Not enough coverage, add this prepare ticket to the pending queue
                    // until enough new proposals have arrived
                    return Ok(false);
                }
            },
            _ => Ok(true),
        }
    }

    // TODO: Double check these checks are good enough
    fn is_valid(&mut self, consensus_message: &ConsensusMessage) -> bool {
        match consensus_message {
            ConsensusMessage::Prepare { slot, view, tc, proposals } => {
                if self.views.get(slot).is_none() {
                    self.views.insert(*slot, 1);
                }

                // NOTE: There are two cases: view = 1, and view > 1
                // For view = 1 the leader can propose "anything", coverage is
                // enforced on a best effort basis
                // For view > 1, the leader must justify its prepare message with
                // a TC from the previous view, so that proposals that could have committed
                // are recovered
                let mut ticket_valid: bool;
                match tc {
                    Some(tc) => {
                        // Ensure tc is valid
                        ticket_valid = tc.verify(&self.committee).is_ok();
                        
                        let winning_proposals = tc.get_winning_proposals();
                        if !winning_proposals.is_empty() {
                            for (pk, proposal) in proposals {
                                ticket_valid = ticket_valid && proposal.eq(winning_proposals.get(&pk).unwrap());
                            }
                        }
                    },
                    None => {
                        // Any prepare is valid for view 1
                        ticket_valid = *view == 1;
                    },
                };

                // Ensure that we haven't already voted in this slot, view, that the ticket is
                // valid, and we are in the same view
                !self.last_voted_consensus.contains(&(*slot, *view)) && ticket_valid && self.views.get(slot).unwrap() == view
            },
            ConsensusMessage::Confirm { slot, view, qc, proposals: _ } => {
                // Ensure that the QC is valid, and that we are in the same view
                qc.verify(&self.committee).is_ok() && self.views.get(slot).unwrap() == view
            },
            ConsensusMessage::Commit { slot, view, qc, proposals: _ } => {
                // Ensure that the QC is valid, and that we are in the same view
                qc.verify(&self.committee).is_ok() && self.views.get(slot).unwrap() == view
            },
        }
    }

    async fn is_consensus_ready(&mut self, header: &Header) -> bool {
        let mut is_ready = true;
        for (_, consensus_message) in &header.consensus_messages {
            match consensus_message {
                ConsensusMessage::Prepare { slot: _, view: _, tc: _, proposals: _ } => {
                    // Consensus is ready if all proposals for all prepare messages in a car aren't
                    // missing
                    // NOTE: If view > 0 then don't have to call this, only the leader of first
                    // view takes responsibility, only check for view > 0 so you don't need to
                    // check whether winning proposals is correct
                    // TODO: For testing with faults make the certificate of tip syncing happen
                    // asynchronously, change synchronizer so that it write to the store without
                    // the history
                    is_ready = is_ready && !self.synchronizer.get_proposals(consensus_message, header).await.unwrap().is_empty();
                },
                _ => {},
            };
        }
        is_ready
    }

    #[async_recursion]
    async fn process_consensus_messages(
        &mut self,
        header: &Header,
    ) -> DagResult<Vec<(Digest, Signature)>> {
        // Map between consensus instance digest and a signature indicating a vote for that
        // instance
        let mut consensus_sigs: Vec<(Digest, Signature)> = Vec::new();

        for (_, consensus_message) in &header.consensus_messages {
            println!("processing instance");
            if self.is_valid(consensus_message) {
                match consensus_message {
                    ConsensusMessage::Prepare {
                        slot: _,
                        view: _,
                        tc: _,
                        proposals: _,
                    } => {
                        println!("processing prepare message");
                        self.process_prepare_message(consensus_message, consensus_sigs.as_mut()).await;
                    },
                    ConsensusMessage::Confirm {
                        slot: _,
                        view: _,
                        qc: _,
                        proposals: _,
                    } => {
                        println!("processing confirm message");
                        // Start syncing on the proposals if we haven't already
                        self.synchronizer.get_proposals(consensus_message, header).await?;
                        self.process_confirm_message(consensus_message, consensus_sigs.as_mut()).await;
                    },
                    ConsensusMessage::Commit {
                        slot: _,
                        view: _,
                        qc: _,
                        proposals: _,
                    } => {
                        println!("processing commit message");
                        self.process_commit_message(consensus_message.clone(), header).await?;
                    }
                }
            }
        }

        println!("Returning from process consensus size of consensus sigs {:?}", consensus_sigs.len());
        Ok(consensus_sigs)
    }

    //#[async_recursion]
    async fn process_prepare_message(
        &mut self,
        prepare_message: &ConsensusMessage,
        consensus_sigs: &mut Vec<(Digest, Signature)>,
    ) {
        match prepare_message {
            ConsensusMessage::Prepare {
                slot,
                view,
                tc: _,
                proposals: _,
            } => {
                // Check if this prepare message can be used for a ticket to propose in the next
                // slot
                // TODO: Remove from process_header
                if !self.is_prepare_ticket_ready(prepare_message).await.unwrap() {
                    println!("prepare ticket not ready");
                    self.prepare_tickets.push_back(prepare_message.clone());
                }


                // If we haven't already started the timer for the next slot, start it
                // TODO:Can implement different forwarding methods (can be random, can forward to
                // f+1, current one is the most pessimisstic)
                if !self.timers.contains(&(slot + 1, 1)) {
                    let timer = Timer::new(slot + 1, 1, self.timeout_delay);
                    self.timer_futures.push(Box::pin(timer));
                    self.timers.insert((slot + 1, 1));
                }


                // Indicate that we vote for this instance's prepare message
                let sig = self
                    .signature_service
                    .request_signature(prepare_message.digest())
                    .await;
                consensus_sigs.push((prepare_message.digest(), sig));

                // Ensure that we don't vote for another prepare in this slot, view
                self.last_voted_consensus.insert((*slot, *view));
            }
            _ => {}
        }
    }

    //#[async_recursion]
    async fn process_confirm_message(
        &mut self,
        confirm_message: &ConsensusMessage,
        consensus_sigs: &mut Vec<(Digest, Signature)>,
    ) {
        match confirm_message {
            ConsensusMessage::Confirm {
                slot,
                view: _,
                qc: _,
                proposals: _,
            } => {
                // Already checked that we were in the right view from validity checks, so just
                // insert into our local qc map
                self.qcs.insert(*slot, confirm_message.clone());

                // Indicate that we vote for this instance's confirm message
                let sig = self
                    .signature_service
                    .request_signature(confirm_message.digest())
                    .await;
                consensus_sigs.push((confirm_message.digest(), sig));
            }
            _ => {}
        }
    }

    fn enough_coverage(
        &mut self,
        prepare_proposals: &HashMap<PublicKey, Proposal>,
        current_proposals: &HashMap<PublicKey, Proposal>,
    ) -> bool {
        // Checks whether there have been n-f new certs from the proposals from the ticket
        let new_tips: HashMap<&PublicKey, &Proposal> = current_proposals
            .iter()
            .filter(|(pk, proposal)| proposal.height > prepare_proposals.get(&pk).unwrap().height)
            .collect();

        new_tips.len() as u32 >= self.committee.quorum_threshold()
    }

    #[async_recursion]
    async fn process_commit_message(&mut self, commit_message: ConsensusMessage, header: &Header) -> DagResult<()> {
        println!("Called process commit");
        match &commit_message {
            ConsensusMessage::Commit {
                slot: _,
                view: _,
                qc: _,
                proposals: _,
            } => {
                // TODO: Stop all timers for this view
                // Only send to committer if proposals and all ancestors are stored locally,
                // otherwise sync will be triggered, and this commit message will be reprocessed
                if !self.synchronizer.get_proposals(&commit_message, &header).await.unwrap().is_empty() {
                    println!("Sent to committer");
                    self.tx_committer
                        .send(commit_message)
                        .await
                        .expect("Failed to send headers");
                }
            }
            _ => {}
        }

        Ok(())
    }

    #[async_recursion]
    async fn process_loopback(&mut self, consensus_message: ConsensusMessage, header: Header) -> DagResult<()> {
        match &consensus_message {
            ConsensusMessage::Prepare { slot: _, view: _, tc: _, proposals: _ } => {
                // Now that proposals are ready we can reprocess the header
                self.process_header(header).await?;
            },
            ConsensusMessage::Confirm { slot: _, view: _, qc: _, proposals: _ } => {
                // Don't need to do anything for the confirm case, since proposals will be
                // sent to the committer once a commit message is received
            },
            ConsensusMessage::Commit { slot: _, view: _, qc: _, proposals: _ } => {
                // Send the commit message to the committer to order everything
                self.tx_committer
                    .send(consensus_message)
                    .await
                    .expect("Failed to send to committer");
            },
        };
        Ok(())
    }

    async fn process_forwarded_message(&mut self, consensus_message: ConsensusMessage) -> DagResult<()> {
        match &consensus_message {
            ConsensusMessage::Prepare { slot: _, view: _, tc: _, proposals: _ } => {
                // We have a ticket for instance (slot + 1, 1), so check if we have enough coverage
                // to send a prepare message, otherwise buffer it
                if !self.is_prepare_ticket_ready(&consensus_message).await.unwrap() {
                    self.prepare_tickets.push_back(consensus_message);
                }
            },
            ConsensusMessage::Commit { slot: _, view: _, qc: _, proposals: _ } => {
                // Process any forwarded commit messages
                // NOTE: Used "dummy header" for second argument for now, header doesn't matter since proposal syncing
                // does not block processing the header, only prepare messages do
                self.process_commit_message(consensus_message, &self.current_header.clone());
            },
            _ => {}
        }
        Ok(())
    }


    async fn local_timeout_round(&mut self, slot: Slot, view: View) -> DagResult<()> {
        warn!("Timeout reached for slot {}, view {}", slot, view);
        // If timing out a smaller view than the current view, ignore
        match self.views.get(&slot) {
            Some(v) => {
                if *v > view {
                    return Ok(());
                }
            },
            None => {},
        };

        // Make a timeout message.for the slot, view, containing the highest QC this replica has
        // seen
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

        // TODO: If already committed then don't need to verify, just forward commit

        // Don't process timeout messages for old views
        match self.views.get(&timeout.slot) {
            Some(view) => {
                if timeout.view < *view {
                    return Ok(());
                }
            }
            _ => {}
        };

        // Ensure the timeout is well formed.
        timeout.verify(&self.committee)?;

        // If we haven't seen a timeout for this slot, view, then create a new TC maker for it.
        if self.tc_makers.get(&(timeout.slot, timeout.view)).is_none() {
            self.tc_makers
                .insert((timeout.slot, timeout.view), TCMaker::new());
        }

        // Otherwise, get the TC maker for this slot, view.
        let tc_maker = self
            .tc_makers
            .get_mut(&(timeout.slot, timeout.view))
            .unwrap();

        // Add the new vote to our aggregator and see if we have a quorum.
        if let Some(tc) = tc_maker.append(timeout.clone(), &self.committee)? {
            debug!("Assembled {:?}", tc);

            // Try to advance the view
            self.views.insert(timeout.slot, timeout.view + 1);

            // Start the new view timer
            let timer = Timer::new(tc.slot, tc.view + 1, self.timeout_delay);
            self.timer_futures.push(Box::pin(timer));
            self.timers.insert((tc.slot, tc.view + 1));

            // Broadcast the TC.
            // TODO: Low priority: If you see f+1 timeouts then join the mutiny
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

            // Generate a new prepare if we are the next leader.
            self.generate_prepare_from_tc(&tc).await?;
        }
        Ok(())
    }

    async fn generate_prepare_from_tc(&mut self, tc: &TC) -> DagResult<()> {
        // Make a new prepare message if we are the next leader.
        if self.name == self.leader_elector.get_leader(tc.slot, tc.view + 1) {
            let mut winning_proposals = tc.get_winning_proposals();

            // If there is no QC we have to propose, then use our current tips for our proposal
            if winning_proposals.is_empty() {
                winning_proposals = self.current_proposal_tips.clone();
            }

            // Create a prepare message for the next view, containing the ticket and proposals
            // TODO: Low priority can make winning proposals empty
            let prepare_message: ConsensusMessage = ConsensusMessage::Prepare {
                slot: tc.slot,
                view: tc.view + 1,
                tc: Some(tc.clone()),
                proposals: winning_proposals.clone(),
            };
            self.tx_info
                .send(prepare_message.clone())
                .await
                .expect("Failed to send consensus instance");

            // A TC could be a ticket for the next slot
            // NOTE: This is TC Ticket optimization code, commented out for now
            /*if !self.already_proposed_slots.contains(&(timeout.slot + 1))
                && self.enough_coverage(&ticket.proposals, &winning_proposals)
            {
                let new_prepare_instance = ConsensusMessage::Prepare {
                    slot: timeout.slot + 1,
                    view: 1,
                    tc: None,
                    proposals: winning_proposals,
                };
                self.already_proposed_slots.insert(timeout.slot + 1);
                self.tx_info
                    .send(new_prepare_instance)
                    .await
                    .expect("failed to send info to proposer");
            } else {
                // Otherwise add the ticket to the queue, and wait later until there
                // are enough new certificates to propose
                self.prepare_tickets.push_back(prepare_instance);
            }*/
        }
        Ok(())
    }

    async fn handle_tc(&mut self, tc: &TC) -> DagResult<()> {
        debug!("Processing {:?}", tc);
        // Generate a new prepare if we are the next leader.
        self.generate_prepare_from_tc(tc).await?;

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
        self.current_proposal_tips = Header::genesis_proposals(&self.committee);

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
                        PrimaryMessage::TC(tc) => self.handle_tc(&tc).await,

                        // We receive a forwarded prepare or commit message from another replica
                        PrimaryMessage::ConsensusMessage(consensus_message) => self.process_forwarded_message(consensus_message).await,
                        _ => panic!("Unexpected core message")
                    }
                },

                // We also receive here our new headers created by the `Proposer`.
                Some(header) = self.rx_proposer.recv() => self.process_own_header(header).await,

                // We receive here loopback headers from the `HeaderWaiter`. Those are headers for which we interrupted
                // execution (we were missing some of their dependencies) and we are now ready to resume processing.
                Some(header) = self.rx_header_waiter.recv() => self.process_header(header).await,

                // Loopback for committed instance that hasn't had all of it ancestors yet
                Some((consensus_message, header)) = self.rx_header_waiter_instances.recv() => self.process_loopback(consensus_message, header).await,
                //Loopback for special headers that were validated by consensus layer.
                //Some((header, consensus_sigs)) = self.rx_validation.recv() => self.create_vote(header, consensus_sigs).await,
                //i.e. core requests validation from consensus (check if ticket valid; wait to receive ticket if we don't have it yet -- should arrive: using all to all or forwarding)

                Some(header_digest) = self.rx_request_header_sync.recv() => self.synchronizer.fetch_header(header_digest).await,

                // We receive here loopback certificates from the `CertificateWaiter`. Those are certificates for which
                // we interrupted execution (we were missing some of their ancestors) and we are now ready to resume
                // processing.
                //Some(certificate) = self.rx_certificate_waiter.recv() => self.process_certificate(certificate).await,

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
                //self.processing.retain(|k, _| k >= &gc_round);

                //self.current_headers.retain(|k, _| k >= &gc_round);
                //self.vote_aggregators.retain(|k, _| k >= &gc_round);

                //self.certificates_aggregators.retain(|k, _| k >= &gc_round);
                self.cancel_handlers.retain(|k, _| k >= &gc_round);
                self.gc_round = gc_round;
                debug!("GC round moved to {}", self.gc_round);
            }
        }
    }
}
