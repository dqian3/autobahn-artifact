use crate::aggregator::Aggregator;
use crate::consensus::{ConsensusMessage, View, Round};
//use crate::error::{ConsensusError, ConsensusResult};
use crate::leader::LeaderElector;
use crate::mempool::MempoolDriver;
//use crate::messages::{Header, Certificate, Block, Timeout, Vote, AcceptVote, QC, TC};
use crate::proposer::ProposerMessage;
use crate::synchronizer::Synchronizer;
use crate::timer::Timer;
use async_recursion::async_recursion;
use bytes::Bytes;
use config::Committee;
use crypto::Hash as _;
use crypto::{PublicKey, SignatureService, Digest};
use log::{debug, error, warn};
use network::SimpleSender;
use primary::messages::{Block, Header, Certificate, Timeout, AcceptVote, Vote, QC, TC};
use primary::error::{ConsensusError, ConsensusResult};
//use primary::messages::AcceptVote;
use std::cmp::max;
use std::collections::VecDeque;
use std::collections::HashMap;
use store::Store;
use tokio::sync::mpsc::{Receiver, Sender};

#[cfg(test)]
#[path = "tests/core_tests.rs"]
pub mod core_tests;

pub struct Core {
    name: PublicKey,
    committee: Committee,
    store: Store,
    signature_service: SignatureService,
    leader_elector: LeaderElector,
    mempool_driver: MempoolDriver,
    synchronizer: Synchronizer,
    rx_message: Receiver<ConsensusMessage>,
    rx_consensus: Receiver<Certificate>,
    rx_loopback: Receiver<Block>,
    tx_proposer: Sender<ProposerMessage>,
    tx_commit: Sender<Certificate>,
    tx_validation: Sender<(Header, u8, Option<QC>, Option<TC>)>,
    tx_ticket: Sender<(View, Round)>,
    rx_special: Receiver<Header>,
    view: View,
    last_voted_view: View,
    last_committed_view: View,
    stored_headers: HashMap<Digest, Header>,
    high_prepare: Header,
    high_cert: Certificate,  //this is a DAG QC
    high_qc: QC,             //this is a CommitQC
    high_tc: TC,             //last TC --> just in case we need to reply with a proof.
    timer: Timer,
    aggregator: Aggregator,
    network: SimpleSender,
    round: Round, 
}

