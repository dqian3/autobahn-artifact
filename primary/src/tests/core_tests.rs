// Copyright(C) Facebook, Inc. and its affiliates.
use super::*;
use crate::common::{
    certificate, committee, committee_with_base_port, header, headers, keys, listener, votes, special_header, special_certificate, special_votes,
};
use futures::future::try_join_all;
use std::{fs, time::Duration};
use tokio::{sync::mpsc::channel, time::sleep};
use serial_test::serial;

#[tokio::test]
#[serial]
async fn process_header() {
    let mut keys = keys();
    let _ = keys.pop().unwrap(); // Skip the header' author.
    let (name, secret) = keys.pop().unwrap();
    let mut signature_service = SignatureService::new(secret);

    let committee = committee_with_base_port(13_000);

    let (tx_sync_headers, _rx_sync_headers) = channel(1);
    let (tx_sync_certificates, _rx_sync_certificates) = channel(1);
    let (tx_primary_messages, rx_primary_messages) = channel(1);
    let (_tx_headers_loopback, rx_headers_loopback) = channel(1);
    let (_tx_certificates_loopback, rx_certificates_loopback) = channel(1);
    let (_tx_headers, rx_headers) = channel(1);
    let (tx_consensus, _rx_consensus) = channel(1);
    let (tx_parents, _rx_parents) = channel(1);

    let(tx_committer, _rx_committer) = channel(1);
    let(_tx_validation, rx_validation) = channel(1);
    let(tx_sailfish, _rx_special) = channel(1);
    let(_tx_pushdown_cert, rx_pushdown_cert) = channel(1);
    let(_tx_request_header_sync, rx_request_header_sync) = channel(1);

    // Create a new test store.
    let path = ".db_test_process_header";
    let _ = fs::remove_dir_all(path);
    let mut store = Store::new(path).unwrap();

    // Make the vote we expect to receive.
    let expected = Vote::new(&header(), &name, &mut signature_service, false).await;

    // Spawn a listener to receive the vote.
    let address = committee
        .primary(&header().author)
        .unwrap()
        .primary_to_primary;
    let handle = listener(address);

    // Make a synchronizer for the core.
    let synchronizer = Synchronizer::new(
        name,
        &committee,
        store.clone(),
        /* tx_header_waiter */ tx_sync_headers,
        /* tx_certificate_waiter */ tx_sync_certificates,
    );

    // Spawn the core.
    Core::spawn(
        name,
        committee,
        store.clone(),
        synchronizer,
        signature_service,
        /* consensus_round */ Arc::new(AtomicU64::new(0)),
        /* gc_depth */ 50,
        /* rx_primaries */ rx_primary_messages,
        /* rx_header_waiter */ rx_headers_loopback,
        /* rx_certificate_waiter */ rx_certificates_loopback,
        /* rx_proposer */ rx_headers,
        tx_consensus,
        tx_committer,
        /* tx_proposer */ tx_parents,
        rx_validation,
        /* special */ tx_sailfish,
        rx_pushdown_cert,
        rx_request_header_sync
    );

    // Send a header to the core.
    tx_primary_messages
        .send(PrimaryMessage::Header(header()))
        .await
        .unwrap();

    // Ensure the listener correctly received the vote.
    let received = handle.await.unwrap();
    match bincode::deserialize(&received).unwrap() {
        PrimaryMessage::Vote(x) => assert_eq!(x, expected),
        x => panic!("Unexpected message: {:?}", x),
    }

    // Ensure the header is correctly stored.
    let stored = store
        .read(header().id.to_vec())
        .await
        .unwrap()
        .map(|x| bincode::deserialize(&x).unwrap());
    assert_eq!(stored, Some(header()));
}

