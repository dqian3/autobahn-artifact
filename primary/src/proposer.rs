// Copyright(C) Facebook, Inc. and its affiliates.
use crate::messages::{Certificate, Header};
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
    rx_ticket: Receiver<(View, Round)>,
    /// Sends newly created headers to the `Core`.
    tx_core: Sender<Header>,
    // Sends new special headers to HotStuff.
    tx_sailfish: Sender<Header>,

    /// The current round of the dag.
    round: Round,
    // Holds the certificate id of the last header issued.
    //last_header: Digest,
    /// Holds the certificates' ids waiting to be included in the next header.
    last_parents: Vec<Digest>,
    /// Holds the batches' digests waiting to be included in the next header.
    digests: Vec<(Digest, WorkerId)>,
    /// Keeps track of the size (in bytes) of batches' digests that we received so far.
    payload_size: usize,
    // The current view from consensus
    view: Round,
    // Whether to propose special block
    propose_special: bool,
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
        rx_ticket: Receiver<(View, Round)>,
        tx_core: Sender<Header>,
        tx_sailfish: Sender<Header>
    ) {
        let genesis = Certificate::genesis(committee)
            .iter()
            .map(|x| x.digest())
            .collect();

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
                tx_sailfish,
                round: 1,
                last_parents: genesis,
                //last_header: Digest::new(),
                digests: Vec::with_capacity(2 * header_size),
                payload_size: 0,
                view: 1,
                propose_special: false,
            }
            .run()
            .await;
        });
    }

    async fn make_header(&mut self, is_special: bool) {
        // Make a new header.
        let header = Header::new(
                self.name,
                self.round,
                self.digests.drain(..).collect(),
                self.last_parents.drain(..).collect(),
                &mut self.signature_service,
            ).await;

        debug!("Created {:?}", header);

        #[cfg(feature = "benchmark")]
        for digest in header.payload.keys() {
            // NOTE: This log entry is used to compute performance.
            info!("Created {} -> {:?}", header, digest);
        }


        if !is_special {
            // Send the new header to the `Core` that will broadcast and process it.
            self.tx_core
                .send(header)
                .await
                .expect("Failed to send header");
        } else {
            // Send the new header to the `HotStuff` module who will be in charge of broadcasting it
            self.tx_sailfish
                .send(header)
                .await
                .expect("Failed to send header");
        }
        //self.last_header = header //getDigest(header)
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
                //not waiting for parent edges to create special block -- 
                //TODO: Want to use available parent edges. AT LEAST own edge.
                //FIXME: Must include at least own edge. CURRENTLY NOT THE CASE. last_parents is fully empty.
                //FIXME: Currently re-uses the same last round. Needs to increment round. If waiting for parents, then by default round is incremented so nothing to do.
                //E.g. round + 1; last_parents, insert parents.
                //Pass down round from last cert. Also pass down current parents EVEN if not full quorum: I.e. add a special flag to "process_cert" to not wait.
                if self.propose_special && enough_parents { //TODO: FOR NOW  also waiting for parent edges -- Replace later. 
                   // self.last_parents.push(self.last_header); //TODO: need to get a digest of previous headers cert... Impossible? That header has no cert yet.. would need to wait
                    self.make_header(true).await;
                    self.propose_special = false; //reset 
                    proposing = true;
                }
                else if enough_parents {
                    // If not special and enough parents available make a new normal header.
                    self.make_header(false).await;
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
                Some((view, round)) = self.rx_ticket.recv() => {
                    if view <= self.view {
                        continue; //Proposal for view already exists.
                    }
                    if round > self.round { //catch up to last special block round --> to ensure special headers are monotonic in rounds
                        self.round = round; //should be round+1?
                    }
                    //TODO: Note: Need a defense mechanism to ensure Byz proposer cannot arbitrarily exhaust round space. Maybe reject voting on headers that are too far ahead (avoid forming ticket)

                    self.view = view;
                    self.propose_special = true;
                }

                Some((parents, round)) = self.rx_core.recv() => {
                    if round < self.round {
                        continue;
                    }

                    // Advance to the next round.
                    self.round = round + 1;
                    debug!("Dag moved to round {}", self.round);

                    // Signal that we have enough parent certificates to propose a new header.
                    self.last_parents = parents;
                }
                Some((digest, worker_id)) = self.rx_workers.recv() => {
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
