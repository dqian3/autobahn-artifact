use super::*;
use crate::common::{chain, committee, committee_with_base_port, keys, listener};
use crypto::{SecretKey, Signature};
use primary::messages::{Header, QC, TC, Vote};
use futures::future::try_join_all;
use std::{fs, collections::BTreeMap, collections::BTreeSet};
use tokio::sync::mpsc::channel;

fn core(
    name: PublicKey,
    secret: SecretKey,
    committee: Committee,
    store_path: &str,
) -> (
    Sender<ConsensusMessage>,
    Receiver<ProposerMessage>,
    Receiver<Block>,
    Receiver<(Header, u8, Option<QC>, Option<TC>)>,
    Sender<Header>,
    Receiver<(u64, u64, Ticket)>,
    Receiver<Certificate>,
) {
    let (tx_core, rx_core) = channel(1);
    let (tx_loopback, rx_loopback) = channel(1);
    let (tx_proposer, rx_proposer) = channel(1);
    let (tx_mempool, mut rx_mempool) = channel(1);
    let (tx_commit, rx_commit) = channel(1);
    let (tx_consensus, rx_consensus) = channel(1);
    let (tx_validation, mut rx_validation) = channel(1);
    let (tx_ticket, rx_ticket) = channel(1);
    let (tx_special, rx_special) = channel(1);
    let (tx_block, rx_block) = channel(1);

    let signature_service = SignatureService::new(secret);
    let _ = fs::remove_dir_all(store_path);
    let store = Store::new(store_path).unwrap();
    let leader_elector = LeaderElector::new(committee.clone());
    let mempool_driver = MempoolDriver::new(committee.clone(), tx_mempool);
    let synchronizer = Synchronizer::new(
        name,
        committee.clone(),
        store.clone(),
        tx_loopback,
        /* sync_retry_delay */ 100_000,
    );

    tokio::spawn(async move {
        loop {
            rx_mempool.recv().await;
        }
    });

    Core::spawn(
        name,
        committee,
        signature_service,
        store,
        leader_elector,
        mempool_driver,
        synchronizer,
        /* timeout_delay */ 100,
        /* rx_message */ rx_core,
        rx_consensus,
        rx_loopback,
        tx_proposer,
        tx_commit,
        tx_validation,
        tx_ticket,
        rx_special,
    );

    (tx_core, rx_proposer, rx_block, rx_validation, tx_special, rx_ticket, rx_commit)
}

fn leader_keys(round: Round) -> (PublicKey, SecretKey) {
    let leader_elector = LeaderElector::new(committee());
    let leader = leader_elector.get_leader(round);
    keys()
        .into_iter()
        .find(|(public_key, _)| *public_key == leader)
        .unwrap()
}

#[tokio::test]
async fn handle_header() {
    let committee = committee_with_base_port(16_000);

    // Make a block and the vote we expect to receive.
    //let block = chain(vec![leader_keys(1)]).pop().unwrap();
    let (public_key, secret_key) = keys().pop().unwrap();
    let (public_key_1, secret_key_1) = leader_keys(1);
    //let vote = Vote::new_from_key(block.digest(), block.view, public_key, &secret_key);

    let header = Header {author: public_key_1, round: 2, payload: BTreeMap::new(), parents: BTreeSet::new(),
                         id: Digest::default(), signature: Signature::default(), is_special: true, view: 1,
                         prev_view_round: 1, special_parent: None, special_parent_round: 1, ticket: None, prev_view_header: None};
    let id = header.digest();
    let signature = Signature::new(&id, &secret_key_1);

    let head = Header {id, signature, ..header};
        //(public_key, 1, BTreeMap::new(), BTreeSet::new(), sig_service, true, 1, 1, None, 1).await;
    let validate: (Header, u8, Option<QC>, Option<TC>) = (head.clone(), 1, None, None);
    //let expected = bincode::serialize(&validate).unwrap();

    // Run a core instance.
    let store_path = ".db_test_handle_proposal";
    let (_, _rx_proposer, _rx_commit, mut rx_validation, tx_special, _, _) =
        core(public_key, secret_key, committee.clone(), store_path);

    // Send a header to the core.
    //let message = ConsensusMessage::Propose(block.clone());
    tx_special.send(head).await.unwrap();

    let received = rx_validation.recv().await.unwrap();

    //assert!(received.0.eq(&validate.0));
    assert_eq!(received.1, validate.1);
    assert_eq!(received.2, validate.2);
    //assert_eq!(received.3, validate.3);

    // Ensure the next leaders gets the vote.

    //let (next_leader, _) = leader_keys(1);
    //let address = committee.address(&next_leader).unwrap();
    //let handle = listener(address, Some(Bytes::from(expected)));
    //assert!(handle.await.is_ok());
}