#[tokio::test]
#[serial]
async fn process_header_missing_parent() {
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let (tx_sync_headers, _rx_sync_headers) = channel(1);
    let (tx_sync_certificates, _rx_sync_certificates) = channel(1);
    let (tx_primary_messages, rx_primary_messages) = channel(1);
    let (_tx_headers_loopback, rx_headers_loopback) = channel(1);
    let (_tx_certificates_loopback, rx_certificates_loopback) = channel(1);
    let (_tx_headers, rx_headers) = channel(1);
    let (tx_consensus, _rx_consensus) = channel(1);
    let (tx_parents, _rx_parents) = channel(1);

    let(tx_committer, _rx_committer) = channel(1);
    let(_tx_validation, rx_validation) = channel(1);
    let(tx_sailfish, _rx_special) = channel(1);
    let(_tx_pushdown_cert, rx_pushdown_cert) = channel(1);
    let(_tx_request_header_sync, rx_request_header_sync) = channel(1);

    // Create a new test store.
    let path = ".db_test_process_header_missing_parent";
    let _ = fs::remove_dir_all(path);
    let mut store = Store::new(path).unwrap();

    // Make a synchronizer for the core.
    let synchronizer = Synchronizer::new(
        name,
        &committee(),
        store.clone(),
        /* tx_header_waiter */ tx_sync_headers,
        /* tx_certificate_waiter */ tx_sync_certificates,
    );

    // Spawn the core.
    Core::spawn(
        name,
        committee(),
        store.clone(),
        synchronizer,
        signature_service,
        /* consensus_round */ Arc::new(AtomicU64::new(0)),
        /* gc_depth */ 50,
        /* rx_primaries */ rx_primary_messages,
        /* rx_header_waiter */ rx_headers_loopback,
        /* rx_certificate_waiter */ rx_certificates_loopback,
        /* rx_proposer */ rx_headers,
        tx_consensus,
        tx_committer,
        /* tx_proposer */ tx_parents,
        rx_validation,
        tx_sailfish,
        rx_pushdown_cert,
        rx_request_header_sync
    );

    // Send a header to the core.
    let header = Header {
        parent_cert: Certificate::genesis_cert(&committee()),//[Digest::default()].iter().cloned().collect(),
        ..header()
    };
    let id = header.id.clone();
    tx_primary_messages
        .send(PrimaryMessage::Header(header))
        .await
        .unwrap();

    // Ensure the header is not stored.
    assert!(store.read(id.to_vec()).await.unwrap().is_none());
}

#[tokio::test]
#[serial]
async fn process_header_missing_payload() {
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let (tx_sync_headers, _rx_sync_headers) = channel(1);
    let (tx_sync_certificates, _rx_sync_certificates) = channel(1);
    let (tx_primary_messages, rx_primary_messages) = channel(1);
    let (_tx_headers_loopback, rx_headers_loopback) = channel(1);
    let (_tx_certificates_loopback, rx_certificates_loopback) = channel(1);
    let (_tx_headers, rx_headers) = channel(1);
    let (tx_consensus, _rx_consensus) = channel(1);
    let (tx_parents, _rx_parents) = channel(1);

    let(tx_committer, _rx_committer) = channel(1);
    let(_tx_validation, rx_validation) = channel(1);
    let(tx_sailfish, _rx_special) = channel(1);
    let(_tx_pushdown_cert, rx_pushdown_cert) = channel(1);
    let(_tx_request_header_sync, rx_request_header_sync) = channel(1);

    // Create a new test store.
    let path = ".db_test_process_header_missing_payload";
    let _ = fs::remove_dir_all(path);
    let mut store = Store::new(path).unwrap();

    // Make a synchronizer for the core.
    let synchronizer = Synchronizer::new(
        name,
        &committee(),
        store.clone(),
        /* tx_header_waiter */ tx_sync_headers,
        /* tx_certificate_waiter */ tx_sync_certificates,
    );

    // Spawn the core.
    Core::spawn(
        name,
        committee(),
        store.clone(),
        synchronizer,
        signature_service,
        /* consensus_round */ Arc::new(AtomicU64::new(0)),
        /* gc_depth */ 50,
        /* rx_primaries */ rx_primary_messages,
        /* rx_header_waiter */ rx_headers_loopback,
        /* rx_certificate_waiter */ rx_certificates_loopback,
        /* rx_proposer */ rx_headers,
        tx_consensus,
        tx_committer,
        /* tx_proposer */ tx_parents,
        rx_validation,
        tx_sailfish,
        rx_pushdown_cert,
        rx_request_header_sync
    );

    // Send a header to the core.
    let header = Header {
        payload: [(Digest::default(), 0)].iter().cloned().collect(),
        ..header()
    };
    let id = header.id.clone();
    tx_primary_messages
        .send(PrimaryMessage::Header(header))
        .await
        .unwrap();

    // Ensure the header is not stored.
    assert!(store.read(id.to_vec()).await.unwrap().is_none());
}

