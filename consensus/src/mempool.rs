use crate::core::RoundNumber;
use crate::error::{ConsensusError, ConsensusResult};
use crate::messages::Block;
use crate::messages::Payload;
use crypto::Digest;
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot;

#[derive(Debug)]
pub enum PayloadStatus {
    Accept,
    Reject,
    Wait,
}

#[derive(Debug)]
pub enum ConsensusMempoolMessage {
    Get(usize, oneshot::Sender<(Digest, Payload)>),
    Verify(Box<Block>, oneshot::Sender<PayloadStatus>),
    Cleanup(Vec<Digest>, RoundNumber),
}

pub struct MempoolDriver {
    mempool_channel: Sender<ConsensusMempoolMessage>,
}

impl MempoolDriver {
    pub fn new(mempool_channel: Sender<ConsensusMempoolMessage>) -> Self {
        Self { mempool_channel }
    }

    pub async fn get(&mut self, max: usize) -> (Digest, Payload) {
        let (sender, receiver) = oneshot::channel();
        let message = ConsensusMempoolMessage::Get(max, sender);
        self.mempool_channel  //Send to mempool
            .send(message)
            .await
            .expect("Failed to send message to mempool");
        //(Digest::default(), Payload::default())
        let res = receiver
            .await
            .expect("Failed to receive payload from mempool");
        res
    }

    pub async fn verify(&mut self, _block: Block) -> ConsensusResult<bool> {
        Ok(true)  //No need to verify. Blocks contain payload now.

        // let (sender, receiver) = oneshot::channel();
        // let message = ConsensusMempoolMessage::Verify(Box::new(block), sender);
        // self.mempool_channel
        //     .send(message)
        //     .await
        //     .expect("Failed to send message to mempool");
        // match receiver
        //     .await
        //     .expect("Failed to receive payload status from mempool")
        // {
        //     PayloadStatus::Accept => Ok(true),
        //     PayloadStatus::Reject => bail!(ConsensusError::InvalidPayload),
        //     PayloadStatus::Wait => Ok(false),
        // }
    }

    pub async fn cleanup(&mut self, b0: &Block, b1: &Block, b2: &Block, block: &Block) {
        let digests = vec![b0.digest.clone(), b1.digest.clone(), b2.digest.clone(), block.digest.clone()];
        let message = ConsensusMempoolMessage::Cleanup(digests, block.round);
        self.mempool_channel
            .send(message)
            .await
            .expect("Failed to send message to mempool");
    }
}
