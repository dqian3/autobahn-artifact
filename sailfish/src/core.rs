use std::convert::TryInto;
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
use primary::messages::{Header, Certificate, Timeout, AcceptVote, QC, TC, Ticket};
use primary::error::{ConsensusError, ConsensusResult};
//use primary::messages::AcceptVote;
use std::cmp::max;
use std::collections::{VecDeque, HashSet};
use std::collections::HashMap;
use store::Store;
use tokio::sync::mpsc::{Receiver, Sender};
use ed25519_dalek::Sha512;
use ed25519_dalek::Digest as _;


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
    rx_loopback_header: Receiver<Header>,
    rx_loopback_cert: Receiver<Certificate>,
    tx_pushdown_cert: Sender<Certificate>,
    tx_proposer: Sender<ProposerMessage>,
    tx_commit: Sender<Certificate>,
    tx_validation: Sender<(Header, u8, Option<QC>, Option<TC>)>,
    tx_ticket: Sender<(View, Round, Ticket)>,
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

    processing_headers: HashMap<View, HashSet<Digest>>,
    processing_certs: HashMap<View, HashSet<Digest>>,
    ready_to_commit: HashSet<Digest>, //Digest of the Header ready to commit -- all consensus parents are available
    waiting_to_commit:  HashSet<Digest>, // Digest of the Header thats eligible to commit (has QC), but not ready because cert has not been processed
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
        rx_loopback_header: Receiver<Header>,
        rx_loopback_cert: Receiver<Certificate>,
        tx_pushdown_cert: Sender<Certificate>,
        tx_proposer: Sender<ProposerMessage>,
        tx_commit: Sender<Certificate>,
        tx_validation: Sender<(Header, u8, Option<QC>, Option<TC>)>, // Loopback Special Headers to DAG
        tx_ticket: Sender<(View, Round, Ticket)>,
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
                rx_loopback_header,
                rx_loopback_cert,
                tx_pushdown_cert,
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
                processing_headers: HashMap::new(),
                processing_certs: HashMap::new(),
                ready_to_commit: HashSet::new(),
                waiting_to_commit: HashSet::new(),
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

    async fn store_cert(&mut self, cert: &Certificate) {
        let header = cert.header.clone();
        let mut hasher = Sha512::new();
        hasher.update(&header.id); //== parent_header.id
        hasher.update(&header.round().to_le_bytes());
        hasher.update(&header.origin()); //parent_header.origin = child_header_origin
        let cert_digest = crypto::Digest(hasher.finalize().as_slice()[..32].try_into().unwrap());

        let key = cert_digest.to_vec();
        let value = bincode::serialize(cert).expect("Failed to serialize cert");
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
    
    //TODO: Also commit TC's in the ticket chain. 
    // Note: currently the parent pointer points to a digest of a cert. process_cert recursively guarantees that all parent certs are stored.
    // Problem: a TC ticket does not necessarily have a cert (e.g. it may just commit a header that has only been prepared by some replicas)
    // To solve this, handle_tc should create and store a dummy cert
    // ==> To ensure handle_tc is called, must ensure that process_special_header is called
    // Conclusion: commit should call process_cert; process_cert should call process_special_header (if not already done)
    // I.e. example workflow: S1 (TC) <- S2 (QC)
    // S2 commit is called. -> S2 process_cert is called. -> S2 process_special_header is called. -> validates & processes ticket, e.g. calls handle_tc(S1.TC)
    // -> generates and stores cert(S1) -> S2 process_cert resumes. checks that parent cert is present. -> S2 process_cert concludes --> S2 commit should wake up

    // Problem: What wakes S2 commit? S2 commit should wake when its own cert is in store. But that is true, because Dag process_cert adds it, not Consensus process_cert
    // either: add some data structure that wakes commit (e.g set "commit_waiting", and if true while calling process_cert, call commit)
    //or: in Dag layer, don't add to store before parent is there

    
    async fn commit(&mut self, header: Header) -> ConsensusResult<()> {
        //TODO: Need to handle duplicate calls?

        if self.last_committed_view >= header.view {
            return Ok(());
        }
         
        // check process_cert for own cert has been called ==> I.e. this guarantees Invariant: All consensus parent certs are there. If not, resume processing later
        // if not, stop processing; set a marker that commit is waiting.
        self.waiting_to_commit.insert(header.id.clone());
        if !self.ready_to_commit.contains(&header.id)
        {
            let dummy_cert = Certificate {
                header,
                ..Certificate::default()
            };
            self.process_special_certificate(dummy_cert).await?;
            return Ok(()); //will resume later.
        }

        
        let mut to_commit = VecDeque::new();


        let mut parent_cert = Certificate {
            header: header.clone(),
            ..Certificate::default()
        };
        to_commit.push_back(parent_cert.clone());

       
        while self.last_committed_view + 1 < parent_cert.header.view {
            let ancestor_cert = self
                .synchronizer
                .get_parent_cert(&parent_cert)
                .await?
                .expect("We should have all the consensus ancestor certificates by now");
            to_commit.push_back(ancestor_cert.clone());
            parent_cert = ancestor_cert;
        }

        self.last_committed_view = header.view;
        
        self.processing_headers.retain(|k, _| k >= &header.view); //garbage collect
        self.processing_certs.retain(|k, _| k >= &header.view); //garbage collect

        let mut certs = Vec::new();
         // Send all the newly committed blocks to the node's application layer.
         while let Some(cert) = to_commit.pop_back() {
            debug!("Committed {:?}", cert.header);
            self.ready_to_commit.remove(&cert.header.id);
            self.waiting_to_commit.remove(&cert.header.id);

            
            certs.push(cert.clone());
            self.tx_commit   //Note: This is sent to the commiter->CertificateWaiter. It waits for all Dag parent certs to arrive. However, this is already guaranteed by our implicit requirement that the committing cert is processed.
                .send(cert)
                .await
                .expect("Failed to send commit cert to committer");
            
            // Cleanup the mempool.
            
        }
        self.mempool_driver.cleanup(certs).await;
        Ok(())
    }

////////////////////////// Old:

    //     let mut to_commit = VecDeque::new();
    //     to_commit.push_back(header.clone());
    //     // Ensure we commit the entire chain in order. This is needed after view-change, or if the replica
    //     // receives qc's out of order (e.g. it was lagging for a while, or some channels are slower/async).
    //     let mut parent = header.clone();
      
    //     while self.last_committed_view + 1 < parent.view {
          
    //         let ancestor = self
    //             .synchronizer
    //             .get_parent_header(&parent)  //TODO: Define this function. It should make sure that all previous views are committed. Must retrieve all QC/TC for prior views. TODO: FIXME: Header must reference either the ticket (QC/TC)
    //             .await?
    //             .expect("We should have all the ancestors by now");
    //         to_commit.push_back(ancestor.clone());
    //         parent = ancestor;
    //     }
    //     //TODO: Is it guaranteed that for each of these headers, all parent edges have already been added to committer?
    //     // ==> if a header is a parent there must have been a QC for it => n-f replicas called process_special_header at consensus level
    //     // => each must have been a cert for it
    //     // but this current replica may or may not have seen that cert yet. //TODO: Ensure that primary/core/process_cert is called...
    //     // (may need synchronizer)

    //     // Save the last committed block.
    //     self.last_committed_view = header.view;
        
    //     self.processing_headers.retain(|k, _| k >= &header.view); //garbage collect
    //     self.processing_certs.retain(|k, _| k >= &header.view); //garbage collect

    //     // Send all the newly committed blocks to the node's application layer.
    //     while let Some(header) = to_commit.pop_back() {
    //         debug!("Committed {:?}", header);
    //         self.ready_to_commit.remove(&header.id);
    //         self.waiting_to_commit.remove(&header.id);

    //         // // Output the Header to the top-level application. //FIXME: This should be after flattening.
    //         // if let Err(e) = self.tx_output.send(block.clone()).await {
    //         //     warn!("Failed to send block through the output channel: {}", e);
    //         // }

    //         let mut certs = Vec::new();
    //         // Send the payload to the committer.

    //         let certificate = self
    //             .synchronizer
    //             .get_cert(&header)
    //             .await?
    //             .expect("we should have certificate by now");

    //         // FIXME: change special_valids to be a valid not None ?
    //         //FIXME: The cert for the header is not enough => need to ensure that all edges of this header have certs in the committer
    //         //TODO: Need to sync on all edges if not already stored ==> this needs to be able to call back into the DAG layer to call process_cert (might need to sync to get that real cert first.)
    //         //let certificate = Certificate { header, special_valids: Vec::new(), votes: qc.clone().votes }; //TODO: I don't think these votes are ever used. Just create empty vec.
    //         certs.push(certificate.clone());

    //         self.tx_commit   //Note: This is sent to the commiter/CertificateWaiter
    //             .send(certificate)
    //             .await
    //             .expect("Failed to send payload");
            

    //         // Cleanup the mempool.
    //         self.mempool_driver.cleanup(certs).await;
    //     }
    //     Ok(())
    // }


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

    /*#[async_recursion]
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
    }*/

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

    #[async_recursion]
    async fn handle_qc(&mut self, qc: &QC) -> ConsensusResult<()> {
        //verify qc
        // Ensure the vote is well formed.
        qc.verify(&self.committee)?;
        
        // Process the QC.  Adopts view, high qc, and resets timer. //Adopt prev_view_round if higher.
        self.process_qc(qc).await;
        
        //TODO: Should only commit QC's in order. Must wait / request all intermediary QC's
        //upcall to app layer: Order dag, and execute.


        let header: Option<&Header> = self.stored_headers.get(&qc.hash);

        if header.is_some() {
            self.commit(header.unwrap().clone()).await?;
        }
        //FIXME: SHould we be returning if none? Re-submit for commit some time later?

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

    // QC or TC formed so send ticket to the consensus
    // check to see whether you are the leader is implicit
    #[async_recursion]
    async fn generate_proposal(&mut self, tc: Option<TC>) {


        //TODO: Pass down as part of ticket or header the digest of the previously committed header. 
        //(Either store it as part of QC/TC -> then its implicit and it does not need to be passed)
        //(Or store locally self.ticket_header as last)  --> Note: We cannot just use last committed, because a TC may propose a header that only gets committed upon next QC (yet the TC proposal should count as ticket)

         
         //pass down Qc or TC for ticket
        let ticket = match tc {
            Some(timeout_cert) => Ticket::new(self.high_qc.clone(), Some(timeout_cert), self.view).await,
            None => Ticket::new(self.high_qc.clone(), None, self.view).await,
        };
        /*if self.high_qc.view + 1 == self.view {
            ticket = Ticket::new(Some(self.high_qc.clone()), None, self.view).await;
        }
        else if tc.is_some() && tc.as_ref().unwrap().view +1 == self.view {  //tc.is_some_and(|&tc| (tc.view +1 == self.view)) {
            ticket = Ticket::new(None, tc, self.view).await; 
        }
        else {
            return
        }*/
       
         self.tx_ticket
             .send((self.view, self.round, ticket))
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
        self.round = max(self.round, qc.view_round);
    }

    //Call process_special_header upon receiving upcall from Dag layer.
    #[async_recursion]
<<<<<<< HEAD
    async fn process_special_header(&mut self, header: Header) -> ConsensusResult<()> {
       
        if self.last_committed_view >= header.view {
            self.tx_validation.send((header, 0, None, None)).await.expect("Failed to send payload");
            return Ok(()); //TODO: change to invalid = 0 and reply.
        }

        //Indicate that we are processing this header.
        self.processing_headers
            .entry(header.round)
            .or_insert_with(HashSet::new)
            .insert(header.id.clone());

        
=======
    async fn process_header(&mut self, header: Header) -> ConsensusResult<()> {
>>>>>>> b184f3ff3f5f6b2dbf5357f622830d0a7873c6a1
        //0) TODO: Check if Ticket valid. If we have not processed ticket yet. Do so.

        //FIXME: If no TC present, must check that QC is for the preceeding view
        //FIXME: Currently always checks if TC present, and returns Error if not => shouldn't do this.

        ensure!(
            header.ticket.is_some(),
            ConsensusError::InvalidTicket
        );
        let ticket = header.ticket.clone().unwrap();
        /*if ticket.qc.view > self.last_committed_view {
            self.handle_qc(&ticket.qc).await?;
        }*/

        match ticket.tc.clone() {
            Some(tc) => {
                // check if tc for prev view.
                ensure!(
                    header.view == tc.view + 1,
                    ConsensusError::InvalidTicket
                );
                // check if we need to process qc.
                if tc.view > self.last_committed_view {
                    self.handle_tc(&tc).await?;
                }
            },
            None => {
                ensure!(
                    header.view == ticket.qc.view + 1,
                    ConsensusError::InvalidTicket
                );

                if ticket.qc.view > self.last_committed_view {
                    self.handle_qc(&ticket.qc).await?;
                }
            }
        };

        /*match ticket.qc {
            Some(qc) => {
               // check if qc for prev view.
               ensure!(
                    header.view == qc.view + 1,
                    ConsensusError::InvalidTicket
                );
               // check if we need to process qc.
               if qc.view > self.last_committed_view { // //TODO:  Update last_committed_view only upon commit. //implies that last_committed_view <= view
                self.handle_qc(&qc).await?; //TODO: don't do this redundantly/check for duplicates --> + TODO: HandleQC directly, but defer Commit upcall until until parent qc has been handled. 
                                                    //TODO: Important: Process_header should still complete, since validation needs to be asynchronous --> it suffices to check that the ticket QC is correct.
               }
                
            },
            None => {
                match ticket.tc {
                    Some(tc) => {
                        // check if tc for prev view.
                        ensure!(
                            header.view == tc.view + 1,
                            ConsensusError::InvalidTicket
                        );
                        // check if we need to process qc.
                        if tc.view > self.last_committed_view {
                            self.handle_tc(&tc).await?;
                        }
                    },
                    None => return Err(ConsensusError::InvalidTicket)
                }
            }
        }*/

        // I.e. whether have QC/TC for view v-1 
        //If don't have QC/TC. Start a waiter. If waiter triggers, call this function again (or directly call down validation complete)

        //1) view > last voted view
        ensure!(
            header.view > self.last_voted_view,
            ConsensusError::TooOld(header.id, header.view)
        );

        // TODO: If we have already voted for this view; or we have already voted in a previoius view for this vote round or
        // bigger ==> then don't vote at all. The proposer must be byz.
        // But if we cannot vote for this view because of a timeout; then do want to vote for Dag, but invalidate the specialness.
        // Problem: Might not have a proof yet. ==> Need to wait for it.
        // (I.e. if don't have conflict QC/TC, start a waiter)

        // NOTE: Proofs thus only need to prove timeout case (since otherwise replicas would stay silent) ==> i.e. checking TC
        // (or a consecutive QC) for higher view suffices. (I.e. don't need to check round number)

        // 2) round > last round
        // Only process special header if the round number is increasing  // self.round >= ticket.qc.view_round since we
        // process_qc first.
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


        //6) Update latest prepared. Update latest_voted_view.
        self.view = header.view; //
        self.increase_last_voted_view(self.view);

        let copy = header.clone();
        let id = copy.id.clone();

        self.high_prepare = copy.clone();

        self.store_header(&header).await;
        self.stored_headers.insert(id, copy);
       
        //self.round = header.round; //TODO: FIXME: only update self.round upon commit(?)
        // (Committed Headers should be monotonic. Since consensus is sequential, should suffice to update it then?)
        
       
        //FIXME: Dummy testing with validation == true.
        let special_valid: u8 = 1;
        //let qc: Option<QC> = Some(ticket.qc);
        //let tc: Option<TC> = None;

        // Loopback to RB, confirming that special block is valid.
        self.tx_validation
            .send((header, special_valid, None, None))
            .await
            .expect("Failed to send payload");

         
        Ok(())
    }

    #[async_recursion]
    async fn process_sync_cert(&mut self, certificate: Certificate) -> ConsensusResult<()> {
        //call down to Dag layer.
        self.tx_pushdown_cert.send(certificate).await.expect("Failed to pushdown certificate to Dag layer");
        //TODO: or should sync itself call down to Dag layer, and let consensus only registers a waiter. --> this would avoid redundantly syncing on the same cert twice.
        //For now don't optimize -- this only affects consensus parents
        // Flow: 
        //       1. Register waiter but send no SyncRequest
        //       2. Downcall sync request to Dag  (change pushdown_cert to request_cert_sync(Digest))
        //       3. Have Dag call synchonizer.sync_consensus_parent
        //       4. this should call header_waiter::SyncParents (but modify to not include header and not start a waiter) . Check parent_requests (this avoids duplicates)
        //       5. Send Cert request, Receive reply, process_cert ==> this will wake waiter 

        Ok(())
    }

    //Call process_special_header upon receiving upcall from Dag layer.
    #[async_recursion]
    async fn process_special_certificate(&mut self, certificate: Certificate) -> ConsensusResult<()> {
       
        if self.last_committed_view >= certificate.header.view {
            return Ok(());
        }

         //Indicate that we are processing this header. --> doing this after invariant is true.
         self.processing_certs
         .entry(certificate.header.round)
         .or_insert_with(HashSet::new)
         .insert(certificate.header.id.clone());

        if !self
        .processing_headers
        .get(&certificate.header.round)
        .map_or_else(|| false, |x| x.contains(&certificate.header.id))
        {
            // This function may still throw an error if the storage fails.
            self.process_special_header(certificate.header.clone()).await?;
        }

         //TODO: Make it so process_cert also waits for ones own cert in store.

         //Ensure our certificate was logged, i.e. it passed Dag process_certificate, and thus the invariant for Dag parents availability is fulfilled. 
         //If not, synchronizer will fetch it and trigger re-processing of the current cert
        if !self.synchronizer.deliver_self(&certificate).await? {
            debug!(
                "Processing of {:?} suspended: missing consensus ancestors",
                 certificate
            );
            self.process_sync_cert(certificate);
            return Ok(());
        }

        //Ensure we have the consensus parent of this certificate. If not, the synchronizer will fetch it and trigger re-processing of the current cert.
        if !self.synchronizer.deliver_consensus_ancestor(&certificate).await? {
            debug!(
                "Processing of {:?} suspended: missing consensus ancestors",
                 certificate
            );
            return Ok(());
        }


        let header = certificate.clone().header;
        let id = header.clone().id;

        //1) Check if cert is still relevant to current view, if so, update view and high_cert
        ensure!(
            header.view >= self.view,
            ConsensusError::TooOld(header.id, header.view)
        );
        self.advance_view(header.view);
        self.increase_last_voted_view(header.view);

<<<<<<< HEAD
        self.high_cert = certificate;
        self.stored_headers.insert(id.clone(), header.clone());
=======
        self.high_cert = certificate.clone();
        self.stored_headers.insert(id, header.clone());
        self.store_cert(&certificate).await;

>>>>>>> b184f3ff3f5f6b2dbf5357f622830d0a7873c6a1

        //2) Set marker that process_cert is complete.
        self.ready_to_commit.insert(id.clone());

        // check marker whether commit is ready. If yes, call commit. If no, send vote.
        if self.waiting_to_commit.contains(&id) {
            self.commit(header).await?;
            return Ok(());
        }

        //3) Send out Vote

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
    //     let res = self.process_special_header(&block.payload[0], &block.author).await;

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
    
   

    async fn handle_tc(&mut self, tc: &TC) -> ConsensusResult<()> {
        tc.verify(&self.committee)?; //FIXME: Added this because I didnt' see any TC validation elsewhere. If there is already, remove this.

        debug!("Processing {:?}", tc);
        self.advance_view(tc.view).await;
        if self.name == self.leader_elector.get_leader(self.view) {
            self.generate_proposal(Some(tc.clone())).await;
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
                Some(header) = self.rx_special.recv() => self.process_special_header(header).await,
                //2) DAG layer certs to vote on
                Some(certificate) = self.rx_consensus.recv() => self.process_special_certificate(certificate).await, // This creates an Accept Vote (rename vote to accept?)  should come via proposer?
                
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
                    ConsensusMessage::TC(tc) => self.handle_tc(&tc).await,
                    //7) Sync Headers
                    ConsensusMessage::Header(header) => self.process_special_header(header).await,  //TODO: Sanity check that this is fine. ==> Sync Headers will also cause a vote at the Dag layer to be generated.
                    //8) Sync Headers
                    ConsensusMessage::Certificate(cert) => self.process_sync_cert(cert).await,  //TODO: Sanity check that this is fine. ==> Sync Headers will also cause a vote at the Dag layer to be generated.
                    _ => panic!("Unexpected protocol message")
                },

                Some(header) = self.rx_loopback_header.recv() => self.process_special_header(header).await,  //Processing Header that we resume via Synchronizer upcall
                Some(cert) = self.rx_loopback_cert.recv() => self.process_special_certificate(cert).await,  //Processing Header that we resume via Synchronizer upcall
                
                //NO LONGER NEEDED: Some(block) = self.rx_loopback_header.recv() => self.process_block(&block).await,  //Processing Block that we propose ourselves. (or that we resume via Synchronizer upcall)
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