#[tokio::test]
#[serial]
async fn process_votes() {
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let committee = committee_with_base_port(13_100);

    let (tx_sync_headers, _rx_sync_headers) = channel(1);
    let (tx_sync_certificates, _rx_sync_certificates) = channel(1);
    let (tx_primary_messages, rx_primary_messages) = channel(1);
    let (_tx_headers_loopback, rx_headers_loopback) = channel(1);
    let (_tx_certificates_loopback, rx_certificates_loopback) = channel(1);
    let (_tx_headers, rx_headers) = channel(1);
    let (tx_consensus, _rx_consensus) = channel(1);
    let (tx_parents, mut rx_parents) = channel(1);

    let(tx_committer, _rx_committer) = channel(1);
    let(_tx_validation, rx_validation) = channel(1);
    let(tx_sailfish, _rx_special) = channel(1);
    let(_tx_pushdown_cert, rx_pushdown_cert) = channel(1);
    let(_tx_request_header_sync, rx_request_header_sync) = channel(1);

    // Create a new test store.
    let path = ".db_test_process_vote";
    let _ = fs::remove_dir_all(path);
    let store = Store::new(path).unwrap();

    // Make a synchronizer for the core.
    let synchronizer = Synchronizer::new(
        name,
        &committee,
        store.clone(),
        /* tx_header_waiter */ tx_sync_headers,
        /* tx_certificate_waiter */ tx_sync_certificates,
    );

    // Spawn the core.
    Core::spawn(
        name,
        committee.clone(),
        store.clone(),
        synchronizer,
        signature_service,
        /* consensus_round */ Arc::new(AtomicU64::new(0)),
        /* gc_depth */ 50,
        /* rx_primaries */ rx_primary_messages,
        /* rx_header_waiter */ rx_headers_loopback,
        /* rx_certificate_waiter */ rx_certificates_loopback,
        /* rx_proposer */ rx_headers,
        tx_consensus,
        tx_committer,
        /* tx_proposer */ tx_parents,
        rx_validation,
        tx_sailfish,
        rx_pushdown_cert,
        rx_request_header_sync
    );

    //TODO: instead of unit test hack: pass header() which includes edges

  
    assert_eq!(Header::default(), Header::genesis(&committee)); //Why are these equal even though author is different? Because genesis still uses the default ID.
    //Can currently use them interchangeably
    //Note: core uses Header::genesis instead of Header::default now
    /*tx_primary_messages
        .send(PrimaryMessage::Header(Header::genesis(&committee)))
        .await
        .unwrap();*/

    // Make the certificate we expect to receive.
    let header = header();
    let expected = certificate(&header);

    // Spawn all listeners to receive our newly formed certificate.
    /*let handles: Vec<_> = committee
        .others_primaries(&name)
        .iter()
        .map(|(_, address)| listener(address.primary_to_primary))
        .collect();*/

    tx_primary_messages
        .send(PrimaryMessage::Header(header.clone()))
        .await
        .unwrap();

    // Send a votes to the core.
    for vote in votes(&header) {
        tx_primary_messages
            .send(PrimaryMessage::Vote(vote))
            .await
            .unwrap();
    }
    let cert_received = rx_parents.recv().await;
    assert_eq!(cert_received.unwrap(), expected);
    // Ensure all listeners got the certificate.
    /*for received in try_join_all(handles).await.unwrap() {
        match bincode::deserialize(&received).unwrap() {
            PrimaryMessage::Header(x) => assert_eq!(x.parent_cert, expected),
            x => panic!("Unexpected message: {:?}", x),
        }
    }*/
}

