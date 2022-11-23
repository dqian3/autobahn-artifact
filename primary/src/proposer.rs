use std::mem;

// Copyright(C) Facebook, Inc. and its affiliates.
use crate::messages::{Certificate, Header, Ticket};
use crate::primary::{Round, View};
use config::{Committee, WorkerId};
use crypto::Hash as _;
use crypto::{Digest, PublicKey, SignatureService};
use log::debug;
#[cfg(feature = "benchmark")]
use log::info;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{sleep, Duration, Instant};

#[cfg(test)]
#[path = "tests/proposer_tests.rs"]
pub mod proposer_tests;

/// The proposer creates new headers and send them to the core for broadcasting and further processing.
pub struct Proposer {
    /// The public key of this primary.
    name: PublicKey,
    /// Service to sign headers.
    signature_service: SignatureService,
    /// The size of the headers' payload.
    header_size: usize,
    /// The maximum delay to wait for batches' digests.
    max_header_delay: u64,

    /// Receives the parents to include in the next header (along with their round number).
    rx_core: Receiver<(Vec<Digest>, Round)>,
    /// Receives the batches' digests from our workers.
    rx_workers: Receiver<(Digest, WorkerId)>,
    // Receives new view from the Consensus engine
    rx_ticket: Receiver<(View, Round, Ticket)>,
    /// Sends newly created headers to the `Core`.
    tx_core: Sender<Header>,
   
    /// The current round of the dag.
    round: Round,
    // Holds the header id of the last header issued.
    last_header_id: Digest,
    last_header_round: Round,

    /// Holds the certificates' ids waiting to be included in the next header.
    last_parents: Vec<Digest>,
    /// Holds the batches' digests waiting to be included in the next header.
    digests: Vec<(Digest, WorkerId)>,
    /// Keeps track of the size (in bytes) of batches' digests that we received so far.
    payload_size: usize,
    // The current view from consensus
    view: View,
    // The round proposed by the last view in consensus.
    prev_view_round: Round,
    prev_view_header: Option<Digest>,

    // Whether to propose special block
    propose_special: bool,
    // Whether the previous block had enough parents. If yes, can issue special block without. If not, then special block must wait for parents.
    last_has_parents: bool,
    // Whether to include special edge or not.
    use_special_parent: bool,

    ticket: Option<Ticket>,
}

