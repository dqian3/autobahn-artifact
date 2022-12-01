use crate::consensus::{Round, CHANNEL_CAPACITY};
//use crate::error::{ConsensusError, ConsensusResult};
use primary::error::{ConsensusError, ConsensusResult};
use config::Committee;
use crypto::Hash as _;
use crypto::{Digest, PublicKey};
use ed25519_dalek::Digest as _;
use ed25519_dalek::Sha512;
use futures::future::try_join_all;
use futures::stream::FuturesOrdered;
use futures::stream::StreamExt as _;
use log::{debug, error, info, log_enabled};
use primary::Certificate;
use primary::messages::Header;
use std::cmp::max;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use store::Store;
use tokio::sync::mpsc::{channel, Receiver, Sender};

/// The representation of the DAG in memory.
type Dag = HashMap<Round, HashMap<PublicKey, (Digest, Certificate)>>;

/// The state that needs to be persisted for crash-recovery.
struct State {
    /// The last committed round.
    last_committed_round: Round,
    // Keeps the last committed round for each authority. This map is used to clean up the dag and
    // ensure we don't commit twice the same certificate.
    last_committed: HashMap<PublicKey, Round>,
    /// Keeps the latest committed certificate (and its parents) for every authority. Anything older
    /// must be regularly cleaned up through the function `update`.
    dag: Dag,
}

impl State {
    fn new(genesis: Vec<Certificate>) -> Self {
        let genesis = genesis
            .into_iter()
            .map(|x| (x.origin(), (x.digest(), x)))
            .collect::<HashMap<_, _>>();

        Self {
            last_committed_round: 0,
            last_committed: genesis.iter().map(|(x, (_, y))| (*x, y.round())).collect(),
            dag: [(0, genesis)].iter().cloned().collect(),
        }
    }

    /// Update and clean up internal state base on committed certificates.
    fn update(&mut self, certificate: &Certificate, gc_depth: Round) {
        self.last_committed
            .entry(certificate.origin())
            .and_modify(|r| *r = max(*r, certificate.round()))
            .or_insert_with(|| certificate.round());

        let last_committed_round = *self.last_committed.values().max().unwrap();
        self.last_committed_round = last_committed_round;

        for (name, round) in &self.last_committed {
            self.dag.retain(|r, authorities| {
                authorities.retain(|n, _| n != name || r >= round);
                !authorities.is_empty() && r + gc_depth >= last_committed_round
            });
        }
    }
}

pub struct Committer {
    gc_depth: Round,
    rx_mempool: Receiver<Certificate>,
    rx_deliver: Receiver<Certificate>,
    tx_output: Sender<Header>,
    genesis: Vec<Certificate>,
}

impl Committer {
    pub fn spawn(
        committee: Committee,
        store: Store,
        gc_depth: Round,
        rx_mempool: Receiver<Certificate>,
        rx_commit: Receiver<Certificate>,
        tx_output: Sender<Header>,
    ) {
        let (tx_deliver, rx_deliver) = channel(CHANNEL_CAPACITY);

        tokio::spawn(async move {
            CertificateWaiter::spawn(store, rx_commit, tx_deliver);
        });

        tokio::spawn(async move {
            Self {
                gc_depth,
                rx_mempool,
                rx_deliver,
                tx_output,
                genesis: Certificate::genesis(&committee),
            }
            .run()
            .await;
        });
    }

