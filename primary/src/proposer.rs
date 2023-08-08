use std::collections::BTreeMap;
use std::mem;

// Copyright(C) Facebook, Inc. and its affiliates.
use crate::messages::{Certificate, Header, Ticket, ConsensusInfo, ConsensusProposal};
use crate::primary::{Height, View};
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
    rx_core: Receiver<Certificate>,
    /// Receives the batches' digests from our workers.
    rx_workers: Receiver<(Digest, WorkerId)>,
    // Receives new view from the Consensus engine
    rx_ticket: Receiver<Ticket>, //Receiver<(View, Round, Ticket)>,
    // Receives certificates from core to determine parallel chain growth
    rx_certs: Receiver<Certificate>,
    /// Sends newly created headers to the `Core`.
    tx_core: Sender<Header>,
   
    /// The current round of the dag.
    height: Height,
    // Holds the header id of the last header issued.
    last_header_id: Digest,
    last_header_round: Height,
    // Whether the last header was special
    is_last_header_special: bool,
    // Whether the proposer received a ticket
    has_received_ticket: bool,

    /// Holds the last parent certificate used to propose header
    last_parent: Option<Certificate>,
    // Holds the consensus info for the last special header
    last_consensus_info: ConsensusInfo,
    // Holds the last consensus proposal info
    last_consensus_proposal: ConsensusProposal,
    // Most recent certificates used for consensus proposals
    current_certs: BTreeMap<PublicKey, Certificate>,
    /// Holds the batches' digests waiting to be included in the next header.
    digests: Vec<(Digest, WorkerId)>,
    /// Keeps track of the size (in bytes) of batches' digests that we received so far.
    payload_size: usize,
    // The current view from consensus
    view: View,
    // The round proposed by the last view in consensus.
    prev_view_round: Height,
    //prev_view_header: Option<Digest>,

    // Whether to propose special block
    consensus_propose_special: bool,
    // Whether the previous block had enough parents. If yes, can issue special block without. If not, then special block must wait for parents.
    last_has_parents: bool,
    // Whether to include special edge or not.
    use_special_parent: bool,

    ticket: Option<Ticket>,

    committee: Committee,
}

