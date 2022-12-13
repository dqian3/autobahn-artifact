// Copyright(C) Facebook, Inc. and its affiliates.
use crate::messages::{Certificate, Header, Vote, Ticket, AcceptVote, QC};
use bytes::Bytes;
use config::{Authority, Committee, ConsensusAddresses, PrimaryAddresses, WorkerAddresses};
use crypto::Hash as _;
use crypto::{generate_keypair, PublicKey, SecretKey, Signature};
use futures::sink::SinkExt as _;
use futures::stream::StreamExt as _;
use rand::rngs::StdRng;
use rand::SeedableRng as _;
use std::collections::{btree_set, BTreeSet};
use std::convert::TryInto;
use std::net::SocketAddr;
use std::vec;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::codec::{Framed, LengthDelimitedCodec};


// Fixture
pub fn keys() -> Vec<(PublicKey, SecretKey)> {
    let mut rng = StdRng::from_seed([0; 32]);
    (0..4).map(|_| generate_keypair(&mut rng)).collect()
}


// Fixture
pub fn committee() -> Committee {
    Committee {
        authorities: keys()
            .iter()
            .enumerate()
            .map(|(i, (id, _))| {
                let consensus = ConsensusAddresses {
                    consensus_to_consensus: format!("127.0.0.1:{}", 0 + i).parse().unwrap(),
                };
                let primary = PrimaryAddresses {
                    primary_to_primary: format!("127.0.0.1:{}", 100 + i).parse().unwrap(),
                    worker_to_primary: format!("127.0.0.1:{}", 200 + i).parse().unwrap(),
                };
                let workers = vec![(
                    0,
                    WorkerAddresses {
                        primary_to_worker: format!("127.0.0.1:{}", 300 + i).parse().unwrap(),
                        transactions: format!("127.0.0.1:{}", 400 + i).parse().unwrap(),
                        worker_to_worker: format!("127.0.0.1:{}", 500 + i).parse().unwrap(),
                    },
                )]
                .iter()
                .cloned()
                .collect();
                (
                    *id,
                    Authority {
                        stake: 1,
                        consensus,
                        primary,
                        workers,
                    },
                )
            })
            .collect(),
    }
}

// Fixture.
pub fn committee_with_base_port(base_port: u16) -> Committee {
    let mut committee = committee();
    for authority in committee.authorities.values_mut() {
        let primary = &mut authority.primary;

        let port = primary.primary_to_primary.port();
        primary.primary_to_primary.set_port(base_port + port);

        let port = primary.worker_to_primary.port();
        primary.worker_to_primary.set_port(base_port + port);

        for worker in authority.workers.values_mut() {
            let port = worker.primary_to_worker.port();
            worker.primary_to_worker.set_port(base_port + port);

            let port = worker.transactions.port();
            worker.transactions.set_port(base_port + port);

            let port = worker.worker_to_worker.port();
            worker.worker_to_worker.set_port(base_port + port);
        }
    }
    committee
}

// Fixture
pub fn header() -> Header {
    let (author, secret) = keys().pop().unwrap();
    let header = Header {
        author,
        round: 1,
        parents: Certificate::genesis(&committee())
            .iter()
            .map(|x| x.digest())
            .collect(),
        is_special: false,
        ..Header::default()
    };
    Header {
        id: header.digest(),
        signature: Signature::new(&header.digest(), &secret),
        ..header
    }
}

// Fixture
pub fn special_header() -> Header {
    let (author, secret) = keys().pop().unwrap();
    //let par = vec![header().id];
    //let par = vec![Header::default().id];
    let header = Header {
        author,
        round: 1,
        //parents: par.iter().cloned().collect(),

        is_special: true,
        view: 1,
        //special parent
        special_parent: Some(Header::genesis(&committee()).id),
        special_parent_round: 0,
        //consensus parent
        ticket: Some(Ticket::genesis(&committee())),
        prev_view_round: 0,
        consensus_parent: Some(Header::genesis(&committee()).id),

        //defaults: payload, parent, 
        ..Header::default()
    };
    Header {
        id: header.digest(),
        signature: Signature::new(&header.digest(), &secret),
        ..header
    }
}

// Fixture
pub fn headers() -> Vec<Header> {
    keys()
        .into_iter()
        .map(|(author, secret)| {
            let header = Header {
                author,
                round: 1,
                parents: Certificate::genesis(&committee())
                    .iter()
                    .map(|x| x.digest())
                    .collect(),

                is_special: false,
                ..Header::default()
            };
            Header {
                id: header.digest(),
                signature: Signature::new(&header.digest(), &secret),
                ..header
            }
        })
        .collect()
}