    async fn run(&mut self) {
        // The consensus state (everything else is immutable).
        let mut state = State::new(self.genesis.clone());

        loop {
            tokio::select! {
                Some(certificate) = self.rx_mempool.recv() => {
                    // Add the new certificate to the local storage.
                    state.dag.entry(certificate.round()).or_insert_with(HashMap::new).insert(
                        certificate.origin(),
                        (certificate.digest(), certificate.clone()),
                    );
                },
                Some(certificate) = self.rx_deliver.recv() => {
                    debug!("Processing {:?}", certificate);

                    // Ensure we didn't already order this certificate.
                    if let Some(r) = state.last_committed.get(&certificate.origin()) {
                        if r >= &certificate.round() {
                            continue;
                        }
                    }

                    // Flatten the sub-dag referenced by the certificate.
                    let mut sequence = Vec::new();
                    for x in self.order_dag(&certificate, &state) {
                        // Update and clean up internal state.
                        state.update(&x, self.gc_depth);

                        // Add the certificate to the sequence.
                        sequence.push(x);
                    }

                    // Log the latest committed round of every authority (for debug).
                    if log_enabled!(log::Level::Debug) {
                        for (name, round) in &state.last_committed {
                            debug!("Latest commit of {}: Round {}", name, round);
                        }
                    }

                    // Print the committed sequence in the right order.
                    for certificate in sequence {
                        info!("Committed {}", certificate.header);

                        #[cfg(feature = "benchmark")]
                        for digest in certificate.header.payload.keys() {
                            // NOTE: This log entry is used to compute performance.
                            info!("Committed {} -> {:?}", certificate.header, digest);
                        }

                         // Output the block to the top-level application.
                        if let Err(e) = self.tx_output.send(certificate.header).await {
                            info!("Failed to send block through the output channel: {}", e);
                        }
                    }
                }
            }
        }
    }

    /// Flatten the dag referenced by the input certificate. This is a classic depth-first search (pre-order):
    /// https://en.wikipedia.org/wiki/Tree_traversal#Pre-order
    fn order_dag(&self, tip: &Certificate, state: &State) -> Vec<Certificate> {
        debug!("Processing sub-dag of {:?}", tip);
        let mut ordered = Vec::new();
        let mut already_ordered = HashSet::new();

        let dummy = (Digest::default(), Certificate::default());
    

        let mut buffer = vec![tip];
        while let Some(x) = buffer.pop() {
            debug!("Sequencing {:?}", x);
            ordered.push(x.clone());

            for parent in &x.header.parents.clone() {

                let parent_digest;
                let round;

                parent_digest = parent;
                round = x.round() -1;

                let (digest, certificate) = match state
                    .dag
                    .get(&(round))                                           // returns Some(HashMap<key, value>)
                    .map(|x| x.values().find(|(x, _)| x == parent_digest))   // x := Some(key, value); where key = pubkey, value = (dig, cert) ==> maps to Some(value)
                    .flatten()                                               // result is something like Some(<Some(value)>)? => Flatten gets rid of outer Some
                {
                    Some(x) => x,
                    None => {
                        debug!("We already processed and cleaned up {}", parent_digest);
                        continue; // We already ordered or GC up to here.
                    }
                };

                // We skip the certificate if we (1) already processed it or (2) we reached a round that we already
                // committed for this authority.
                let mut skip = already_ordered.contains(&digest);
                skip |= state
                    .last_committed
                    .get(&certificate.origin())
                    .map_or_else(|| false, |r| r == &certificate.round());   //stop if last committed = the round we'd evaluate next
                if !skip {
                    buffer.push(certificate);
                    already_ordered.insert(digest);
                }
                
            }

            if x.header.is_special && x.header.special_parent.is_some() { // i.e. is special edge ==> manually hack the digest (only works because of requirement that header is from same node in prev round)
                //Currently we can skip rounds. Header needs to include parent round to solve this.
                //Note: process_header verifies that author and rounds are correct.

                //generate digest of dummy cert
                let mut hasher = Sha512::new();
                hasher.update(&x.header.special_parent.as_ref().unwrap()); //== parent_header.id
                hasher.update(&x.header.special_parent_round.to_le_bytes()); 
                hasher.update(&x.header.origin()); //parent_header.origin = child_header_origin
                let parent_digest = Digest(hasher.finalize().as_slice()[..32].try_into().unwrap());

                let round = x.header.special_parent_round;

                let mut skip: bool = false; 
                
    

                let (digest, certificate) = match state
                    .dag
                    .get(&(round))                                           // returns Some(HashMap<key, value>)
                    .map(|x| x.values().find(|(x, _)| x == &parent_digest))   // x := Some(key, value); where key = pubkey, value = (dig, cert) ==> maps to Some(value)
                    .flatten()                                               // result is something like Some(<Some(value)>)? => Flatten gets rid of outer Some
                {
                    Some(x) => x,
                    None => {
                        debug!("We already processed and cleaned up {}", parent_digest);
                        skip = true; // We already ordered or GC up to here.
                        &dummy
                    }
                };

                // We skip the certificate if we (1) already processed it or (2) we reached a round that we already
                // committed for this authority.
                skip |= already_ordered.contains(&digest);
                skip |= state
                    .last_committed
                    .get(&certificate.origin())
                    .map_or_else(|| false, |r| r == &certificate.round());   //stop if last committed = the round we'd evaluate next
                if !skip {
                    buffer.push(certificate);
                    already_ordered.insert(digest);
                }
            }

        }

        // Ensure we do not commit garbage collected certificates.
        ordered.retain(|x| x.round() + self.gc_depth >= state.last_committed_round);

        // Ordering the output by round is not really necessary but it makes the commit sequence prettier.
        ordered.sort_by_key(|x| x.round());
        ordered
    }
}

 //TODO: Create a sync call to request sync at the dag layer
                    