impl Proposer {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        name: PublicKey,
        committee: &Committee,
        signature_service: SignatureService,
        header_size: usize,
        max_header_delay: u64,
        rx_core: Receiver<(Vec<Digest>, Round)>,
        rx_workers: Receiver<(Digest, WorkerId)>,
        rx_ticket: Receiver<(View, Round, Ticket)>,
        tx_core: Sender<Header>,
    ) {
        let genesis: Vec<Digest> = Certificate::genesis(committee)
            .iter()
            .map(|x| x.digest())
            .collect();

        // let genesis_parent: Digest = Certificate {
        //         header: Header {
        //             author: name,
        //             ..Header::default()
        //         },
        //         ..Certificate::default()
        //     }.digest();

        let genesis_parent: Digest = Header {
                author: name,
                ..Header::default()
            }.digest();
      

        tokio::spawn(async move {
            Self {
                name,
                signature_service,
                header_size,
                max_header_delay,
                rx_core,
                rx_workers,
                rx_ticket,
                tx_core,
                round: 1,
                last_parents: genesis,
                last_header_id: genesis_parent, //Digest::default(),
                last_header_round: 1,
                digests: Vec::with_capacity(2 * header_size),
                payload_size: 0,
                view: 0,
                prev_view_round: 1,
                prev_view_header: None,
                propose_special: false,
                last_has_parents: true,
                use_special_parent: false,
                ticket: None,
            }
            .run()
            .await;
        });
    }

    
    async fn make_header(&mut self, is_special: bool) {
        println!("make block. special? {}", is_special);

        let mut ticket = None;
        mem::swap(&mut self.ticket, &mut ticket); //Reset self.ticket; and move ticket (this just avoids copying)
        let mut prev_view_header = None;
        mem::swap(&mut self.prev_view_header, &mut prev_view_header);
        // Make a new header.
        let header = Header::new(
                self.name,
                self.round,
                self.digests.drain(..).collect(),
                self.last_parents.drain(..).collect(),
                &mut self.signature_service,
                is_special,
                self.view,
                if self.use_special_parent {Some(self.last_header_id.clone())} else {None},
                self.last_header_round,
                ticket, 
                self.prev_view_round,
                prev_view_header,
            ).await;

        debug!("Created {:?}", header);

        #[cfg(feature = "benchmark")]
        for digest in header.payload.keys() {
            // NOTE: This log entry is used to compute performance.
            info!("Created {} -> {:?}", header, digest);
        }

        //Store reference to our own previous header -- used for special edge. Does not include a certificate (does not exist yet)
        self.last_header_id = header.digest();
        self.last_header_round = self.round.clone();
      
        // Send the new header to the `Core` that will broadcast and process it.
        self.tx_core
        .send(header)
        .await
        .expect("Failed to send header");
    }

    // Main loop listening to incoming messages.
    pub async fn run(&mut self) {
        debug!("Dag starting at round {}", self.round);

        let timer = sleep(Duration::from_millis(self.max_header_delay));
        tokio::pin!(timer);

        loop {
            // Check if we can propose a new header. We propose a new header when one of the following
            // conditions is met:
            // 1. We have a quorum of certificates from the previous round and enough batches' digests;
            // 2. We have a quorum of certificates from the previous round and the specified maximum
            // inter-header delay has passed.
            // 3. If it is a special block opportunity. That is when either a QC or TC from the previous view forms,
            // we have a ticket to propose a new block
            // For both normal blocks and special blocks, delegate the actual sending to the consensus module
            // in other words core should not be disseminating headers
            let enough_parents = !self.last_parents.is_empty();
            let enough_digests = self.payload_size >= self.header_size;
            let timer_expired = timer.is_elapsed();

            let mut proposing = false;

            if timer_expired || enough_digests {

                debug!("enough_digests. special? {:?}, last_parents? {:?}, enough parents? {:?}", self.propose_special, self.last_has_parents, enough_parents);
                //Case A: Have Digests OR Timer: Receive Ticket Before parents. ==> next loop will start a special header
                //Case B: Have Digests OR Timer: Receive Parents before ticket. ==> next loop will start a normal header
                //Case C: Waiting for Digest AND Timer: Received both Ticket and Parent. ==> next loop will start a special header (with parents)
               
                //Pass down round from last cert. Also pass down current parents EVEN if not full quorum: I.e. add a special flag to "process_cert" to not wait.
                if self.propose_special && (self.last_has_parents || enough_parents) { //2nd clause: Special block must have, or extend block with n-f edges ==> cant have back to back special edges
                
                    //not waiting for n-f parent edges to create special block -- but will use them if available (Note, this will only happen in case C)
                    //Includes at least own parent as special edge.

                    if enough_parents { // (Note, this will only be true in case C -- or if the previous header was special and used a special edge)
                        self.last_has_parents = true;
                    }
                    else{  
                        self.last_has_parents = false ;
                        self.use_special_parent = true; 
                        //self.last_parents.push(self.last_header_id.clone()); //Use last header as special edge. //TODO: Consensus should process parents. Distinguish whether n-f edges, or just 1 edge (special)
                                                                                                                    // If n-f: Do the same Synchronization/availability checks that the DAG does.
                                                                                                                    // If 1: Check if DAG has that payload. If yes, vote. If no, block processing and wait.
                                                                                                               //TODO: if want to simplify, can always use just 1 edge for special blocks.
                    } 
                    //TODO: Special case: Have enough parents, but they are from an older round than the latest header? ==> as far as I can tell Impossible:
                     //if we received parents before ticket for last header, then latest header wouldve included them
                    //if we received ticket for last header first, then we currently reject the parents, because they are for a smaller round   ( //should we keep rejecting the parents, or is there a benefit to using them?)
                    // ==> Conclusion: If we have enough parents, and we receive a ticket for a view that is skipping ahead, then those parents must be for a round >= than our last header. Thus we should use them.

                    self.make_header(true).await;
                    //reset
                    self.use_special_parent = false;
                    self.propose_special = false; 
                    


                    proposing = true;
                }
                else if enough_parents {
                    // If not special and enough parents available make a new normal header.
                    self.make_header(false).await;
                    self.last_has_parents = true;

                    proposing = true;
                }
            }
          
            if proposing {
                self.payload_size = 0;
                // Reschedule the timer.
                let deadline = Instant::now() + Duration::from_millis(self.max_header_delay);
                timer.as_mut().reset(deadline);
            }
           

            tokio::select! {
    
                 //Passing Ticket view AND round, i.e. round that the special Header in view belonged to. Skip to that round if lagging behind (to guarantee special headers are monotonically growing)
                Some((view, round, ticket)) = self.rx_ticket.recv() => {
                    debug!("   received ticket with view {:?}, and last round {:?}", view, round);
                    if view <= self.view {
                        continue; //Proposal for view already exists.
                    }
                    if round > self.round { //catch up to last special block round --> to ensure special headers are monotonic in rounds
                        self.round = round+1; //should be round+1?
                    }
                    //TODO: Note: Need a defense mechanism to ensure Byz proposer cannot arbitrarily exhaust round space. Maybe reject voting on headers that are too far ahead (avoid forming ticket)

                    self.view = view;
                    self.prev_view_round = round;
                    self.propose_special = true;
                    self.ticket = Some(ticket);
                    debug!("Dag moved to round {}, and view {}. propose_special {}", self.round, self.view, self.propose_special);
    
                }

                Some((parents, round)) = self.rx_core.recv() => {
                    debug!("   received parents from round {:?}", round);
                    if self.propose_special && !self.last_has_parents {   // If we are trying to propose a special block, but cannot because not enough parents are ready, and we've already exhausted last_parent_header rule
                        if round < self.last_header_round { // i.e. these are parents for a round that we already issued.
                            continue;
                        }
                        self.last_parents = parents;
                        if round >= self.round {
                            self.round = round + 1; // ==> special round must be bigger than parent round
                        }
                        //Note: If round = last_header round, then this logic is equivalent to the normal case below; but if we skipped rounds, then its possible that round > last_header_round; in which case we do want to use the parents.
                    }

                    else{ //normal case
                        if round < self.round {
                            continue;
                        }
    
                        // Advance to the next round.
                        self.round = round + 1;
                        debug!("Dag moved to round {}", self.round);
    
                        // Signal that we have enough parent certificates to propose a new header.
                        self.last_parents = parents;
                    }
                   
                }
                Some((digest, worker_id)) = self.rx_workers.recv() => {
                    println!("   received payload from worker {}", worker_id);
                    self.payload_size += digest.size();
                    self.digests.push((digest, worker_id));
                }
                () = &mut timer => {
                    // Nothing to do.
                }
            }
        }
    }
}