#[tokio::test]
#[serial]
async fn process_certificates() {
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let (tx_sync_headers, _rx_sync_headers) = channel(1);
    let (tx_sync_certificates, _rx_sync_certificates) = channel(1);
    let (tx_primary_messages, rx_primary_messages) = channel(3);
    let (_tx_headers_loopback, rx_headers_loopback) = channel(1);
    let (_tx_certificates_loopback, rx_certificates_loopback) = channel(1);
    let (_tx_headers, rx_headers) = channel(1);
    let (tx_consensus, mut _rx_consensus) = channel(3);
    let (tx_parents, mut rx_parents) = channel(1);

    let(tx_committer, mut rx_committer) = channel(3);
    let(_tx_validation, rx_validation) = channel(1);
    let(tx_sailfish, _rx_special) = channel(3);
    let(_tx_pushdown_cert, rx_pushdown_cert) = channel(1);
    let(_tx_request_header_sync, rx_request_header_sync) = channel(1);

    // Create a new test store.
    let path = ".db_test_process_certificates";
    let _ = fs::remove_dir_all(path);
    let mut store = Store::new(path).unwrap();

    // Make a synchronizer for the core.
    let synchronizer = Synchronizer::new(
        name,
        &committee(),
        store.clone(),
        /* tx_header_waiter */ tx_sync_headers,
        /* tx_certificate_waiter */ tx_sync_certificates,
    );

    // Spawn the core.
    Core::spawn(
        name,
        committee(),
        store.clone(),
        synchronizer,
        signature_service,
        /* consensus_round */ Arc::new(AtomicU64::new(0)),
        /* gc_depth */ 50,
        /* rx_primaries */ rx_primary_messages,
        /* rx_header_waiter */ rx_headers_loopback,
        /* rx_certificate_waiter */ rx_certificates_loopback,
        /* rx_proposer */ rx_headers,
        tx_consensus,
        tx_committer,
        /* tx_proposer */ tx_parents,
        rx_validation,
        tx_sailfish,
        rx_pushdown_cert,
        rx_request_header_sync
    );

    // Send enough certificates to the core.
    let certificates: Vec<_> = headers()
        .iter()
        .take(3)
        .map(|header| certificate(header))
        .collect();

        //Digest/Sig debug
    // for header in headers(){
    //     println!("header digest: {:?}", header.digest());
    //     let votes = votes(&header); //Votes for the first header ==> these are the votes that were used for the first certificate
    //     for vote in votes {
    //         println!("vote digest: {:?}", vote.digest());
    //         println!("vote signature: {:?}", vote.signature);
    //         println!("vote author: {:?}", vote.author);
    //     }
    //     println!("             ");
    // }
   
   
    for x in certificates.clone() {
        tx_primary_messages
            .send(PrimaryMessage::Certificate(x))
            .await
            .unwrap();
    }

    // // Ensure the core sends the parents of the certificates to the proposer.
    //let received = rx_parents.recv().await.unwrap();
    //let parents = certificates.iter().map(|x| x.digest()).collect();
    //assert_eq!(received, (parents, 1));

    // Ensure the core sends the certificates to the consensus committer.
    for x in certificates.clone() {
        let received = rx_committer.recv().await.unwrap();
        assert_eq!(received, x);
    }

    // Ensure the certificates are stored.
    for x in &certificates {
        let stored = store.read(x.digest().to_vec()).await.unwrap();
        let serialized = bincode::serialize(x).unwrap();
        assert_eq!(stored, Some(serialized));
    }
}