impl Core {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        name: PublicKey,
        committee: Committee,
        signature_service: SignatureService,
        store: Store,
        leader_elector: LeaderElector,
        mempool_driver: MempoolDriver,
        synchronizer: Synchronizer,
        timeout_delay: u64,
        rx_message: Receiver<ConsensusMessage>,
        rx_consensus: Receiver<Certificate>,
        rx_loopback: Receiver<Block>,
        tx_proposer: Sender<ProposerMessage>,
        tx_commit: Sender<Certificate>,
        tx_validation: Sender<(Header, u8, Option<QC>, Option<TC>)>, // Loopback Special Headers to DAG
        tx_ticket: Sender<(View, Round)>,
        rx_special: Receiver<Header>,
    ) {
        tokio::spawn(async move {
            Self {
                name,
                committee: committee.clone(),
                signature_service,
                store,
                leader_elector,
                mempool_driver,
                synchronizer,
                rx_message,
                rx_consensus,
                rx_loopback,
                tx_proposer,
                tx_commit,
                tx_validation,
                tx_ticket,
                rx_special,
                view: 1,
                last_voted_view: 0,
                last_committed_view: 0,
                high_prepare: Header::default(),
                stored_headers: HashMap::default(),
                high_cert: Certificate::default(),
                high_qc: QC::genesis(),
                high_tc: TC::default(),
                timer: Timer::new(timeout_delay),
                aggregator: Aggregator::new(committee),
                network: SimpleSender::new(),
                round: 1,
            }
            .run()
            .await
        });
    }

    // async fn store_block(&mut self, block: &Block) {
    //     let key = block.digest().to_vec();
    //     let value = bincode::serialize(block).expect("Failed to serialize block");
    //     self.store.write(key, value).await;
    // }

    async fn store_header(&mut self, header: &Header) {
        let key = header.id.to_vec();
        let value = bincode::serialize(header).expect("Failed to serialize header");
        self.store.write(key, value).await;
    }



    fn increase_last_voted_view(&mut self, target: View) {
        self.last_voted_view = max(self.last_voted_view, target);
    }


    // async fn make_vote(&mut self, block: &Block) -> Option<Vote> {
    //     // Check if we can vote for this block.
    //     let safety_rule_1 = block.view > self.last_voted_view;
    //     let mut safety_rule_2 = block.qc.view + 1 == block.view;
    //     if let Some(ref tc) = block.tc {
    //         let mut can_extend = tc.view + 1 == block.view;
    //         can_extend &= block.qc.view >= *tc.high_qc_views().iter().max().expect("Empty TC");
    //         safety_rule_2 |= can_extend;
    //     }
    //     if !(safety_rule_1 && safety_rule_2) {
    //         return None;
    //     }

    //     // Ensure we won't vote for contradicting blocks.
    //     self.increase_last_voted_view(block.view);
    //     // TODO [issue #15]: Write to storage preferred_view and last_voted_view.
    //     Some(Vote::new(&block, self.name, self.signature_service.clone()).await)
    // }
    
    async fn commit(&mut self, header: &Header, qc: &QC) -> ConsensusResult<()> {
        if self.last_committed_view >= header.view {
            return Ok(());
        }

        let mut to_commit = VecDeque::new();
        to_commit.push_back(header.clone());

        // Ensure we commit the entire chain. This is needed after view-change. //TODO:
        // let mut parent = header.clone();
        // while self.last_committed_view + 1 < parent.view {
        //     let ancestor = self
        //         .synchronizer
        //         .get_parent_header(&parent)  //TODO: Define this function. It should make sure that all previous views are committed. Must retrieve all QC/TC for prior views. TODO: FIXME: Header must reference either the ticket (QC/TC)
        //         .await?
        //         .expect("We should have all the ancestors by now");
        //     to_commit.push_back(ancestor.clone());
        //     parent = ancestor;
        // }

        // Save the last committed block.
        self.last_committed_view = header.view;

        // TODO: Implement view change/indirect commit rule here

        // Send all the newly committed blocks to the node's application layer.
        while let Some(header) = to_commit.pop_back() {
            debug!("Committed {:?}", header);

            // // Output the block to the top-level application. //FIXME: This should be after flattening.
            // if let Err(e) = self.tx_output.send(block.clone()).await {
            //     warn!("Failed to send block through the output channel: {}", e);
            // }

            let mut certs = Vec::new();
            // Send the payload to the committer.

            // FIXME: change special_valids to be a valid not None 
            let certificate = Certificate { header, special_valids: Vec::new(), votes: qc.clone().votes }; //TODO: I don't think these votes are ever used. Just create empty vec.
            certs.push(certificate.clone());

            self.tx_commit
                .send(certificate)
                .await
                .expect("Failed to send payload");
            

            // Cleanup the mempool.
            self.mempool_driver.cleanup(certs).await;
        }
        Ok(())
    }


    // async fn commit(&mut self, block: Block, qc: QC) -> ConsensusResult<()> {
    //     if self.last_committed_view >= block.view {
    //         return Ok(());
    //     }

    //     let mut to_commit = VecDeque::new();
    //     to_commit.push_back(block.clone());

    //     // Ensure we commit the entire chain. This is needed after view-change.
    //     let mut parent = block.clone();
    //     while self.last_committed_view + 1 < parent.view {
    //         let ancestor = self
    //             .synchronizer
    //             .get_parent_block(&parent)
    //             .await?
    //             .expect("We should have all the ancestors by now");
    //         to_commit.push_back(ancestor.clone());
    //         parent = ancestor;
    //     }

    //     // Save the last committed block.
    //     self.last_committed_view = block.view;

    //     // Send all the newly committed blocks to the node's application layer.
    //     while let Some(block) = to_commit.pop_back() {
    //         debug!("Committed {:?}", block);

    //         // Output the block to the top-level application.
    //         if let Err(e) = self.tx_output.send(block.clone()).await {
    //             warn!("Failed to send block through the output channel: {}", e);
    //         }

    //         let mut certs = Vec::new();
    //         // Send the payload to the committer.
    //         for header in block.payload {
    //             let certificate = Certificate { header, votes: qc.clone().votes };
    //             certs.push(certificate.clone());

    //             self.tx_commit
    //                 .send(certificate)
    //                 .await
    //                 .expect("Failed to send payload");
    //         }

    //         // Cleanup the mempool.
    //         self.mempool_driver.cleanup(certs).await;
    //     }
    //     Ok(())
    // }

    fn update_high_qc(&mut self, qc: &QC) {
        if qc.view > self.high_qc.view {
            self.high_qc = qc.clone();
        }
    }

    async fn local_timeout_view(&mut self) -> ConsensusResult<()> {
        warn!("Timeout reached for view {}", self.view);

        // Increase the last voted view.
        self.increase_last_voted_view(self.view);

        // Make a timeout message.
        let timeout = Timeout::new(
            self.high_qc.clone(),
            self.view,
            self.name,
            self.signature_service.clone(),
        )
        .await;
        debug!("Created {:?}", timeout);

        // Reset the timer.
        self.timer.reset();

        // Broadcast the timeout message.
        debug!("Broadcasting {:?}", timeout);
        let addresses = self
            .committee
            .others_consensus(&self.name)
            .into_iter()
            .map(|(_, x)| x.consensus_to_consensus)
            .collect();
        let message = bincode::serialize(&ConsensusMessage::Timeout(timeout.clone()))
            .expect("Failed to serialize timeout message");
        self.network
            .broadcast(addresses, Bytes::from(message))
            .await;

        // Process our message.
        self.handle_timeout(&timeout).await
    }

    #[async_recursion]
    async fn handle_vote(&mut self, vote: &Vote) -> ConsensusResult<()> {
        debug!("Processing {:?}", vote);
        if vote.view < self.view {
            return Ok(());
        }

        // Ensure the vote is well formed.
        vote.verify(&self.committee)?;

        // Add the new vote to our aggregator and see if we have a quorum.
        if let Some(qc) = self.aggregator.add_vote(vote.clone())? {
            debug!("Assembled {:?}", qc);

            // Process the QC.
            self.process_qc(&qc).await;

            // Make a new block if we are the next leader.
            if self.name == self.leader_elector.get_leader(self.view) {
                self.generate_proposal(None).await;
                  //TODO: Pass down cert ==> then can include parent edges
            }
        }
        Ok(())
    }

    #[async_recursion]
    async fn handle_accept_vote(&mut self, vote: &AcceptVote) -> ConsensusResult<()> {
        debug!("Processing {:?}", vote);
        if vote.view < self.view {
            return Ok(());
        }

        // Ensure the vote is well formed.
        vote.verify(&self.committee)?;

        // Add the new vote to our aggregator and see if we have a quorum.
        if let Some(qc) = self.aggregator.add_accept_vote(vote.clone())? {
            debug!("Assembled {:?}", qc);

            // Process the QC.  Adopts view, high qc, and resets timer. //Adopt prev_view_round if higher.
            self.process_qc(&qc).await;
             
             // Broadcast the QC. //TODO: alternatively: pass it down as ticket.
             debug!("Broadcasting {:?}", qc);
             let addresses = self
                 .committee
                 .others_consensus(&self.name)
                 .into_iter()
                 .map(|(_, x)| x.consensus_to_consensus)
                 .collect();
             let message = bincode::serialize(&ConsensusMessage::QC(qc.clone()))
                 .expect("Failed to serialize quorum (commit) certificate");
             self.network
                 .broadcast(addresses, Bytes::from(message))
                 .await;

            // Make a new special header if we are the next leader.
            if self.name == self.leader_elector.get_leader(self.view) {
                self.generate_proposal(None).await;
                  //TODO: Pass down ticket
            }
        }
        Ok(())
    }

    //#[async_recursion]
    async fn handle_qc(&mut self, qc: &QC) -> ConsensusResult<()> {
        //verify qc
        // Ensure the vote is well formed.
        //qc.verify(&self.committee)?;
        
        // Process the QC.  Adopts view, high qc, and resets timer. //Adopt prev_view_round if higher.
        self.process_qc(qc).await;
        
        //upcall to app layer: Order dag, and execute.
        let header = self.stored_headers.get(&qc.hash);

        if header.is_some() {
            self.commit(&header.unwrap().clone(), qc);
        }

         // Make a new special header if we are the next leader.
         if self.name == self.leader_elector.get_leader(self.view) {
            self.generate_proposal(None).await;
              //TODO: Pass down ticket
        }

        Ok(())
    }

    async fn handle_timeout(&mut self, timeout: &Timeout) -> ConsensusResult<()> {
        debug!("Processing {:?}", timeout);
        if timeout.view < self.view {
            return Ok(());
        }

        // Ensure the timeout is well formed.
        timeout.verify(&self.committee)?;

        // Process the QC embedded in the timeout.
        self.process_qc(&timeout.high_qc).await;

        // Add the new vote to our aggregator and see if we have a quorum.
        if let Some(tc) = self.aggregator.add_timeout(timeout.clone())? {
            debug!("Assembled {:?}", tc);

            // Try to advance the view.
            self.advance_view(tc.view).await;

            // Broadcast the TC.
            debug!("Broadcasting {:?}", tc);
            let addresses = self
                .committee
                .others_consensus(&self.name)
                .into_iter()
                .map(|(_, x)| x.consensus_to_consensus)
                .collect();
            let message = bincode::serialize(&ConsensusMessage::TC(tc.clone()))
                .expect("Failed to serialize timeout certificate");
            self.network
                .broadcast(addresses, Bytes::from(message))
                .await;

            // Make a new block if we are the next leader.
            if self.name == self.leader_elector.get_leader(self.view) {
                self.generate_proposal(Some(tc)).await;
            }
        }
        Ok(())
    }

    #[async_recursion]
    async fn advance_view(&mut self, view: View) {
        if view < self.view {
            return;
        }
        // Reset the timer and advance view.
        self.timer.reset();
        self.view = view + 1;
        debug!("Moved to view {}", self.view);

        // Cleanup the vote aggregator.
        self.aggregator.cleanup(&self.view);
    }

    #[async_recursion]
    async fn generate_proposal(&mut self, tc: Option<TC>) {

         // QC or TC formed so send ticket to the consensus
         // check to see whether you are the leader is implicit
         //TODO: Also pass down QC/TC if desired.
         self.tx_ticket
             .send((self.view, self.round))
             .await
             .expect("Failed to send view");

             
        // self.tx_proposer
        //     .send(ProposerMessage(self.view, self.high_qc.clone(), tc))
        //     .await
        //     .expect("Failed to send message to proposer");
    }

    async fn process_qc(&mut self, qc: &QC) {
        debug!("Processing {:?}", qc);
        self.advance_view(qc.view).await;
        self.update_high_qc(qc);
        self.round = max(self.round, qc.prev_view_round);
    }

    //Call process_header upon receiving upcall from Dag layer.
    #[async_recursion]
    async fn process_header(&mut self, header: Header) -> ConsensusResult<()> {

        //1) view > last voted view
        ensure!(
            header.view >self.last_voted_view,
            ConsensusError::TooOld(header.id, header.view)
        );
                    //TODO: If we have already voted for this view; or we have already voted in a previoius view for this vote round or bigger ==> then don't vote at all. The proposer must be byz.
                // But if we cannot vote for this view because of a timeout; then do want to vote for Dag, but invalidate the specialness. Problem: Might not have a proof yet. ==> Need to wait for it. 
                //(I.e. if don't have conflict QC/TC, start a waiter)

                //NOTE: Proofs thus only need to prove timeout case (since otherwise replicas would stay silent) ==> i.e. checking TC (or a consecutive QC) for higher view suffices. (I.e. don't need to check round number)

        //2) round > last round
        //Only process special header if the round number is increasing
        ensure!(
            header.round > self.round,
            ConsensusError::NonMonotonicRounds(header.round, self.round)
        );


        //3) Header signature correct
        header.verify(&self.committee)?;

        //4) Header author == view leader
        ensure!(
            header.author == self.leader_elector.get_leader(header.view),
            ConsensusError::WrongProposer
        );

        //5) TODO: Check if Ticket valid.
        // I.e. whether have QC/TC for view v-1 
        //If don't have QC/TC. Start a waiter. If waiter triggers, call this function again (or directly call down validation complete)

        //6) Update latest prepared. Update latest_voted_view.
        self.view = header.view; //
        self.increase_last_voted_view(self.view);
        let copy = header.clone();
        let id = copy.id.clone();
        self.high_prepare = copy.clone();
        self.stored_headers.insert(id, copy);
       
        //self.round = header.round; //TODO: FIXME: only update self.round upon commit(?) (Committed Headers should be monotonic. Since consensus is sequential, should suffice to update it then?)
        
       
        //FIXME: Dummy testing with validation == true.
        let special_valid: u8 = 1;
        let qc: Option<QC> = None;
        let tc: Option<TC> = None;

        // Loopback to RB, confirming that special block is valid.
            self.tx_validation
            .send((header, special_valid, qc, tc))
            .await
            .expect("Failed to send payload");

         
        Ok(())
    }

    //Call process_header upon receiving upcall from Dag layer.
    #[async_recursion]
    async fn process_certificate(&mut self, certificate: Certificate) -> ConsensusResult<()> {
        let header = certificate.clone().header;
        let id = header.clone().id;
        //1) Check if cert is still relevant to current view, if so, update view and high_cert
        ensure!(
            header.view >= self.view,
            ConsensusError::TooOld(header.id, header.view)
        );
        self.advance_view(header.view);
        self.increase_last_voted_view(header.view);

        self.high_cert = certificate;
        self.stored_headers.insert(id, header.clone());

        //2) Send out Vote

        let vote = AcceptVote::new(&header, self.name, self.signature_service.clone()).await;

        debug!("Created {:?}", vote.digest());
        
        let next_leader = self.leader_elector.get_leader(self.view + 1);
        if next_leader == self.name {
            self.handle_accept_vote(&vote).await?;
        } else {
            debug!("Sending {:?} to {}", vote.digest(), next_leader);
            let address = self
                    .committee
                    .consensus(&next_leader)
                    .expect("The next leader is not in the committee")
                    .consensus_to_consensus;
            let message = bincode::serialize(&ConsensusMessage::AcceptVote(vote))
                    .expect("Failed to serialize vote");
            self.network.send(address, Bytes::from(message)).await;
        }
        
        Ok(())
    }



    // #[async_recursion]
    // async fn process_block(&mut self, block: &Block) -> ConsensusResult<()> {
    //     debug!("Processing {:?}", block);

    //     // Let's see if we have the last three ancestors of the block, that is:
    //     //      b0 <- |qc0; b1| <- |qc1; block|
    //     // If we don't, the synchronizer asks for them to other nodes. It will
    //     // then ensure we process both ancestors in the correct order, and
    //     // finally make us resume processing this block.
    //     let (b0, b1) = match self.synchronizer.get_ancestors(block).await? {
    //         Some(ancestors) => ancestors,
    //         None => {
    //             debug!("Processing of {} suspended: missing parent", block.digest());
    //             return Ok(());
    //         }
    //     };

    //     // Store the block only if we have already processed all its ancestors.
    //     self.store_block(block).await;

    //     // Check if we can commit the head of the 2-chain.
    //     // Note that we commit blocks only if we have all its ancestors.
    //     if b0.view + 1 == b1.view {
    //         self.commit(b0, b1.qc).await?;
    //     }

    //     // Ensure the block's view is as expected.
    //     // This check is important: it prevents bad leaders from producing blocks
    //     // far in the future that may cause overflow on the view number.
    //     if block.view != self.view {
    //         return Ok(());
    //     }

    //     // See if we can vote for this block.
    //     if let Some(vote) = self.make_vote(block).await {
    //         debug!("Created {:?}", vote);
    //         let next_leader = self.leader_elector.get_leader(self.view + 1);
    //         if next_leader == self.name {
    //             self.handle_vote(&vote).await?;
    //         } else {
    //             debug!("Sending {:?} to {}", vote, next_leader);
    //             let address = self
    //                 .committee
    //                 .consensus(&next_leader)
    //                 .expect("The next leader is not in the committee")
    //                 .consensus_to_consensus;
    //             let message = bincode::serialize(&ConsensusMessage::Vote(vote))
    //                 .expect("Failed to serialize vote");
    //             self.network.send(address, Bytes::from(message)).await;
    //         }
    //     }
    //     Ok(())
    // }

    // async fn handle_proposal(&mut self, block: &Block) -> ConsensusResult<()> {
    //     let digest = block.digest();

    //     // Ensure the block proposer is the right leader for the view.
    //     ensure!(
    //         block.author == self.leader_elector.get_leader(block.view),
    //         ConsensusError::WrongLeader {
    //             digest,
    //             leader: block.author,
    //             view: block.view
    //         }
    //     );

    //     // Check the block is correctly formed.
    //     block.verify(&self.committee)?;

    //     // Process the special Header. Advance round
    //     let res = self.process_header(&block.payload[0], &block.author).await;

    //     // Process the QC. This may allow us to advance view.
    //     self.process_qc(&block.qc).await;

    //     // Process the TC (if any). This may also allow us to advance view.
    //     if let Some(ref tc) = block.tc {
    //         debug!("Processing (embedded) {:?}", tc);
    //         self.advance_view(tc.view).await;
    //     }

    //     // Check that the payload certificates are valid.
    //     self.mempool_driver.verify(&block).await?;

    //     // All check pass, we can process this block.
    //     self.process_block(block).await
    // }
    
   

    async fn handle_tc(&mut self, tc: TC) -> ConsensusResult<()> {
        tc.verify(&self.committee)?; //FIXME: Added this because I didnt' see any TC validation elsewhere. If there is already, remove this.

        debug!("Processing {:?}", tc);
        self.advance_view(tc.view).await;
        if self.name == self.leader_elector.get_leader(self.view) {
            self.generate_proposal(Some(tc)).await;
        }
        Ok(())
    }

    pub async fn run(&mut self) {
        // Upon booting, generate the very first block (if we are the leader).
        // Also, schedule a timer in case we don't hear from the leader.
        self.timer.reset();
        if self.name == self.leader_elector.get_leader(self.view) {
            self.generate_proposal(None).await;
        }

        // This is the main loop: it processes incoming blocks and votes,
        // and receive timeout notifications from our Timeout Manager.
        loop {
            let result = tokio::select! {
    
                //1) DAG layer headers for special validation
                Some(header) = self.rx_special.recv() => self.process_header(header).await,
                //2) DAG layer certs to vote on
                Some(certificate) = self.rx_consensus.recv() => self.process_certificate(certificate).await, // This creates an Accept Vote (rename vote to accept?)  should come via proposer?
                
                Some(message) = self.rx_message.recv() => match message {   //Receiving Messages from other Replicas
                    //NO LONGER NEEDED: ConsensusMessage::Propose(block) => self.handle_proposal(&block).await,
                    //NO LONGER NEEDED: ConsensusMessage::Vote(vote) => self.handle_vote(&vote).await,
                    //3) Votes from other replicas to form QC.
                    ConsensusMessage::AcceptVote(vote) => self.handle_accept_vote(&vote).await,
                    //4) Receive QC  //For now send separately; but can be made part of (1)
                    ConsensusMessage::QC(qc) => self.handle_qc(&qc).await,
                    //5) Timeouts from other replicas to form TC
                    ConsensusMessage::Timeout(timeout) => self.handle_timeout(&timeout).await,
                    //6) Receive TC
                    ConsensusMessage::TC(tc) => self.handle_tc(tc).await,
                    _ => panic!("Unexpected protocol message")
                },
                
                //NO LONGER NEEDED: Some(block) = self.rx_loopback.recv() => self.process_block(&block).await,  //Processing Block that we propose ourselves. (or that we resume via Synchronizer upcall)
                () = &mut self.timer => self.local_timeout_view().await,
            };
            match result {
                Ok(()) => (),
                Err(ConsensusError::StoreError(e)) => error!("{}", e),
                Err(ConsensusError::SerializationError(e)) => error!("Store corrupted. {}", e),
                Err(e) => warn!("{}", e),
            }
        }
    }
}