#[tokio::test]
async fn generate_proposal() {
    // Get the keys of the leaders of this round and the next.
    let (leader, leader_key) = leader_keys(1);
    let (next_leader, next_leader_key) = leader_keys(2);

    // Make a block, votes, and QC.
    let block = Block::new_from_key(QC::genesis(), leader, 1, Vec::new(), &leader_key);
    let hash = block.digest();
    let votes: Vec<_> = keys()
        .iter()
        .map(|(public_key, secret_key)| {
            Vote::new_from_key(hash.clone(), block.view, *public_key, &secret_key)
        })
        .collect();
    let high_qc = QC {
        hash,
        view: block.view,
        view_round: block.view,
        votes: votes
            .iter()
            .cloned()
            .map(|x| (x.author, x.signature))
            .collect(),
    };

    // Run a core instance.
    let store_path = ".db_test_generate_proposal";
    let (tx_core, mut rx_proposer, _rx_commit, _, _, mut rx_ticket, _) =
        core(next_leader, next_leader_key, committee(), store_path);

    let message = ConsensusMessage::QC(high_qc.clone());
    tx_core.send(message).await.unwrap();

    // Send all votes to the core.
    /*for vote in votes.clone() {
        let message = ConsensusMessage::Vote(vote);
        tx_core.send(message).await.unwrap();
    }*/

    // Ensure the core sends a new block.
    let (view, round, _) = rx_ticket.recv().await.unwrap();
    assert_eq!(round, 1);
    assert_eq!(view, 2);
    //assert_eq!(qc, high_qc);
    //assert!(tc.is_none());
}

#[tokio::test]
async fn commit_block() {
    // Get enough distinct leaders to form a quorum.
    let leaders = vec![leader_keys(1), leader_keys(2), leader_keys(3)];
    let chain = chain(leaders);

    // Run a core instance.
    let store_path = ".db_test_commit_block";
    let (public_key, secret_key) = keys().pop().unwrap();
    let (tx_core, _rx_proposer, _, mut rx_validation, tx_special, mut rx_ticket, mut rx_commit) =
        core(public_key, secret_key, committee(), store_path);

    let header = Header {author: leader_keys(1).0, round: 2, payload: BTreeMap::new(), parents: BTreeSet::new(),
                         id: Digest::default(), signature: Signature::default(), is_special: true, view: 1,
                         prev_view_round: 1, special_parent: None, special_parent_round: 1, prev_view_header: None, ticket: None};
    let id = header.digest();
    let signature = Signature::new(&id, &leader_keys(1).1);

    let head = Header {id: id.clone(), signature, ..header};

    tx_special.send(head.clone()).await.unwrap();
    rx_validation.recv().await.unwrap();

    let mut payload = Vec::new();
    payload.push(head.clone());

    //let block = Block::new_from_key(QC::genesis(), leaders[0].0, 1, Vec::new(), &leaders[0].1);
    let mut block = chain[0].clone();
    block.payload = payload;
    let hash = block.digest();
    let votes: Vec<_> = keys()
        .iter()
        .map(|(public_key, secret_key)| {
            Vote::new_from_key(hash.clone(), block.view, *public_key, &secret_key)
        })
        .collect();
    let high_qc = QC {
        hash: id,
        view: block.view,
        view_round: block.view,
        votes: votes
            .iter()
            .cloned()
            .map(|x| (x.author, x.signature))
            .collect(),
    };

    let message = ConsensusMessage::QC(high_qc.clone());
    tx_core.send(message).await.unwrap();
    //rx_validation.recv().await.unwrap();

    // Send a the blocks to the core.
    //let committed = chain[0].clone();
    let committed = Certificate { header: head, special_valids: Vec::new(), votes: high_qc.votes.clone() };
    //for block in chain {
//        let message = ConsensusMessage::Propose(block);
  //      tx_core.send(message).await.unwrap();
    //}

    let b = rx_commit.recv().await.unwrap();
    assert_eq!(b, committed);
    // Ensure the core commits the head.
    /*match rx_commit.recv().await {
        Some(b) => assert_eq!(b, committed),
        _ => assert!(false),
    }*/
}

#[tokio::test]
async fn local_timeout_round() {
    let committee = committee_with_base_port(16_100);

    // Make the timeout vote we expect to send.
    let (public_key, secret_key) = leader_keys(3);
    let timeout = Timeout::new_from_key(QC::genesis(), 1, public_key, &secret_key);
    let expected = bincode::serialize(&ConsensusMessage::Timeout(timeout)).unwrap();

    // Run a core instance.
    let store_path = ".db_test_local_timeout_round";
    let (_tx_core, _rx_proposer, _rx_commit, _, _, _, _) =
        core(public_key, secret_key, committee.clone(), store_path);

    // Ensure the node broadcasts a timeout vote.
    let handles: Vec<_> = committee
        .broadcast_addresses(&public_key)
        .into_iter()
        .map(|(_, address)| listener(address, Some(Bytes::from(expected.clone()))))
        .collect();
    assert!(try_join_all(handles).await.is_ok());
}
