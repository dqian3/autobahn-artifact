// Copyright(C) Facebook, Inc. and its affiliates.
use super::*;
use crate::common::{committee, keys};
use tokio::sync::mpsc::channel;

#[tokio::test]
async fn propose_empty() {
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let (_tx_parents, rx_parents) = channel(1);
    let (_tx_our_digests, rx_our_digests) = channel(1);
    let (tx_headers, mut rx_headers) = channel(1);
    let(_tx_ticket, rx_ticket) = channel(1);

    // Spawn the proposer.
    Proposer::spawn(
        name,
        &committee(),
        signature_service,
        /* header_size */ 1_000,
        /* max_header_delay */ 20,
        /* rx_core */ rx_parents,
        /* rx_workers */ rx_our_digests,
        rx_ticket, 
        /* tx_core */ tx_headers,
    );

    // Ensure the proposer makes a correct empty header.
    let header = rx_headers.recv().await.unwrap();
    assert_eq!(header.is_special, false);
    assert_eq!(header.round, 1);
    assert!(header.payload.is_empty());
    assert!(header.verify(&committee()).is_ok());
}

#[tokio::test]
async fn propose_payload() {
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let (_tx_parents, rx_parents) = channel(1);
    let (tx_our_digests, rx_our_digests) = channel(1);
    let (tx_headers, mut rx_headers) = channel(1);
    let(_tx_ticket, rx_ticket) = channel(1);

    // Spawn the proposer.
    Proposer::spawn(
        name,
        &committee(),
        signature_service,
        /* header_size */ 32,
        /* max_header_delay */ 1_000_000, // Ensure it is not triggered.
        /* rx_core */ rx_parents,
        /* rx_workers */ rx_our_digests,
        rx_ticket,
        /* tx_core */ tx_headers,
    );

    // Send enough digests for the header payload.
    let digest = Digest(name.0);
    let worker_id = 0;
    tx_our_digests
        .send((digest.clone(), worker_id))
        .await
        .unwrap();

    // Ensure the proposer makes a correct header from the provided payload.
    let header = rx_headers.recv().await.unwrap();
    assert_eq!(header.is_special, false);
    assert_eq!(header.round, 1);
    assert_eq!(header.payload.get(&digest), Some(&worker_id));
    assert!(header.verify(&committee()).is_ok());
}


#[tokio::test]
async fn propose_special_ticket_first() {
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let (_tx_parents, rx_parents) = channel(1);
    let (tx_our_digests, rx_our_digests) = channel(1);
    let (tx_headers, mut rx_headers) = channel(1);
    let(tx_ticket, rx_ticket) = channel(1);

    // Spawn the proposer.
    Proposer::spawn(
        name,
        &committee(),
        signature_service,
        /* header_size */ 32,
        /* max_header_delay */ 1_000_000, // Ensure it is not triggered.
        /* rx_core */ rx_parents,
        /* rx_workers */ rx_our_digests,
        rx_ticket,
        /* tx_core */ tx_headers,
    );

    //Send ticket to form a special header
    let view = 1;
    let round = 5;
    tx_ticket
        .send((view, round))
        .await
        .unwrap();

    sleep(Duration::from_secs(1)).await; //just to guarantee ticket arrives before digest (else normal block can be triggered.)

    // Send enough digests for the header payload.
    let digest = Digest(name.0);
    let worker_id = 0;
    tx_our_digests
        .send((digest.clone(), worker_id))
        .await
        .unwrap();

    // Ensure the proposer makes a correct special header from the provided payload.
    let header = rx_headers.recv().await.unwrap();
   
    assert_eq!(header.is_special, true);
    assert_eq!(header.view, 1);
    assert_eq!(header.round_view, 5);
    //TODO: special_parent round
    println!("num parents {:?}", header.parents.len());

    assert_eq!(header.round, 6);
    assert_eq!(header.payload.get(&digest), Some(&worker_id));
    assert!(header.verify(&committee()).is_ok());
}


