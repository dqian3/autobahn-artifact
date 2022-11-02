use crate::consensus::{ConsensusMessage, View};
use crate::messages::{Block, QC, TC};
use bytes::Bytes;
use config::{Committee, Stake};
use crypto::{PublicKey, SignatureService};
use futures::stream::futures_unordered::FuturesUnordered;
use futures::stream::StreamExt as _;
use log::{debug, info};
use network::{CancelHandler, ReliableSender};
use primary::Certificate;
use primary::Header;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{sleep, Duration, Instant};

#[derive(Debug)]
pub struct ProposerMessage(pub View, pub QC, pub Option<TC>);

pub struct Proposer {
    name: PublicKey,
    committee: Committee,
    signature_service: SignatureService,
    max_block_delay: u64,
    rx_consensus: Receiver<Header>, // receives special header from primary proposer
    rx_mempool: Receiver<Certificate>,
    rx_message: Receiver<ProposerMessage>,
    tx_loopback: Sender<Block>,
    tx_committer: Sender<Certificate>,
    buffer: Vec<Header>, // Buffer should contain the latest created header   //FIXME: Does this need to be a vector? should always only contain one header
    network: ReliableSender,
    leader: Option<(View, QC, Option<TC>)>,
}

impl Proposer {
    pub fn spawn(
        name: PublicKey,
        committee: Committee,
        signature_service: SignatureService,
        rx_consensus: Receiver<Header>,
        rx_mempool: Receiver<Certificate>,
        rx_message: Receiver<ProposerMessage>,
        tx_loopback: Sender<Block>,
        tx_committer: Sender<Certificate>,
    ) {
        tokio::spawn(async move {
            Self {
                name,
                committee,
                signature_service,
                max_block_delay: 2_000,
                rx_consensus,
                rx_mempool,
                rx_message,
                tx_loopback,
                tx_committer,
                buffer: Vec::new(),
                network: ReliableSender::new(),
                leader: None,
            }
            .run()
            .await;
        });
    }

    /// Helper function. It waits for a future to complete and then delivers a value.
    async fn waiter(wait_for: CancelHandler, deliver: Stake) -> Stake {
        let _ = wait_for.await;
        deliver
    }

    async fn make_block(&mut self, view: View, qc: QC, tc: Option<TC>) {
        // Generate a new block.
        let block = Block::new(
            qc,
            tc,
            self.name,
            view,
            /* payload */ self.buffer.drain(..).collect(),
            self.signature_service.clone(),
        )
        .await;

        info!("Created {:?}", block);

        // Broadcast our new block.
        debug!("Broadcasting {:?}", block);
        let (names, addresses): (Vec<_>, _) = self
            .committee
            .others_consensus(&self.name)
            .into_iter()
            .map(|(name, x)| (name, x.consensus_to_consensus))
            .unzip();
        let message = bincode::serialize(&ConsensusMessage::Propose(block.clone()))
            .expect("Failed to serialize block");
        let handles = self
            .network
            .broadcast(addresses, Bytes::from(message))
            .await;

        // Send our block to the core for processing.
        self.tx_loopback
            .send(block)
            .await
            .expect("Failed to send block");

        // Control system: Wait for 2f+1 nodes to acknowledge our block before continuing.
        let mut wait_for_quorum: FuturesUnordered<_> = names
            .into_iter()
            .zip(handles.into_iter())
            .map(|(name, handler)| {
                let stake = self.committee.stake(&name);
                Self::waiter(handler, stake)
            })
            .collect();

        let mut total_stake = self.committee.stake(&self.name);
        while let Some(stake) = wait_for_quorum.next().await {
            total_stake += stake;
            if total_stake >= self.committee.quorum_threshold() {
                break;
            }
        }

        // TODO: Ugly -- needed for small committee sizes.
        //sleep(Duration::from_millis(100)).await;
    }

    async fn run(&mut self) {
        let timer = sleep(Duration::from_millis(self.max_block_delay));
        tokio::pin!(timer);

        loop {
            // Check if we can propose a new block.
            let timer_expired = timer.is_elapsed();
            let got_payload = !self.buffer.is_empty();

            if timer_expired || got_payload {
                if let Some((view, qc, tc)) = self.leader.take() {
                    // Make a new block.
                    self.make_block(view, qc, tc).await; //won't try to make next block before previous block has been acked.

                    // Reschedule the timer.
                    let deadline = Instant::now() + Duration::from_millis(self.max_block_delay);
                    timer.as_mut().reset(deadline);
                }
            }

            tokio::select! {
                /*Some(certificate) = self.rx_mempool.recv() => {
                    debug!("Received {:?}", certificate);
                    self.tx_committer
                        .send(certificate.clone())
                        .await
                        .expect("Failed to send certificate to committer");

                    if self.buffer.is_empty() {
                        self.buffer.push(certificate);
                        continue;
                    }
                    if self.buffer[0].view() < certificate.view() {  //remove proposals for smaller dag views; bigger ones subsume them.
                        self.buffer.push(certificate);
                        self.buffer.swap_remove(0);
                    }
                },*/
                Some(ProposerMessage(view, qc, tc)) = self.rx_message.recv() =>  {
                    self.leader = Some((view, qc, tc));
                },
                Some(header) = self.rx_consensus.recv() => {
                    if self.buffer.is_empty() {
                        self.buffer.push(header);
                        continue;
                    }

                    if self.buffer[0].round < header.round{ //Note: This means we've been tasked with processing a header from a higher round before the RB of the previous one happened. 
                                                                    // Unclear whether we should skip the lower rounds RB, or do it also. For now, skipping. 
                                                                    // Don't think it will happen since currently we only issue new special header if we have parents 
                                                                    //& only request next special header once we have ticket ==> previous special header completed.
                        self.buffer.push(header);
                        self.buffer.swap_remove(0);
                    }
                },
                () = &mut timer => {
                    // Nothing to do.
                }
            }

            // Give the change to schedule other tasks.
            //tokio::task::yield_now().await;
        }
    }
}
