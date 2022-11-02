use crate::aggregator::Aggregator;
use crate::consensus::{ConsensusMessage, View};
use crate::error::{ConsensusError, ConsensusResult};
use crate::leader::LeaderElector;
use crate::mempool::MempoolDriver;
use crate::messages::{Block, Timeout, Vote, QC, TC};
use crate::proposer::ProposerMessage;
use crate::synchronizer::Synchronizer;
use crate::timer::Timer;
use async_recursion::async_recursion;
use bytes::Bytes;
use config::Committee;
use crypto::Hash as _;
use crypto::{PublicKey, SignatureService};
use log::{debug, error, warn};
use network::SimpleSender;
use primary::{Certificate, Round, Header};
use std::cmp::max;
use std::collections::VecDeque;
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
    rx_loopback: Receiver<Block>,
    tx_proposer: Sender<ProposerMessage>,
    tx_commit: Sender<Certificate>,
    tx_output: Sender<Block>,
    tx_dag: Sender<Certificate>,
    tx_ticket: Sender<(View, Round)>,
    view: View,
    last_voted_view: View,
    last_committed_view: View,
    high_qc: QC,
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
        rx_loopback: Receiver<Block>,
        tx_proposer: Sender<ProposerMessage>,
        tx_commit: Sender<Certificate>,
        tx_output: Sender<Block>,
        tx_dag: Sender<Certificate>, // Translate a QC into a Certificate and send it to the DAG
        tx_ticket: Sender<(View, Round)>,
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
                rx_loopback,
                tx_proposer,
                tx_commit,
                tx_output,
                tx_dag,
                tx_ticket,
                view: 1,
                last_voted_view: 0,
                last_committed_view: 0,
                high_qc: QC::genesis(),
                timer: Timer::new(timeout_delay),
                aggregator: Aggregator::new(committee),
                network: SimpleSender::new(),
                round: 1,
            }
            .run()
            .await
        });
    }

    async fn store_block(&mut self, block: &Block) {
        let key = block.digest().to_vec();
        let value = bincode::serialize(block).expect("Failed to serialize block");
        self.store.write(key, value).await;
    }

    fn increase_last_voted_view(&mut self, target: View) {
        self.last_voted_view = max(self.last_voted_view, target);
    }

    async fn make_vote(&mut self, block: &Block) -> Option<Vote> {
        // Check if we can vote for this block.
        let safety_rule_1 = block.view > self.last_voted_view;
        let mut safety_rule_2 = block.qc.view + 1 == block.view;
        if let Some(ref tc) = block.tc {
            let mut can_extend = tc.view + 1 == block.view;
            can_extend &= block.qc.view >= *tc.high_qc_views().iter().max().expect("Empty TC");
            safety_rule_2 |= can_extend;
        }
        if !(safety_rule_1 && safety_rule_2) {
            return None;
        }

        // Ensure we won't vote for contradicting blocks.
        self.increase_last_voted_view(block.view);
        // TODO [issue #15]: Write to storage preferred_view and last_voted_view.
        Some(Vote::new(&block, self.name, self.signature_service.clone()).await)
    }

    async fn commit(&mut self, block: Block, qc: QC) -> ConsensusResult<()> {
        if self.last_committed_view >= block.view {
            return Ok(());
        }

        let mut to_commit = VecDeque::new();
        to_commit.push_back(block.clone());

        // Ensure we commit the entire chain. This is needed after view-change.
        let mut parent = block.clone();
        while self.last_committed_view + 1 < parent.view {
            let ancestor = self
                .synchronizer
                .get_parent_block(&parent)
                .await?
                .expect("We should have all the ancestors by now");
            to_commit.push_back(ancestor.clone());
            parent = ancestor;
        }

        // Save the last committed block.
        self.last_committed_view = block.view;

        // Send all the newly committed blocks to the node's application layer.
        while let Some(block) = to_commit.pop_back() {
            debug!("Committed {:?}", block);

            // Output the block to the top-level application.
            if let Err(e) = self.tx_output.send(block.clone()).await {
                warn!("Failed to send block through the output channel: {}", e);
            }

            let mut certs = Vec::new();
            // Send the payload to the committer.
            for header in block.payload {
                let certificate = Certificate { header, votes: qc.clone().votes };
                certs.push(certificate.clone());

                self.tx_commit
                    .send(certificate)
                    .await
                    .expect("Failed to send payload");
            }

            // Cleanup the mempool.
            self.mempool_driver.cleanup(certs).await;
        }
        Ok(())
    }

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
            }
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
         self.tx_ticket
             .send((self.view, self.round))
             .await
             .expect("Failed to send view");

             
        self.tx_proposer
            .send(ProposerMessage(self.view, self.high_qc.clone(), tc))
            .await
            .expect("Failed to send message to proposer");
    }

    async fn process_qc(&mut self, qc: &QC) {
        debug!("Processing {:?}", qc);
        self.advance_view(qc.view).await;
        self.update_high_qc(qc);
    }

    async fn process_header(&mut self, payload: &Vec<Header>) -> ConsensusResult<()> {
        //TODO: Any other checks?

        //Only process special header if the round number is increasing
        ensure!(
            payload[0].round > self.round,
            ConsensusError::NonMonotonicRounds {
               round: payload[0].round,
               curr_round: self.round,
            }
        );
        self.round = payload[0].round;
        Ok(())
    }

    #[async_recursion]
    async fn process_block(&mut self, block: &Block) -> ConsensusResult<()> {
        debug!("Processing {:?}", block);

        // Let's see if we have the last three ancestors of the block, that is:
        //      b0 <- |qc0; b1| <- |qc1; block|
        // If we don't, the synchronizer asks for them to other nodes. It will
        // then ensure we process both ancestors in the correct order, and
        // finally make us resume processing this block.
        let (b0, b1) = match self.synchronizer.get_ancestors(block).await? {
            Some(ancestors) => ancestors,
            None => {
                debug!("Processing of {} suspended: missing parent", block.digest());
                return Ok(());
            }
        };

        // Store the block only if we have already processed all its ancestors.
        self.store_block(block).await;

        // Construct a certificate from block.qc (qc1)
        let certificate = Certificate {header: b1.payload[0].clone(), votes: block.qc.votes.clone()};

        // Send certificate to the DAG
        self.tx_dag
            .send(certificate)
            .await
            .expect("Failed to send payload");

        // // QC formed so send ticket to the consensus
        // // TODO: add additional check to see whether you are the leader
       
        // self.tx_ticket
        //     .send((b1.view, b1.payload[0].round))
        //     .await
        //     .expect("Failed to send view");

        // Check if we can commit the head of the 2-chain.
        // Note that we commit blocks only if we have all its ancestors.
        if b0.view + 1 == b1.view {
            self.commit(b0, b1.qc).await?;
        }

        // Ensure the block's view is as expected.
        // This check is important: it prevents bad leaders from producing blocks
        // far in the future that may cause overflow on the view number.
        if block.view != self.view {
            return Ok(());
        }

        // See if we can vote for this block.
        if let Some(vote) = self.make_vote(block).await {
            debug!("Created {:?}", vote);
            let next_leader = self.leader_elector.get_leader(self.view + 1);
            if next_leader == self.name {
                self.handle_vote(&vote).await?;
            } else {
                debug!("Sending {:?} to {}", vote, next_leader);
                let address = self
                    .committee
                    .consensus(&next_leader)
                    .expect("The next leader is not in the committee")
                    .consensus_to_consensus;
                let message = bincode::serialize(&ConsensusMessage::Vote(vote))
                    .expect("Failed to serialize vote");
                self.network.send(address, Bytes::from(message)).await;
            }
        }
        Ok(())
    }

    async fn handle_proposal(&mut self, block: &Block) -> ConsensusResult<()> {
        let digest = block.digest();

        // Ensure the block proposer is the right leader for the view.
        ensure!(
            block.author == self.leader_elector.get_leader(block.view),
            ConsensusError::WrongLeader {
                digest,
                leader: block.author,
                view: block.view
            }
        );

        // Check the block is correctly formed.
        block.verify(&self.committee)?;

        // Process the special Header. Advance round
        let res = self.process_header(&block.payload).await;

        // Process the QC. This may allow us to advance view.
        self.process_qc(&block.qc).await;

        // Process the TC (if any). This may also allow us to advance view.
        if let Some(ref tc) = block.tc {
            debug!("Processing (embedded) {:?}", tc);
            self.advance_view(tc.view).await;
        }

        // Check that the payload certificates are valid.
        self.mempool_driver.verify(&block).await?;

        // All check pass, we can process this block.
        self.process_block(block).await
    }
    
   

    async fn handle_tc(&mut self, tc: TC) -> ConsensusResult<()> {
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
                Some(message) = self.rx_message.recv() => match message {   //Receiving Messages from other Replicas
                    ConsensusMessage::Propose(block) => self.handle_proposal(&block).await,
                    ConsensusMessage::Vote(vote) => self.handle_vote(&vote).await,
                    ConsensusMessage::Timeout(timeout) => self.handle_timeout(&timeout).await,
                    ConsensusMessage::TC(tc) => self.handle_tc(tc).await,
                    _ => panic!("Unexpected protocol message")
                },
                Some(block) = self.rx_loopback.recv() => self.process_block(&block).await,  //Processing Block that we propose ourselves. (or that we resume via Synchronizer upcall)
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
