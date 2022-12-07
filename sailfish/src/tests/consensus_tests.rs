use super::*;
use crate::common::{committee_with_base_port, keys};
use config::Parameters;
use crypto::{SecretKey, Hash};
use futures::future::try_join_all;
use std::collections::{VecDeque, BTreeSet, BTreeMap};
use std::fs;
use std::time::Duration;
use tokio::{sync::mpsc::channel, time::sleep};
use tokio::task::JoinHandle;

/* 
fn spawn_nodes(
    keys: Vec<(PublicKey, SecretKey)>,
    committee: Committee,
    store_path: &str,
) -> Vec<JoinHandle<Block>> {
    keys.into_iter()
        .enumerate()
        .map(|(i, (name, secret))| {
            let committee = committee.clone();
            let parameters = Parameters {
                timeout_delay: 100,
                ..Parameters::default()
            };
            let store_path = format!("{}_{}", store_path, i);
            let _ = fs::remove_dir_all(&store_path);
            let store = Store::new(&store_path).unwrap();
            let signature_service = SignatureService::new(secret);
            let (tx_consensus_to_mempool, mut rx_consensus_to_mempool) = channel(10);
            let (_tx_mempool_to_consensus, rx_mempool_to_consensus) = channel(1);
            let (tx_commit, mut rx_commit) = channel(1);
            let (tx_committer, rx_committer) = channel(1);

            let (tx_output, rx_output) = channel(1);
            let (tx_ticket, rx_ticket) = channel(1);
            let (tx_validation, rx_validation) = channel(1);
            let (tx_sailfish, rx_sailfish) = channel(1);

            let(tx_pushdown_cert, _rx_pushdown_cert) = channel(1);
            let(tx_request_header_sync, _rx_request_header_sync) = channel(1);


            // Sink the mempool channel.
            tokio::spawn(async move {
                loop {
                    rx_consensus_to_mempool.recv().await;
                    //rx_sailfish.recv().await;
                }
            });

            // Spawn the consensus engine.
            tokio::spawn(async move {
                Consensus::spawn(
                    name,
                    committee,
                    parameters,
                    signature_service,
                    store,
                    rx_mempool_to_consensus,
                    rx_committer,
                    tx_consensus_to_mempool,
                    tx_output,
                    tx_ticket,
                    tx_validation,
                    rx_sailfish,
                    tx_pushdown_cert,
                    tx_request_header_sync,
                );

                rx_commit.recv().await.unwrap()
            })
        })
        .collect()
}

// TODO: This test is not applicable since primary module aids in consensus
//TODO: Write an end to end test that involves primary:
    // send ticket to primary proposer, and link all channels correctls -> should commit

#[tokio::test]
async fn end_to_end() {
    let committee = committee_with_base_port(15_000);

    // Run all nodes.
    let store_path = ".db_test_end_to_end";
    let handles = spawn_nodes(keys(), committee, store_path);

    // Ensure all threads terminated correctly.
    let blocks = try_join_all(handles).await.unwrap();
    assert!(blocks.windows(2).all(|w| w[0] == w[1]));
}

*/