// Fixture
pub fn votes(header: &Header) -> Vec<Vote> {
    keys()
        .into_iter()
        .map(|(author, secret)| {
            let vote = Vote {
                id: header.id.clone(),
                round: header.round,
                origin: header.author,
                author,
                signature: Signature::default(),
                view: 1,
                special_valid: 0u8,
                qc: None,
                tc: None,
            };
            Vote {
                signature: Signature::new(&vote.digest(), &secret),
                ..vote
            }
        })
        .collect()
}

// Fixture
pub fn special_votes(header: &Header) -> Vec<Vote> {
    keys()
        .into_iter()
        .map(|(author, secret)| {
            let vote = Vote {
                id: header.id.clone(),
                round: header.round,
                origin: header.author,
                author,
                signature: Signature::default(),
                view: header.view,
                special_valid: author.0[0] % 2, //make some valid, some invalid
                qc: None,
                tc: None,
            };
            Vote {
                signature: Signature::new(&vote.digest(), &secret),
                ..vote
            }
        })
        .collect()
}

// Fixture
pub fn certificate(header: &Header) -> Certificate {
    Certificate {
        header: header.clone(),
        special_valids: vec![0u8, 0u8, 0u8],
        votes: votes(&header)
            .into_iter()
            .map(|x| (x.author, x.signature))
            .collect(),
    }
}

// Fixture
pub fn special_certificate(header: &Header) -> Certificate {
    Certificate {
        header: header.clone(),
        special_valids: special_votes(&header)
            .into_iter()
            .map(|x| x.special_valid)
            .collect(),
        votes: special_votes(&header)
            .into_iter()
            .map(|x| (x.author, x.signature))
            .collect(),
    }
}

// Fixture
pub fn listener(address: SocketAddr) -> JoinHandle<Bytes> {
    tokio::spawn(async move {
        let listener = TcpListener::bind(&address).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let transport = Framed::new(socket, LengthDelimitedCodec::new());
        let (mut writer, mut reader) = transport.split();
        match reader.next().await {
            Some(Ok(received)) => {
                writer.send(Bytes::from("Ack")).await.unwrap();
                received.freeze()
            }
            _ => panic!("Failed to receive network message"),
        }
    })
}


//Consensus message_tests

// Fixture.
pub fn committee_basic() -> Committee {
    Committee::new(
        keys()
            .into_iter()
            .enumerate()
            .map(|(i, (name, _))| {
                let address = format!("127.0.0.1:{}", i).parse().unwrap();
                let stake = 1;
                (name, stake, address)
            })
            .collect(),
        /* epoch */ //100,
    )
}

// Fixture.
pub fn accept_vote() -> AcceptVote {
    let (public_key, secret_key) = keys().pop().unwrap();
    AcceptVote::new_from_key(special_header().digest(), 1, 1, public_key, &secret_key)
}

// Fixture.
pub fn qc() -> QC {
    let qc = QC {
        hash: special_header().id, //Digest::default(),
        view: 1,
        view_round: 1,
        votes: Vec::new(),
        origin: PublicKey::default(),
    };
    let digest = qc.digest();
    let mut keys = keys();
    let votes: Vec<_> = (0..3)
        .map(|_| {
            let (public_key, secret_key) = keys.pop().unwrap();
            (public_key, Signature::new(&digest, &secret_key))
        })
        .collect();
    QC { votes, ..qc }
}

// Fixture.
pub fn fast_qc() -> QC {
    let qc = QC {
        hash: special_header().id, //Digest::default(),
        view: 1,
        view_round: 1,
        votes: Vec::new(),
        origin: special_header().author,
    };

    let vote = Vote {
        id: special_header().id, 
        round: 1,
        origin: special_header().author,
        author: PublicKey::default(),
        signature: Signature::default(),
        view: 1, 
        special_valid: 1,
        qc: None,
        tc: None,
    };
    let digest = vote.digest();

    //Using Cert to simulate should be equivalent
    // let cert = Certificate {
    //     header: special_header(),
    //     special_valids: vec![1u8, 1u8, 1u8, 1u8],
    //     votes: Vec::new(),
    // };
    // let digest = cert.verifiable_digest();

    let mut keys = keys();
    let votes: Vec<_> = (0..4)
        .map(|_| {
            let (public_key, secret_key) = keys.pop().unwrap();
            (public_key, Signature::new(&digest, &secret_key))
        })
        .collect();
    QC { votes, ..qc }
}