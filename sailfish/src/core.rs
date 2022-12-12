use std::convert::TryInto;
use std::net::SocketAddr;
use crate::aggregator::Aggregator;
use crate::consensus::{ConsensusMessage, View, Round};
//use crate::error::{ConsensusError, ConsensusResult};
use crate::leader::LeaderElector;
use crate::mempool::MempoolDriver;
//use crate::messages::{Header, Certificate, Block, Timeout, Vote, AcceptVote, QC, TC};
//use crate::proposer::ProposerMessage;
use crate::synchronizer::Synchronizer;
use crate::timer::Timer;
use async_recursion::async_recursion;
use bytes::Bytes;
use config::Committee;
use crypto::{Hash as _, CryptoError};
use crypto::{PublicKey, SignatureService, Digest};
use futures::FutureExt;
use log::{debug, error, warn};
use network::SimpleSender;
use primary::messages::{Header, Certificate, Timeout, AcceptVote, QC, TC, Ticket, Vote};
use primary::{ensure, error::{ConsensusError, ConsensusResult}};
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
    //tx_proposer: Sender<ProposerMessage>,
    tx_commit: Sender<Certificate>,
    tx_validation: Sender<(Header, u8, Option<QC>, Option<TC>)>,
    tx_ticket: Sender<Ticket>, //Sender<(View, Round, Ticket)>,
    rx_special: Receiver<Header>,
    rx_loopback_process_commit: Receiver<(Digest, Ticket)>,
    rx_loopback_commit: Receiver<(Header, Ticket)>,

    view: View,
    last_prepared_view: View,
    last_voted_view: View,
    last_committed_view: View,
    stored_headers: HashMap<Digest, Header>,
    high_prepare: Header,
    high_cert: Certificate,  //this is a DAG QC
    high_qc: QC,             //this is a CommitQC
    high_tc: TC,             //last TC --> just in case we need to reply with a proof. ::Note: This is not necessary if we are not proving invalidations. (For Timeout messages high_prepare suffices)
    timer: Timer,
    aggregator: Aggregator,
    network: SimpleSender,
    round: Round, 

    processing_headers: HashMap<View, HashSet<Digest>>,
    processing_certs: HashMap<View, HashSet<Digest>>,
    // process_commit: HashSet<Digest>, //Digest of the Header ready to commit -- all consensus parents are available
    // waiting_to_commit:  HashSet<Digest>, // Digest of the Header thats eligible to commit (has QC), but not ready because cert has not been processed
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
        //tx_proposer: Sender<ProposerMessage>,
        tx_commit: Sender<Certificate>,
        tx_validation: Sender<(Header, u8, Option<QC>, Option<TC>)>, // Loopback Special Headers to DAG
        tx_ticket: Sender<Ticket>, //Sender<(View, Round, Ticket)>,
        rx_special: Receiver<Header>,
        rx_loopback_process_commit: Receiver<(Digest, Ticket)>,
        rx_loopback_commit: Receiver<(Header, Ticket)>,
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
                //tx_proposer,
                tx_commit,
                tx_validation,
                tx_ticket,
                rx_special,
                rx_loopback_process_commit,
                rx_loopback_commit,

                view: 1,
                last_prepared_view: 0, 
                last_voted_view: 0,
                last_committed_view: 0,
                stored_headers: HashMap::default(),         //TODO: Can remove?
                high_prepare: Header::genesis(&committee),
                high_cert: Certificate::genesis_cert(&committee), //== first cert of Cert::genesis
                high_qc: QC::genesis(&committee),
                high_tc: TC::genesis(&committee),
                timer: Timer::new(timeout_delay),
                aggregator: Aggregator::new(committee),
                network: SimpleSender::new(),
                round: 0,
                processing_headers: HashMap::new(),
                processing_certs: HashMap::new(),
                // process_commit: HashSet::new(),
                // waiting_to_commit: HashSet::new(),
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


    fn increase_last_prepared_view(&mut self, target: View) {
        self.last_prepared_view = max(self.last_prepared_view, target);
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
    
    async fn generate_and_handle_fast_qc(&mut self, certificate: Certificate) -> ConsensusResult<> {

        //generate fast QC
        let qc: QC = QC {
            hash: certificate.header.id,
            view: certificate.header.view,
            view_round: certificate.header.round,
            votes: certificate.votes,
        };
        //TODO: Edit QC verification function to verify a fast QC (check whether votes.size = 3f+1, and if so, generate a dummy cert with special_valid = 1 and verify Certificate instead)
        // call handle_qc
        self.handle_qc(&qc).await
    }
   
    async fn generate_ticket_and_commit(&mut self, header_digest: Digest, qc: Option<QC>, tc: Option<TC>) -> ConsensusResult<Ticket> {
      
        //generate header
        //let new_ticket = Ticket::new(qc.unwrap(), tc, header_digest.clone(), self.view).await;
        
        //Create new ticket, using the associated Qc/Tc and it's view  (Important: Don't use latest view)
        let new_ticket = match tc {
            Some(timeout_cert) => Ticket::new(header_digest.clone(), timeout_cert.view.clone(), timeout_cert.view_round.clone(), self.high_qc.clone(), Some(timeout_cert)).await,
            None => {
                match qc {
                    Some(quorum_cert) => Ticket::new(header_digest.clone(), quorum_cert.view.clone(), quorum_cert.view_round.clone(), quorum_cert.clone(), None).await,
                    None => return Err(ConsensusError::InvalidTicket),
                }
            }
           
        };
        self.process_commit(header_digest, new_ticket.clone()).await?; //TODO: Can execute this asynchronously? 
        Ok(new_ticket)
    }

    async fn process_commit(&mut self, header_digest: Digest, new_ticket: Ticket) -> ConsensusResult<()> {
        //Fetch header. If not available, sync first.
        match self.synchronizer.get_commit_header(header_digest, &new_ticket).await? { 
            // If have header, call commit
           Some(header) => self.commit(header, new_ticket).await?,
            // If don't have header, start sync. On reply call commit.  (Same reply as the ticket sync reply!!) ==> both use same sync function.
           None => {debug!("don't have header");}
       }
       Ok(())
    }

    async fn commit(&mut self, header: Header, new_ticket: Ticket) -> ConsensusResult<()> {
        //TODO: Need to handle duplicate calls?
        if self.last_committed_view >= header.view {
            return Ok(());
        }

        // deliver_parent_ticket.  confirm that ticket is in store. Else create waiter to re-trigger commit. //sync on ticket header (and directly call commit on it once received)
        if !self.synchronizer.deliver_parent_ticket(&header, &new_ticket).await? {
            return Ok(());
        }

        if new_ticket.tc.is_some() { //don't commit it, just store it.
            // store the ticket.
            let bytes = bincode::serialize(&new_ticket).expect("Failed to serialize ticket");
            self.store.write(new_ticket.digest().to_vec(), bytes).await;
            return Ok(());
        }
    
        self.last_committed_view = header.view.clone();
        
        self.processing_headers.retain(|k, _| k >= &header.view); //garbage collect
        self.processing_certs.retain(|k, _| k >= &header.view); //garbage collect

        let mut to_commit = VecDeque::new();
        let mut parent = header;
        to_commit.push_back(parent.clone());

       //Commits parents too. This will only happen in case of View Changes, i.e. if the parent ticket is a TC. 
        while self.last_committed_view + 1 < parent.view {
            let ancestor:Header = self
                .synchronizer
                .get_parent_header(&parent) //TODO: Create. Lookup consensus parent ticket (should be there), and then the header from that.
                .await?
                .expect("Should have ancestor by now");
            to_commit.push_back(ancestor.clone());
            parent= ancestor;
        }

        

        let mut certs = Vec::new();
         // Send all the newly committed blocks to the node's application layer.
         while let Some(header) = to_commit.pop_back() {
            debug!("Committed {:?}", header);
            
            let cert = Certificate { header, special_valids: Vec::new(), votes: Vec::new() };
            certs.push(cert.clone());
            self.tx_commit   //Note: This is sent to the commiter->CertificateWaiter. It waits for all Dag parent certs to arrive. However, this is already guaranteed by our implicit requirement that the committing cert is processed.
                .send(cert)
                .await
                .expect("Failed to send commit cert to committer");
            
            // Cleanup the mempool.
            
        }
        self.mempool_driver.cleanup(certs).await;

        //store it store the ticket.
        let bytes = bincode::serialize(&new_ticket).expect("Failed to serialize ticket");
        self.store.write(new_ticket.digest().to_vec(), bytes).await;
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
       //TESTING 
        //return Ok(());

        warn!("Timeout reached for view {}", self.view);
        println!("timeout reached for view {}", self.view);

        // Increase the last prepared and voted view.
        self.increase_last_prepared_view(self.view);
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
        let addresses: Vec<SocketAddr> = self
            .committee
            .others_consensus(&self.name)
            .into_iter()
            .map(|(_, x)| x.consensus_to_consensus)
            .collect();
        let message = bincode::serialize(&ConsensusMessage::Timeout(timeout.clone()))
            .expect("Failed to serialize timeout message");
        println!("timeout message bytes {:?}", message.clone());
        self.network
            .broadcast(addresses.clone(), Bytes::from(message.clone()))
            .await;
        //self.network.broadcast(addresses, Bytes::from(message)).await;

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
             
            //The QC will be forwarded as part of the next proposals ticket. If we are pessimistic/we want it faster, we can also broadcast
            let pessimistic_qc_exchange = true; //TODO: Turn this into a flag.
             // Broadcast the QC. //alternatively: pass it down as ticket.
             if pessimistic_qc_exchange {
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
             }
            

            let ticket = self.generate_ticket_and_commit(qc.hash.clone(), Some(qc.clone()), None).await?;
            // Make a new special header if we are the next leader.
            if self.name == self.leader_elector.get_leader(self.view) {
                self.generate_proposal(ticket).await;
            }
        }
        Ok(())
    }



    #[async_recursion]
    async fn handle_qc(&mut self, qc: &QC) -> ConsensusResult<()> {

        //Accept QC if genesis (e.g. first Ticket contains genesis QC)
        if *qc == QC::genesis(&self.committee) {
            return Ok(());
        }

        //verify qc
        // Ensure the vote is well formed.
        if let Err(_) =  qc.verify(&self.committee){
            return Err(ConsensusError::InvalidQC(qc.clone()));
        } 
        
        // Process the QC.  Adopts view, high qc, and resets timer. //Adopt prev_view_round if higher.
        self.process_qc(qc).await;
        
        let ticket = self.generate_ticket_and_commit(qc.hash.clone(), Some(qc.clone()), None).await?;
    
         // Make a new special header if we are the next leader.
         if self.name == self.leader_elector.get_leader(self.view) {
            self.generate_proposal(ticket).await;
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
        if let Some(tc) = self.aggregator.add_timeout(timeout.clone())? {  //TODO: FIXME: Re-factor this TC to include a proposal and a proof of the proposal
            debug!("Assembled {:?}", tc);

            self.process_tc(&tc).await?;

            //The TC will be forwarded as part of the next proposals ticket. If we are pessimistic/we want it faster, we can also broadcast
            let pessimistic_qc_exchange = true; //TODO: Turn this into a flag.
              // Broadcast the QC. //alternatively: pass it down as ticket.
            if pessimistic_qc_exchange {
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
    async fn generate_proposal(&mut self, ticket: Ticket) {

        //Start new proposal: Pass down to proposer: 1) current view, 2) current Dag round, 3) latest ticket  

         //generate ticket to include Qc or TC 
        // let ticket = match tc {
        //     Some(timeout_cert) => Ticket::new(self.high_qc.clone(), Some(timeout_cert), self.view).await,
        //     None => Ticket::new(self.high_qc.clone(), None, self.view).await,
        // };

        //Don't generate proposals for old views.
        if ticket.view == self.view - 1 {  //Note: view -1 since pur next proposal should be for self.view, and thus the associate ticket is from view-1
            self.tx_ticket
            //.send((self.view, ticket.round, ticket)) 
            .send(ticket) 
            .await
            .expect("Failed to send view");
        }

       
      
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

    async fn process_ticket(&mut self, header: &Header, ticket: Ticket) -> ConsensusResult<()>{
        //Process ticket qc/tc (i.e. commit last proposal)
        match ticket.tc {
            Some(tc) => {
                // check if tc for prev view.
                ensure!(
                    header.view == tc.view + 1,
                    ConsensusError::InvalidTicket
                );
                // check if we need to process qc.
                if tc.view > self.last_committed_view { // Since we update last_committed_view only upon commit it is implied that last_committed_view <= view
                    return self.handle_tc(&tc).await; //TODO: don't do this redundantly/check for duplicates 
                }
            },
            None => {
                ensure!(
                    header.view == ticket.qc.view + 1,
                    ConsensusError::InvalidTicket
                );

                if ticket.qc.view > self.last_committed_view {
                    return self.handle_qc(&ticket.qc).await; //TODO: don't do this redundantly/check for duplicates 
                }
            }
        }
        Ok(())
    }

    //Call process_special_header upon receiving upcall from Dag layer.
    #[async_recursion]
    async fn process_special_header(&mut self, header: Header) -> ConsensusResult<()> {

        //println!("CONSENSUS: processing special header view {} certificate at replica? {}", header.view.clone(), self.name);

         //Indicate that we are processing this header.
        if !self.processing_headers
         .entry(header.view)
         .or_insert_with(HashSet::new)
         .insert(header.id.clone()){
            return Ok(());
         }


        let mut special_valid: u8 = 1;


        //A) CHECK WHETHER HEADER IS CURRENT ==> If not, reply invalid   

                //1) Check if we have already voted in this view > last prepared view
                if self.last_prepared_view >= header.view {
                    special_valid = 0;
                }
                /*ensure!(
                    header.view > self.last_prepared_view,
                    ConsensusError::TooOld(header.id, header.view)  //TODO: Still want to reply invalid in DAG. ==> Corner-case: Have already moved views, but Dag still requires cert.
                );*/
                
                //2) If we have not voted, but have already committed higher view, vote invalid
                if self.last_committed_view >= header.view {
                    special_valid = 0;
                }

                if special_valid == 0 {
                    self.tx_validation.send((header, special_valid, None, None)).await .expect("Failed to send payload");
                    return Ok(());
                }
            
                // TODO: If we have already voted for this view; or we have already voted in a previoius view for this vote round or
                // bigger ==> then don't vote at all. The proposer must be byz.
                // But if we cannot vote for this view because of a timeout; then do want to vote for Dag, but invalidate the specialness.
                // Problem: Might not have a proof yet. ==> Need to wait for it.
                // (I.e. if don't have conflict QC/TC, start a waiter)

                // NOTE: Invalid replies/Proofs thus only need to prove timeout case (since otherwise replicas would stay silent) ==> i.e. checking TC
                // (or a consecutive QC) for higher view suffices. (I.e. don't need to check round number)
        
    
        //B) CHECK HEADER CORRECTNESS ==> If not, don't need to reply. Proposer must be byz

            //1) Header signature correct => If false, dont need to reply
            if let Err(_) = header.verify(&self.committee){
                return Err(ConsensusError::InvalidHeader);
            } 

            //2) Header author == view leader
            ensure!(
                header.author == self.leader_elector.get_leader(header.view),
                 ConsensusError::WrongProposer
            );

            //3) Ensure that proposed round isnt reaching too far ahead TODO:
                // Check that header.prev_view_round is satisfied by ticket. 
                //FIXME: Technically want to do this in the DAG layer already; to avoid the DAG joining a high round that is invalid/too far ahead.
                //NOTE: DAG doesn't "join round", it just keeps track. If it receives a high round, but there is no quorum, it doesnt matter.
                    //honest proposer should fall into the trap of proposing something too high ===> can guard this by ensuring ticket vouches for round
                    // ===> transitively, ticket only exist if enough replicas checked during process_special_header that round does not exceed parents by 2
                    // i.e. check that round = max(parents+2, last_consensus_round+1);

            //4)Check if Ticket valid. If we have not processed qc/tc in ticket yet then process it.

                        // ensure!(
                        //     header.ticket.is_some(),
                        //     ConsensusError::InvalidTicket
                        // );
            if header.ticket.is_none() { //TODO: Could ignore ticket validation if we have header.consensus_parent (=ticket) in store already.
                special_valid = 0;
            }
            else{ //If there is a ticket attached
                let ticket = header.ticket.clone().unwrap();
                if let Err(e) = self.process_ticket(&header, ticket).await {
                    return Err(e);
                }
            }
                //IGNORE Comment (only true if we use Ticket as proof for validation result.)
                    // // I.e. whether have QC/TC for view v-1 
                    // // If don't have QC/TC. Start a waiter. If waiter triggers, call this function again (or directly call down validation complete)

                //5) proposed round > last committed round
                // Only process special header if the round number is increasing  // self.round >= ticket.qc.view_round since we process_qc first.
                if header.round <= self.round {
                    special_valid = 0;
                }
                // ensure!(
                //     header.round > self.round,
                //     ConsensusError::NonMonotonicRounds(header.round, self.round)
                // );
        
    
        //C) PROCESS CORRECT SPECIAL HEADER
        
            //Update latest prepared. Update latest_voted_view.
            self.view = header.view; //
            self.increase_last_prepared_view(self.view);

            let copy = header.clone();
            let id = copy.id.clone();

            self.high_prepare = copy.clone();

            self.store_header(&header).await;
            self.stored_headers.insert(id, copy);
            
                //self.round = header.round; //only update self.round upon commit(?)
                // (Committed Headers should be monotonic. Since consensus is sequential, should suffice to update it then?)
        
        //D) SEND REPLY TO THE DAG

        //Dummy proofs ==> Currently are NOT using proofs, but using timeouts.
        let qc: Option<QC> = None;
        let tc: Option<TC> = None;


        // Loopback to RB, confirming that special block is valid.
        self.tx_validation
            .send((header, special_valid, qc, tc))
            .await
            .expect("Failed to send payload");

         
        Ok(())
    }

    //NOTE: Deprecated, not used.
    #[async_recursion]
    async fn process_sync_cert(&mut self, certificate: Certificate) -> ConsensusResult<()> {
        //call down to Dag layer.
        //self.tx_pushdown_cert.send(certificate).await.expect("Failed to pushdown certificate to Dag layer");
        //TODO: or should sync itself call down to Dag layer, and let consensus only registers a waiter. --> this would avoid redundantly syncing on the same cert twice. ==> Note: This is what we do now, but we sync on headers.
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
      
        //println!("process special cert for view {} at replica {}", certificate.header.view.clone(), self.name);
    
         //1) Check if cert is still relevant to current view, if so, update view and high_cert. Ignore cert if we've already committed in the round.
         ensure!(
            certificate.header.view >= self.view && certificate.header.view > self.last_committed_view,
            ConsensusError::TooOld(certificate.header.id, certificate.header.view)
        );

        //2)  Don't vote twice on a cert; 
        ensure!(
            certificate.header.view > self.last_voted_view,
            ConsensusError::AlreadyVoted(certificate.header.id, certificate.header.view)
        );
         

         //Indicate that we are processing this header. (don't need this unless we want to avoid duplicates)
         //self.processing_certs.entry(certificate.header.round).or_insert_with(HashSet::new).insert(certificate.header.id.clone());

        //process_header if we have not already -> this will process the ticket (Note: This is not strictly necessary, but it's useful to call early to avoid having to sync on consensus_parent_ticket later)
        if !self.processing_headers.get(&certificate.header.round).map_or_else(|| false, |x| x.contains(&certificate.header.id))
        { // This function may still throw an error if the storage fails.
            self.process_special_header(certificate.header.clone()).await?;
        }


        //  //Ensure our certificate was logged, i.e. it passed Dag process_certificate, and thus the invariant for Dag parents availability is fulfilled. 
        //  //If not, synchronizer will fetch it and trigger re-processing of the current cert
        // if !self.synchronizer.deliver_self(&certificate).await? {
        //     debug!(
        //         "Processing of {:?} suspended: missing consensus ancestors",
        //          certificate
        //     );
        //     self.process_sync_cert(certificate.clone());
        //     //return Ok(());
        // }

        // //Ensure we have the consensus parent of this certificate. If not, the synchronizer will fetch it and trigger re-processing of the current cert.
        // if !self.synchronizer.deliver_consensus_ancestor(&certificate).await? {
        //     debug!(
        //         "Processing of {:?} suspended: missing consensus ancestors",
        //          certificate
        //     );
        //     return Ok(());
        // }


        let header = certificate.clone().header;
        let id = header.clone().id;

        self.advance_view(header.view);
        self.increase_last_voted_view(header.view);

        self.high_cert = certificate.clone();
        self.stored_headers.insert(id.clone(), header.clone());
        self.store_cert(&certificate).await;


        //2) Fast Path:
        if certificate.is_special_fast(&self.committee){
            self.generate_and_handle_fast_qc(certificate).await;
            return Ok(());
        }

        //3) Slow Path: Send out AcceptVote

        let vote = AcceptVote::new(&header, self.name, self.signature_service.clone()).await;  
        debug!("Created {:?}", vote.digest());
        
        let next_leader = self.leader_elector.get_leader(self.view + 1);
        //println!("should handle vote, {}, {}", next_leader, self.name);
        if next_leader == self.name {
            self.handle_accept_vote(&vote).await?;
        } else {
            debug!("Sending {:?} to {}", vote.digest(), next_leader);
            let address = self
                    .committee
                    .consensus(&next_leader)
                    .expect("The next leader is not in the committee")
                    .consensus_to_consensus;
            //println!("address sending is {}", address);
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
        //tc.verify(&self.committee)?; //FIXME: Added this because I didnt' see any TC validation elsewhere. If there is already, remove this.
        if let Err(_) =  tc.verify(&self.committee){
            return Err(ConsensusError::InvalidTC(tc.clone()));
        } 

        self.process_tc(tc).await
    }

    async fn process_tc(&mut self, tc: &TC) -> ConsensusResult<()> {
        
        //TODO: FIXME:
        //Set high_prepare/high_cert/high_qc to the content of the TC

        debug!("Processing {:?}", tc);
        self.advance_view(tc.view).await;

        let ticket = self.generate_ticket_and_commit(tc.hash.clone(), None, Some(tc.clone())).await?;
        
        // Make a new special header if we are the next leader.
        if self.name == self.leader_elector.get_leader(self.view) {
            self.generate_proposal(ticket).await;
        }
        
        Ok(())
    }

    pub async fn run(&mut self) {
        // Upon booting, generate the very first block (if we are the leader).
        // Also, schedule a timer in case we don't hear from the leader.
        self.timer.reset();
        if self.name == self.leader_elector.get_leader(self.view) {
            self.generate_proposal(Ticket::genesis(&self.committee)).await; //Starts a new proposal for view = 1, with genesis ticket from view = 0; prev_round = 0
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

                Some((header_dig, ticket)) = self.rx_loopback_process_commit.recv() => self.process_commit(header_dig, ticket).await,
                Some((header, ticket)) = self.rx_loopback_commit.recv() => self.commit(header, ticket).await,
                
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