/*#[tokio::test]
#[serial]
async fn process_special_header() {
    let mut keys = keys();
    let _ = keys.pop().unwrap(); // Skip the header' author.
    let (name, secret) = keys.pop().unwrap();
    let mut signature_service = SignatureService::new(secret);

    let committee = committee_with_base_port(13_000);

    let (tx_sync_headers, _rx_sync_headers) = channel(1);
    let (tx_sync_certificates, _rx_sync_certificates) = channel(1);
    let (tx_primary_messages, rx_primary_messages) = channel(1);
    let (_tx_headers_loopback, rx_headers_loopback) = channel(1);
    let (_tx_certificates_loopback, rx_certificates_loopback) = channel(1);
    let (_tx_headers, rx_headers) = channel(1);
    let (tx_consensus, _rx_consensus) = channel(1);
    let (tx_parents, _rx_parents) = channel(1);

    let(tx_committer, _rx_committer) = channel(1);
    let(tx_validation, rx_validation) = channel(1);
    let(tx_sailfish, mut rx_special) = channel(1);
    let(_tx_pushdown_cert, rx_pushdown_cert) = channel(1);
    let(_tx_request_header_sync, rx_request_header_sync) = channel(1);

    // Create a new test store.
    let path = ".db_test_process_header";
    let _ = fs::remove_dir_all(path);
    let mut store = Store::new(path).unwrap();

    // Make the vote we expect to receive.
    //let expected = Vote::new(&header(), &name, &mut signature_service, 0u8, None, None).await;
    let special_expected = Vote::new(&special_header(), &name, &mut signature_service, true).await;

    // Spawn a listener to receive the vote.
    let address = committee
        .primary(&header().author)
        .unwrap()
        .primary_to_primary;
    let handle = listener(address.clone());
    

    // Make a synchronizer for the core.
    let synchronizer = Synchronizer::new(
        name,
        &committee,
        store.clone(),
        /* tx_header_waiter */ tx_sync_headers,
        /* tx_certificate_waiter */ tx_sync_certificates,
    );

    // Spawn the core.
    Core::spawn(
        name,
        committee,
        store.clone(),
        synchronizer,
        signature_service,
        /* consensus_round */ Arc::new(AtomicU64::new(0)),
        /* gc_depth */ 50,
        /* rx_primaries */ rx_primary_messages,
        /* rx_header_waiter */ rx_headers_loopback,
        /* rx_certificate_waiter */ rx_certificates_loopback,
        /* rx_proposer */ rx_headers,
        tx_consensus,
        tx_committer,
        /* tx_proposer */ tx_parents,
        rx_validation,
        tx_sailfish,
        rx_pushdown_cert,
        rx_request_header_sync
    );

//  // Send a normal header to the core. 
//     tx_primary_messages
//         .send(PrimaryMessage::Header(header()))
//         .await
//         .unwrap();

    
//     //Generates vote:
//     // Ensure the listener correctly received the vote.
//     let received = handle.await.unwrap();
//     match bincode::deserialize(&received).unwrap() {
//         PrimaryMessage::Vote(x) => assert_eq!(x, expected),
//         x => panic!("Unexpected message: {:?}", x),
//     }

//     // Ensure the header is correctly stored.
//     let stored = store
//         .read(header().id.to_vec())
//         .await
//         .unwrap()
//         .map(|x| bincode::deserialize(&x).unwrap());
//     assert_eq!(stored, Some(header()));

    
//     //// Start special header
//     ////////// once we confirm parent is stored.
//     let handle = listener(address.clone());

    //Send special header with special edge = previous header
    tx_primary_messages
        .send(PrimaryMessage::Header(special_header()))
        .await
        .unwrap();

   
    //TODO: create receiver for validation
    //send back val result = correct
    let val = rx_special.recv().await.unwrap();
    tx_validation.send((val, 1u8, None, None)).await.unwrap();
    

    // Ensure the listener correctly received the vote.
    
    let received = handle.await.unwrap();
    match bincode::deserialize(&received).unwrap() {
        PrimaryMessage::Vote(x) => assert_eq!(x, special_expected),
        x => panic!("Unexpected message: {:?}", x),
    }

    // Ensure the header is correctly stored.
    let stored = store
        .read(special_header().id.to_vec())
        .await
        .unwrap()
        .map(|x| bincode::deserialize(&x).unwrap());
    assert_eq!(stored, Some(special_header()));
}*/