fn spawn_nodes(
    mut keys: Vec<(PublicKey, SecretKey)>,
    committee: Committee,
    store_path: &str,
) //-> Vec<JoinHandle<Certificate>> {
 {

    //Create vector of consensus channels.
    let mut consensus_channels: VecDeque<(Sender<Certificate>, Receiver<Certificate>)> = keys.iter_mut()
                                 .map(|_| {
                                    let (tx_mempool_to_consensus, rx_mempool_to_consensus) = channel(1);
                                    (tx_mempool_to_consensus, rx_mempool_to_consensus)
                                 })
                                 .collect();
    let consensus_senders: Vec<Sender<Certificate>> = consensus_channels.iter_mut()
                                .map(|(sender, receiver)| {
                                    sender.clone()
                                })
                                .collect();

    let mut stores: VecDeque<Store> = consensus_channels.iter_mut().enumerate().map(|(i, _)| {
        let store_path = format!("{}_{}", store_path, i);
        let _ = fs::remove_dir_all(&store_path);
        let store = Store::new(&store_path).unwrap();
        store
    }).collect();

    let nodes: Vec<i32> = keys.into_iter()
        .enumerate()
        .map(|(i, (name, secret))| {
            let committee = committee.clone();
            let parameters = Parameters {
                timeout_delay: 100,
                ..Parameters::default()
            };
            // let store_path = format!("{}_{}", store_path, i);
            // let _ = fs::remove_dir_all(&store_path);
            // let store = Store::new(&store_path).unwrap();
            let signature_service = SignatureService::new(secret);
            let (tx_consensus_to_mempool, mut rx_consensus_to_mempool) = channel(10);
            //let (_tx_mempool_to_consensus, rx_mempool_to_consensus) = channel(1);
            //let (tx_commit, mut rx_commit) = channel(1);
            let (tx_committer, rx_committer) = channel(1);

            let (tx_output, mut rx_output) = channel(1);
            let (tx_ticket, mut rx_ticket) = channel(1);
            let (tx_validation, mut rx_validation) = channel(1);
            let (tx_sailfish, rx_sailfish) = channel(1);

            let(tx_pushdown_cert, _rx_pushdown_cert) = channel(1);
            let(tx_request_header_sync, mut rx_request_header_sync) = channel(1);

            let rx_mempool_to_consensus = consensus_channels.pop_front().unwrap().1;

            let consensus_sender_channels = consensus_senders.clone();
            let mut signature_service_copy = signature_service.clone();
            let author = name.clone();
            let mut stores_copy = stores.clone();

            let store = stores.pop_front().unwrap();

            // Sink the mempool channel.
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        Some(_) = rx_request_header_sync.recv() => {},
                        Some(_) = rx_validation.recv() => {},
                          //Receive Ticket
                        Some(tick) = rx_ticket.recv() => {
                            //Ignore all DAG processing TODO: Add a proper primary module here.
                           let ticket: Ticket = tick;
                           
                           //Issue Certificate to all nodes. (Note: This is simulating *only* special headers)
                           // Create a new header based on past ticket
                           let parents: BTreeSet<Digest> = BTreeSet::new();
                           let mut payload: Vec<(Digest, u32)> = Vec::new(); // = BTreeMap::new();
                           let new_round = ticket.round.clone() + 1;
                           let new_view = ticket.view.clone() + 1;
                
                           let header: Header = Header::new(author, new_round, payload.drain(..).collect(), parents, &mut signature_service_copy, true, new_view, None, 0, Some(ticket.clone()), ticket.round.clone(), Some(ticket.digest())).await;
                           // write header to stores
                            for store in &mut stores_copy {
                                let bytes = bincode::serialize(&header).expect("Failed to serialize header");
                                (*store).write(header.id.to_vec(), bytes).await;
                            }
                           
                            
                           let cert: Certificate = Certificate { header: header, special_valids: Vec::new(), votes: Vec::new() };
                
                           for sender in &consensus_sender_channels{
                               sender.send(cert.clone()).await.expect("failed to send cert to consensus core");
                           }  
                       },
                       
                    }
                }
            });

            // Spawn the consensus engine.
            tokio::spawn(async move {
                Consensus::spawn(
                    name,
                    committee,
                    parameters,
                    signature_service,
                    store,
                    rx_mempool_to_consensus,
                    rx_committer,
                    tx_consensus_to_mempool,
                    tx_output,
                    tx_ticket,
                    tx_validation,
                    rx_sailfish,
                    tx_pushdown_cert,
                    tx_request_header_sync,
                );

                //rx_commit.recv().await.unwrap()
                while let Some(header) = rx_output.recv().await {
                    println!("committed header for view {}", header.view);
                    // NOTE: Here goes the application logic.
                }
            });
            0
        })
       .collect();
}

#[tokio::test]
async fn end_to_end() {
    let committee = committee_with_base_port(15_000);

    // Run all nodes.
    let store_path = ".db_test_end_to_end";
    let handles = spawn_nodes(keys(), committee, store_path);

    sleep(Duration::from_millis(10_000)).await;
    // Ensure all threads terminated correctly.
    //let certs = try_join_all(handles).await.unwrap();
    //assert!(certs.windows(2).all(|w| w[0].view+1 == w[1].view));
}