use crate::committer::Committer;
use crate::core::Core;
use crate::error::ConsensusError;
use crate::helper::Helper;
use crate::leader::LeaderElector;
use crate::mempool::MempoolDriver;
use crate::messages::{Block, Timeout, Vote, TC};
use crate::proposer::Proposer;
use crate::synchronizer::Synchronizer;
use async_trait::async_trait;
use bytes::Bytes;
use config::{Committee, Parameters};
use crypto::{Digest, PublicKey, SignatureService};
use futures::SinkExt as _;
use log::info;
use network::{MessageHandler, Receiver as NetworkReceiver, Writer};
use primary::{Certificate, Header};
use serde::{Deserialize, Serialize};
use std::error::Error;
use store::Store;
use tokio::sync::mpsc::{channel, Receiver, Sender};

#[cfg(test)]
#[path = "tests/consensus_tests.rs"]
pub mod consensus_tests;

/// The default channel capacity for each channel of the consensus.
pub const CHANNEL_CAPACITY: usize = 1_000;

/// The consensus view number.
pub type View= u64;
/// The Dag round number.
pub type Round= u64;

#[derive(Serialize, Deserialize, Debug)]
pub enum ConsensusMessage {
    Propose(Block),
    Vote(Vote),
    Timeout(Timeout),
    TC(TC),
    SyncRequest(Digest, PublicKey),
}

pub struct Consensus;

impl Consensus {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        name: PublicKey,
        committee: Committee,
        parameters: Parameters,
        signature_service: SignatureService,
        store: Store,
        rx_mempool: Receiver<Certificate>,
        tx_mempool: Sender<Certificate>,
        tx_output: Sender<Block>,
        tx_ticket: Sender<(View, Round)>,
        tx_validation: Sender<Header>,
        rx_sailfish: Receiver<Header>,
    ) {
        // NOTE: This log entry is used to compute performance.
        parameters.log();

        let (tx_consensus, rx_consensus) = channel(CHANNEL_CAPACITY);
        let (tx_loopback, rx_loopback) = channel(CHANNEL_CAPACITY);
        let (tx_proposer, rx_proposer) = channel(CHANNEL_CAPACITY);
        let (tx_helper, rx_helper) = channel(CHANNEL_CAPACITY);
        let (tx_commit, rx_commit) = channel(CHANNEL_CAPACITY);
        let (tx_mempool_copy, rx_mempool_copy) = channel(CHANNEL_CAPACITY);
        //let (tx_sailfish, rx_sailfish) = channel(CHANNEL_CAPACITY);
        //let (tx_dag, rx_dag) = channel(CHANNEL_CAPACITY);

        // Spawn the network receiver.
        let mut address = committee
            .consensus(&name)
            .expect("Our public key is not in the committee")
            .consensus_to_consensus;
        address.set_ip("0.0.0.0".parse().unwrap());
        NetworkReceiver::spawn(
            address,
            /* handler */
            ConsensusReceiverHandler {
                tx_consensus,
                tx_helper,
            },
        );
        info!(
            "Node {} listening to consensus messages on {}",
            name, address
        );

        // Make the leader election module.
        let leader_elector = LeaderElector::new(committee.clone());

        // Make the mempool driver.
        let mempool_driver = MempoolDriver::new(committee.clone(), tx_mempool);

        // Make the synchronizer.
        let synchronizer = Synchronizer::new(
            name,
            committee.clone(),
            store.clone(),
            tx_loopback.clone(),
            parameters.sync_retry_delay,
        );

        // Spawn the consensus core.
        Core::spawn(
            name,
            committee.clone(),
            signature_service.clone(),
            store.clone(),
            leader_elector,
            mempool_driver,
            synchronizer,
            parameters.timeout_delay,
            /* rx_message */ rx_consensus,
            rx_loopback,
            tx_proposer,
            tx_commit,
            tx_output,
            tx_validation,
            tx_ticket,
        );

        // Commits the mempool certificates and their sub-dag.
        Committer::spawn(
            committee.clone(),
            store.clone(),
            parameters.gc_depth,
            rx_mempool_copy,
            rx_commit,
        );

        // Spawn the block proposer.
        Proposer::spawn(
            name,
            committee.clone(),
            signature_service,
            /* rx_consensus */ rx_sailfish,
            rx_mempool,
            /* rx_message */ rx_proposer,
            tx_loopback,
            tx_mempool_copy,
        );

        // Spawn the helper module.
        Helper::spawn(committee, store, /* rx_requests */ rx_helper);
    }
}

/// Defines how the network receiver handles incoming primary messages.
#[derive(Clone)]
struct ConsensusReceiverHandler {
    tx_consensus: Sender<ConsensusMessage>,
    tx_helper: Sender<(Digest, PublicKey)>,
}

#[async_trait]
impl MessageHandler for ConsensusReceiverHandler {
    async fn dispatch(&self, writer: &mut Writer, serialized: Bytes) -> Result<(), Box<dyn Error>> {
        // Deserialize and parse the message.
        match bincode::deserialize(&serialized).map_err(ConsensusError::SerializationError)? {
            ConsensusMessage::SyncRequest(missing, origin) => self
                .tx_helper
                .send((missing, origin))
                .await
                .expect("Failed to send consensus message"),
            message @ ConsensusMessage::Propose(..) => {
                // Reply with an ACK.
                let _ = writer.send(Bytes::from("Ack")).await;

                // Pass the message to the consensus core.
                self.tx_consensus
                    .send(message)
                    .await
                    .expect("Failed to consensus message")
            }
            message => self
                .tx_consensus
                .send(message)
                .await
                .expect("Failed to consensus message"),
        }
        Ok(())
    }
}