//todo: process special vote




/*#[tokio::test]
#[serial]
async fn process_special_votes() { 
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let committee = committee_with_base_port(13_100);

    let (tx_sync_headers, _rx_sync_headers) = channel(1);
    let (tx_sync_certificates, _rx_sync_certificates) = channel(1);
    let (tx_primary_messages, rx_primary_messages) = channel(1);
    let (_tx_headers_loopback, rx_headers_loopback) = channel(1);
    let (_tx_certificates_loopback, rx_certificates_loopback) = channel(1);
    let (tx_headers, rx_headers) = channel(1);
    let (tx_consensus, _rx_consensus) = channel(1);
    let (tx_parents, _rx_parents) = channel(1);

    let(tx_committer, _rx_committer) = channel(1);
    let(tx_validation, rx_validation) = channel(1);
    let(tx_sailfish, mut rx_special) = channel(1);
    let(_tx_pushdown_cert, rx_pushdown_cert) = channel(1);
    let(_tx_request_header_sync, rx_request_header_sync) = channel(1);

    // Create a new test store.
    let path = ".db_test_process_vote";
    let _ = fs::remove_dir_all(path);
    let store = Store::new(path).unwrap();

    // Make a synchronizer for the core.
    let synchronizer = Synchronizer::new(
        name,
        &committee,
        store.clone(),
        /* tx_header_waiter */ tx_sync_headers,
        /* tx_certificate_waiter */ tx_sync_certificates,
    );

    // Spawn the core.
    Core::spawn(
        name,
        committee.clone(),
        store.clone(),
        synchronizer,
        signature_service,
        /* consensus_round */ Arc::new(AtomicU64::new(0)),
        /* gc_depth */ 50,
        /* rx_primaries */ rx_primary_messages,
        /* rx_header_waiter */ rx_headers_loopback,
        /* rx_certificate_waiter */ rx_certificates_loopback,
        /* rx_proposer */ rx_headers,
        tx_consensus,
        tx_committer,
        /* tx_proposer */ tx_parents,
        rx_validation,
        tx_sailfish,
        rx_pushdown_cert,
        rx_request_header_sync
    );

    // Spawn all listeners to receive our newly formed certificate.
    let vote_handles: Vec<_> = committee
        .others_primaries(&name)
        .iter()
        .map(|(_, address)| listener(address.clone().primary_to_primary))
        .collect();

    //Send new special header to core (to propose itself)

    let header = special_header();
    tx_headers
        .send(header)
        .await
        .unwrap();
    //will broadcast header and process own vote

    //Ensure special header is processed first and becomes "current_header"
    sleep(Duration::from_millis(100)).await;


      // Ensure all listeners got the header.
      for received in try_join_all(vote_handles).await.unwrap() {
        match bincode::deserialize(&received).unwrap() {
            PrimaryMessage::Header(x) => assert_eq!(x, special_header()),
            x => panic!("Unexpected message: {:?}", x),
        }
    }
    println!("received all votes");
    // // Send a votes to the core. ==> Sending only 2 votes. Supplementing quorum with own vote (called as result of process_header).
    let mut count = 0;
    for vote in special_votes(&special_header()) {
        if vote.author == name {continue;}
        tx_primary_messages
            .send(PrimaryMessage::Vote(vote))
            .await
            .unwrap();
        count = count+1;
        if count == 2 { break;}
    }

    println!("count {}", count); 

     //send back val result = correct ==> allows us to form our own vote.
     let val = rx_special.recv().await.unwrap();
     tx_validation.send((val, name.0[0] % 2, None, None)).await.unwrap();

    //Upon receiving all votes, will broadcast cert.

    // Make the certificate we expect to receive.
    let expected = special_certificate(&special_header());
    
    // let received = rx_committer.recv().await.unwrap();
    // assert_eq!(received, expected);

    println!("expected num votes: {}", expected.votes.len());

     // Spawn all listeners to receive our newly formed certificate.
    let cert_handles: Vec<_> = committee
    .others_primaries(&name)
    .iter()
    .map(|(_, address)| listener(address.clone().primary_to_primary))
    .collect();

    // Ensure all listeners got the certificate.
    for received in try_join_all(cert_handles).await.unwrap() {
        match bincode::deserialize(&received).unwrap() {
            PrimaryMessage::Certificate(x) => {println!{"received cert with {} votes", x.votes.len()}; assert_eq!(x, expected)},
            x => panic!("Unexpected message: {:?}", x),
        }
    }
}*/