/// Waits to receive all the ancestors of a certificate before sending it through the output
/// channel. The outputs are in the same order as the input (FIFO).
pub struct CertificateWaiter {
    /// The persistent storage.
    store: Store,
    /// Receives input certificates.
    rx_input: Receiver<Certificate>,
    /// Outputs the certificates once we have all its parents.
    tx_output: Sender<Certificate>,
}

impl CertificateWaiter {
    pub fn spawn(store: Store, rx_input: Receiver<Certificate>, tx_output: Sender<Certificate>) {
        tokio::spawn(async move {
            Self {
                store,
                rx_input,
                tx_output,
            }
            .run()
            .await
        });
    }

    /// Helper function. It waits for particular data to become available in the storage
    /// and then delivers the specified header.
    async fn waiter(
        mut missing: Vec<(Vec<u8>, Store)>,
        deliver: Certificate,
    ) -> ConsensusResult<Certificate> {
        let waiting: Vec<_> = missing
            .iter_mut()
            .map(|(x, y)| y.notify_read(x.to_vec()))
            .collect();

        try_join_all(waiting)
            .await
            .map(|_| deliver)
            .map_err(ConsensusError::from)
    }

    async fn run(&mut self) {
        let mut waiting = FuturesOrdered::new();
        loop {
            tokio::select! {
                Some(certificate) = self.rx_input.recv() => {
                    // Skip genesis' children.
                    if certificate.round() == 1 {
                        self.tx_output.send(certificate).await.expect("Failed to send certificate");
                        continue;
                    }

                    debug!("Waiting for history of {:?}", certificate);

                    // Add the certificate to the waiter pool. The waiter will return it to us
                    // when all its parents are in the store.
                    let mut wait_for: Vec<(Vec<u8>, Store)>= certificate
                        .header
                        .parents
                        .iter()
                        .cloned()
                        .map(|x| (x.to_vec(), self.store.clone()))
                        .collect();

                     //Add a waiter for the special parent header.
                     if certificate.header.special_parent.is_some(){
                        let special_parent = certificate
                        .header
                        .special_parent
                        .clone().unwrap();
                        //create dummy digest
                        let mut hasher = Sha512::new();
                        hasher.update(&special_parent); //== parent_header.id
                        hasher.update(&certificate.header.special_parent_round.to_le_bytes()); 
                        hasher.update(&certificate.header.origin()); //parent_header.origin = child_header_origin
                        let special_wait_for = Digest(hasher.finalize().as_slice()[..32].try_into().unwrap());
                        wait_for.push(  (special_wait_for.to_vec(), self.store.clone())  );
                     }

                    let fut = Self::waiter(wait_for, certificate);
                    waiting.push_back(fut);
                }
                Some(result) = waiting.next() => match result {
                    Ok(certificate) => {
                        debug!("Got all the history of {:?}", certificate);
                        self.tx_output.send(certificate).await.expect("Failed to send certificate");
                    },
                    Err(e) => {
                        error!("{}", e);
                        panic!("Storage failure: killing node.");
                    }
                },
            }
        }
    }
}