#[tokio::test]
async fn propose_special_ticket_after() {
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let (_tx_parents, rx_parents) = channel(1);
    let (tx_our_digests, rx_our_digests) = channel(1);
    let (tx_headers, mut rx_headers) = channel(1);
    let(tx_ticket, rx_ticket) = channel(1);

    // Spawn the proposer.
    Proposer::spawn(
        name,
        &committee(),
        signature_service,
        /* header_size */ 32,
        /* max_header_delay */ 1_000_000, // Ensure it is not triggered.
        /* rx_core */ rx_parents,
        /* rx_workers */ rx_our_digests,
        rx_ticket,
        /* tx_core */ tx_headers,
    );

     // Send enough digests for the header payload.
     let digest = Digest(name.0);
     let worker_id = 0;
     tx_our_digests
         .send((digest.clone(), worker_id))
         .await
         .unwrap();

     // Ensure the proposer makes a correct header from the provided payload.
     let header = rx_headers.recv().await.unwrap();
     assert_eq!(header.is_special, false);
     assert_eq!(header.round, 1);
     assert_eq!(header.payload.get(&digest), Some(&worker_id));
     assert!(header.verify(&committee()).is_ok());
     let last_header_id = header.id;
     let last_header_round = header.round;

    //Send ticket to form a special header
    let view = 1;
    let round = 5;
    tx_ticket
        .send((view, round))
        .await
        .unwrap();

    //Note: No sleep needed. Parents are unavailable ==> will form special edge.

    // Send enough digests for the header payload.
    let digest = Digest(name.0);
    let worker_id = 0;
    tx_our_digests
        .send((digest.clone(), worker_id))
        .await
        .unwrap();


    // Ensure the proposer makes a correct special header from the provided payload.
    let header = rx_headers.recv().await.unwrap();
   
    assert_eq!(header.is_special, true);
    assert_eq!(header.view, 1);
    assert_eq!(header.round_view, 5);
    //TODO: special_parent round
    assert_eq!(header.parents.len(), 1);
    assert_eq!(header.special_parent_round, last_header_round);
    let special_parent: Digest = header.parents.iter().cloned().next().unwrap();
    assert_eq!(special_parent, last_header_id);


    assert_eq!(header.round, 6);
    assert_eq!(header.payload.get(&digest), Some(&worker_id));
    assert!(header.verify(&committee()).is_ok());
}



#[tokio::test]
async fn propose_special_ticket_after_requiring_parents() {
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let (tx_parents, rx_parents) = channel(1);
    let (tx_our_digests, rx_our_digests) = channel(1);
    let (tx_headers, mut rx_headers) = channel(1);
    let(tx_ticket, rx_ticket) = channel(1);

    // Spawn the proposer.
    Proposer::spawn(
        name,
        &committee(),
        signature_service,
        /* header_size */ 32,
        /* max_header_delay */ 1_000_000, // Ensure it is not triggered.
        /* rx_core */ rx_parents,
        /* rx_workers */ rx_our_digests,
        rx_ticket,
        /* tx_core */ tx_headers,
    );

     // Send enough digests for the header payload.
     let digest = Digest(name.0);
     let worker_id = 0;
     tx_our_digests
         .send((digest.clone(), worker_id))
         .await
         .unwrap();

     // Ensure the proposer makes a correct header from the provided payload.
     let header = rx_headers.recv().await.unwrap();
     assert_eq!(header.is_special, false);
     assert_eq!(header.round, 1);
     assert_eq!(header.payload.get(&digest), Some(&worker_id));
     assert!(header.verify(&committee()).is_ok());
     let last_header_id = header.id;
     let last_header_round = header.round;

    //Send ticket to form a special header
    let view = 1;
    let last_round = 5; //skipping ahead
    tx_ticket
        .send((view, last_round))
        .await
        .unwrap();

    //Note: No sleep needed. Parents are unavailable ==> will form special edge.

    // Send enough digests for the header payload.
    let digest = Digest(name.0);
    let worker_id = 0;
    tx_our_digests
        .send((digest.clone(), worker_id))
        .await
        .unwrap();


    // Ensure the proposer makes a correct special header from the provided payload.
    let header = rx_headers.recv().await.unwrap();
   
    assert_eq!(header.is_special, true);
    assert_eq!(header.view, 1);
    assert_eq!(header.round_view, 5);
    //TODO: special_parent round
    assert_eq!(header.parents.len(), 1);
    assert_eq!(header.special_parent_round, last_header_round);
    let special_parent: Digest = header.parents.iter().cloned().next().unwrap();
    assert_eq!(special_parent, last_header_id);

    assert_eq!(header.round, 6);
    assert_eq!(header.payload.get(&digest), Some(&worker_id));
    assert!(header.verify(&committee()).is_ok());

    //Send another ticket. But this time it won't be able to form a special edge. Must wait for parents.

    //Send ticket to form a special header
    let view = 2;
    let last_round = 6;
    tx_ticket
        .send((view, last_round))
        .await
        .unwrap();

    

    // Send enough digests for the header payload.
    let digest = Digest(name.0);
    let worker_id = 0;
    tx_our_digests
        .send((digest.clone(), worker_id))
        .await
        .unwrap();

    sleep(Duration::from_millis(1000)).await;  //sleep to guarantee ticket arrives before parents --> otherwise normal block can form.
    //Generate dummy parents (creating 4 here)
    let parents = Certificate::genesis(&committee())
        .iter()
        .map(|x| x.digest())
        .collect();
    let last_round = 6;
    tx_parents
        .send((parents, last_round))
        .await
        .unwrap();

     // Ensure the proposer makes a correct special header from the provided payload.
     let header = rx_headers.recv().await.unwrap();


    assert_eq!(header.is_special, true);
    assert_eq!(header.view, 2);
    assert_eq!(header.round_view, 6);
    //TODO: special_parent round
    assert_eq!(header.parents.len(), 4);
    // assert_eq!(header.special_parent_round, last_header_round);
    // let special_parent: Digest = header.parents.iter().cloned().next().unwrap();
    // assert_eq!(special_parent, last_header_id);


    assert_eq!(header.round, 7);
    assert_eq!(header.payload.get(&digest), Some(&worker_id));
    assert!(header.verify(&committee()).is_ok());

}