/*#[tokio::test]
#[serial]
async fn process_special_certificate() {
    let (name, secret) = keys().pop().unwrap();
    let signature_service = SignatureService::new(secret);

    let (tx_sync_headers, _rx_sync_headers) = channel(1);
    let (tx_sync_certificates, _rx_sync_certificates) = channel(1);
    let (tx_primary_messages, rx_primary_messages) = channel(3);
    let (_tx_headers_loopback, rx_headers_loopback) = channel(1);
    let (_tx_certificates_loopback, rx_certificates_loopback) = channel(1);
    let (_tx_headers, rx_headers) = channel(1);
    let (tx_consensus, mut _rx_consensus) = channel(3);
    let (tx_parents, mut _rx_parents) = channel(1);

    let(tx_committer, mut rx_committer) = channel(2);
    let(_tx_validation, rx_validation) = channel(1);
    let(tx_sailfish, _rx_special) = channel(3);
    let(_tx_pushdown_cert, rx_pushdown_cert) = channel(1);
    let(_tx_request_header_sync, rx_request_header_sync) = channel(1);

    // Create a new test store.
    let path = ".db_test_process_certificates";
    let _ = fs::remove_dir_all(path);
    let mut store = Store::new(path).unwrap();

    // Make a synchronizer for the core.
    let synchronizer = Synchronizer::new(
        name,
        &committee(),
        store.clone(),
        /* tx_header_waiter */ tx_sync_headers,
        /* tx_certificate_waiter */ tx_sync_certificates,
    );

    // Spawn the core.
    Core::spawn(
        name,
        committee(),
        store.clone(),
        synchronizer,
        signature_service,
        /* consensus_round */ Arc::new(AtomicU64::new(0)),
        /* gc_depth */ 50,
        /* rx_primaries */ rx_primary_messages,
        /* rx_header_waiter */ rx_headers_loopback,
        /* rx_certificate_waiter */ rx_certificates_loopback,
        /* rx_proposer */ rx_headers,
        tx_consensus,
        tx_committer,
        /* tx_proposer */ tx_parents,
        rx_validation,
        tx_sailfish,
        rx_pushdown_cert,
        rx_request_header_sync
    );

    //Send one special cert to core
    let cert = special_certificate(&special_header());
    tx_primary_messages
            .send(PrimaryMessage::Certificate(cert.clone()))
            .await
            .unwrap();

    //Make sure cert is stored
    //Make sure committer receives two certs, one for parent, one for self.

    // Ensure the core sends the certificates to the consensus committer.
    let expected_parent_cert = Certificate {
        header_digest: Header::genesis(&committee()).,
        ..Certificate::default()
    };
    let parent = rx_committer.recv().await.unwrap(); //TODO: Receive special parent
    assert_eq!(parent, expected_parent_cert);

    let received = rx_committer.recv().await.unwrap();
    assert_eq!(received, cert);
    
    // Ensure the certificates are stored.
    let stored = store.read(cert.digest().to_vec()).await.unwrap();
    let serialized = bincode::serialize(&cert).unwrap();
    assert_eq!(stored, Some(serialized));
    
}*/