impl Proposer {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        name: PublicKey,
        committee: Committee,
        signature_service: SignatureService,
        header_size: usize,
        max_header_delay: u64,
        rx_core: Receiver<Certificate>,
        rx_workers: Receiver<(Digest, WorkerId)>,
        rx_ticket: Receiver<Ticket>,//Receiver<(View, Round, Ticket)>,
        rx_certs: Receiver<Certificate>,
        tx_core: Sender<Header>,
    ) {
        /*let genesis: Vec<Digest> = Certificate::genesis(&committee)
            .iter()
            .map(|x| x.digest())
            .collect();*/

        let genesis = Certificate::genesis_cert(&committee);

        // let genesis_parent: Digest = Certificate {
        //         header: Header {
        //             author: name,
        //             ..Header::default()
        //         },
        //         ..Certificate::default()
        //     }.digest();

        // let genesis_parent: Digest = Header {
        //         author: name,
        //         ..Header::default()
        //     }.digest();
      
        let genesis_parent: Digest = Header::genesis(&committee).id;  //Note: This should never be necessary to use. 
                                                                        // Two cases: A) first header is special => it will use genesis_parents. B) special is not first header =>last_header_id references a previous proposal

        tokio::spawn(async move {
            Self {
                name,
                signature_service,
                header_size,
                max_header_delay,
                rx_core,
                rx_workers,
                rx_ticket,
                rx_certs,
                tx_core,
                height: 1,
                last_parent: Some(genesis),
                last_consensus_info: ConsensusInfo { slot: 0, view: 0 },
                last_consensus_proposal: ConsensusProposal { ticket: genesis.digest(), proposals: BTreeMap::new() },
                current_certs: BTreeMap::new(),
                is_last_header_special: false,
                last_header_id: genesis_parent, //Digest::default(),
                last_header_round: 0,
                digests: Vec::with_capacity(2 * header_size),
                payload_size: 0,
                view: 0,
                prev_view_round: 0,
                //prev_view_header: None,
                consensus_propose_special: false,
                last_has_parents: true,
                use_special_parent: false,
                has_received_ticket: false,
                ticket: None,
                committee,
            }
            .run()
            .await;
        });
    }

    pub fn get_leader(&self, view: View) -> PublicKey {
        let mut keys: Vec<_> = self.committee.authorities.keys().cloned().collect();
        keys.sort();
        keys[view as usize % self.committee.size()]
        //keys[0]
    }
    
    async fn make_header(&mut self, is_special_propose: bool, is_special_consensus: bool) {
      
        let mut ticket = None;
        let mut ticket_digest = Digest::default();
        if is_special_propose && self.ticket.is_some() {
            mem::swap(&mut self.ticket, &mut ticket); //Reset self.ticket; and move ticket (this just avoids copying)
            ticket_digest = ticket.as_ref().unwrap().digest();
            debug!("PROPOSER: make special block for view {}, round {} at replica? {}. Ticket round: {}", self.view, self.height, self.name, ticket.as_ref().unwrap().round);
        }
        else {
            debug!("PROPOSER: make normal block for round {} at replica? {}", self.height, self.name);
        }

        let mut consensus_propose_info: Option<ConsensusProposal> = None;
        let mut consensus_info: Option<ConsensusInfo> = None;

        if is_special_propose {
            consensus_propose_info = Some(ConsensusProposal { ticket: ticket_digest, proposals: self.current_certs.clone() });
            consensus_info = Some(ConsensusInfo { slot: self.last_consensus_info.slot+1, view: 1 });
        }

        if is_special_consensus {
            consensus_info = Some(self.last_consensus_info.clone());
        }

        //let mut prev_view_header = None;
        //mem::swap(&mut self.prev_view_header, &mut prev_view_header);  //TODO: Currently unused = just None. Need to pass a prev_view_header digest as part of ticket channel. (or include it inside ticket => preferred)
        // Make a new header.
        let header = Header::new(
                self.name,
                self.height,
                self.digests.drain(..).collect(),
                self.last_parent.clone().unwrap(),
                &mut self.signature_service,
                is_special_propose,
                consensus_info,
                consensus_propose_info,
                Some(self.view),
                if self.use_special_parent {Some(self.last_header_id.clone())} else {None},
                Some(self.last_header_round),
                ticket,
                None,
                self.prev_view_round,
                if is_special_propose {Some(ticket_digest)} else {None},//prev_view_header,
            ).await;

        debug!("Created {:?}", header);

        #[cfg(feature = "benchmark")]
        for digest in header.payload.keys() {
            // NOTE: This log entry is used to compute performance.
            info!("Created {} -> {:?}", header, digest);
        }

        //Store reference to our own previous header -- used for special edge. Does not include a certificate (does not exist yet)
        self.last_header_id = header.digest();
        self.last_header_round = header.height.clone(); ////self.round.clone();
       
        // Reset last parent
        self.last_parent = None;
      
        // Send the new header to the `Core` that will broadcast and process it.
        self.tx_core
        .send(header)
        .await
        .expect("Failed to send header");
    }

    // Main loop listening to incoming messages.
    pub async fn run(&mut self) {
        debug!("Dag starting at round {}", self.height);

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
            //let enough_parents = !self.last_parent.is_empty();
            let enough_parents = self.last_parent.is_some();
            let enough_digests = self.payload_size >= self.header_size;
            let timer_expired = timer.is_elapsed();

            // We will always propose a header (special propose, special, or normal) when we have a
            // certificate from previous height and a batch ready/timeout
            if (timer_expired || enough_digests) && enough_parents {
                let mut special_consensus_proposals = false;

                if self.has_received_ticket {
                    // Propose special header with proposals
                    let mut num_increase_growth = 0;
                    for (pub_key, certificate) in self.current_certs.iter() {
                        if certificate.height > self.last_consensus_proposal.proposals.get(pub_key).unwrap().height {
                            num_increase_growth += 1;
                        }
                    }

                    special_consensus_proposals = num_increase_growth >= self.committee.quorum_threshold();
                } 

                // Consensus header (without proposals) contains a prepare/commit certificate
                let special_consensus = self.last_parent.clone().unwrap().special_valids.iter().all(|x| *x);

                self.height += 1;
                // Make new header (special is depending on whether we have a ticket)
                self.make_header(special_consensus_proposals, special_consensus).await;
                self.payload_size = 0;

                // Reschedule the timer
                let deadline = Instant::now() + Duration::from_millis(self.max_header_delay);
                timer.as_mut().reset(deadline);


                //Case A: Have Digests OR Timer: Receive Ticket Before parents. ==> next loop will start a special header
                //Case B: Have Digests OR Timer: Receive Parents before ticket. ==> next loop will start a normal header
                //Case C: Waiting for Digest AND Timer: Received both Ticket and Parent. ==> next loop will start a special header (with parents)

               
                //Pass down round from last cert. Also pass down current parents EVEN if not full quorum: I.e. add a special flag to "process_cert" to not wait.
                /*if self.propose_special && self.last_has_parents { //2nd clause: Special block must have, or extend block with n-f edges ==> cant have back to back special edges
                
                    debug!("enough_digests. special? {:?}, last_parents? {:?}, enough parents? {:?}", self.propose_special, self.last_has_parents, enough_parents);
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
                    // Update that the last header was special
                    self.is_last_header_special = true;
                    //reset
                    self.use_special_parent = false;
                    self.propose_special = false; 
                
                    proposing = true;
                }
                else if enough_parents {
                    debug!("enough_digests. special? {:?}, last_parents? {:?}, enough parents? {:?}", self.propose_special, self.last_has_parents, enough_parents);
                    // If not special and enough parents available make a new normal header.
                    self.make_header(false).await;
                    self.last_has_parents = true;

                    proposing = true;
                }
                // else{
                //     debug!("Cannot propose. Even though enough_digests/timer. special? {:?}, last_parents? {:?}, enough parents? {:?}", self.propose_special, self.last_has_parents, enough_parents);
                // }*/
            }
          
            /*if proposing {
                self.payload_size = 0;
                // Reschedule the timer.
                let deadline = Instant::now() + Duration::from_millis(self.max_header_delay);
                timer.as_mut().reset(deadline);
            }*/
           

            tokio::select! {
    
                 //Passing Ticket view AND round, i.e. round that the special Header in view belonged to. Skip to that round if lagging behind (to guarantee special headers are monotonically growing)
                Some(ticket) = self.rx_ticket.recv() => { //TODO: Remove view and round and pass just ticket ==> view = ticket.view+1. round = ticket.round
                    debug!("   received ticket with view {:?}, and last round {:?}", ticket.view, ticket.round);
                    if ticket.view < self.view {
                        continue; //Proposal for view already exists.
                    }
                    if false && ticket.round >= self.height { //catch up to last special block round --> to ensure special headers are monotonic in rounds
                        self.height = ticket.round+1; 
                    }
                    else if false && self.last_header_round == self.height { //if last special block round is smaller then our current round, increment round normally. Only increment if we have not done so already (e.g. edges have been received)
                        self.height = self.height +1;
                    }
                    
                    
                    //TODO: Note: Need a defense mechanism to ensure Byz proposer cannot arbitrarily exhaust round space. Maybe reject voting on headers that are too far ahead (avoid forming ticket)
                    // ==> This is implicitly solved by requiring normal blocks to have n-f parents. Byz proposer cannot issue ticket for high round without n-f total replicas being in that round.
                    //TODO: Ticket must include view_round ==> QC already does. (TC must be edited) ==> just 

                    self.view = ticket.view+1;
                    self.prev_view_round = ticket.round;
                    //if self.get_leader(self.view) == self.name {
                    if self.height > self.prev_view_round { //if we've caught up to special round.
                        self.consensus_propose_special = true;
                    }
                        self.ticket = Some(ticket);
                   // }
                    debug!("Dag moved to round {}, and view {}. propose_special {}, has_ticket {}", self.height, self.view, self.consensus_propose_special, self.ticket.is_some());
    
                }

                // Receive a ticket from consensus to propose a new special header with proposals

                // Receive own certificate from core (we are the author)
                Some(parent) = self.rx_core.recv() => {
                    debug!("   received parents from height {:?}", parent.height);

                    if parent.height < self.height {
                        continue;
                    }

                    // Signal that we have enough parent certificates to propose a new header.
                    self.last_parent = Some(parent.clone());
                    self.current_certs.insert(parent.origin(), parent);
                }

                // Receive certificate with a different author
                Some(cert) = self.rx_certs.recv() => {
                    debug!("   received certificate from height {:?}", cert.height);
                    if cert.height > self.current_certs.get(&cert.origin()).unwrap().height {
                        self.current_certs.insert(cert.origin(), cert.clone());
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
