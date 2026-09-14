// Measures client-signature verification throughput for batches from other
// workers: one `spawn_blocking` + `verify_strict` per transaction with
// batches handled one at a time, against `worker::verify_peer_batch` with
// several batches in flight.
//
//   cargo run --release -p worker --example verify_bench
use crypto::{generate_keypair, Digest, Hash as _, PublicKey, Signer};
use ed25519_dalek::ed25519;
use futures::future::try_join_all;
use futures::stream::{self, StreamExt as _};
use rand::rngs::StdRng;
use rand::SeedableRng as _;
use serde::Serialize;
use std::time::Instant;

const TRANSACTIONS: usize = 50_000;

#[derive(Serialize)]
enum WorkerMessage {
    Batch(PublicKey, Vec<Vec<u8>>),
    #[allow(dead_code)]
    BatchRequest(Vec<Digest>, PublicKey),
}

fn make_batches(size: usize, per_batch: usize) -> (PublicKey, Vec<Vec<u8>>) {
    let (name, secret) = generate_keypair(&mut StdRng::from_seed([0; 32]));
    let signer = Signer::new(&secret);
    let transactions: Vec<Vec<u8>> = (0..TRANSACTIONS)
        .map(|i| {
            let mut tx = vec![0u8; size];
            tx[..8].copy_from_slice(&(i as u64).to_be_bytes());
            let signature = signer.sign(&tx.as_slice().digest()).flatten();
            tx.extend_from_slice(&signature);
            tx
        })
        .collect();
    let batches = transactions
        .chunks(per_batch)
        .map(|chunk| bincode::serialize(&WorkerMessage::Batch(name, chunk.to_vec())).unwrap())
        .collect();
    (name, batches)
}

async fn old_approach(batches: Vec<Vec<u8>>) -> usize {
    let mut verified = 0;
    for serialized in batches {
        let (id, batch): (PublicKey, Vec<Vec<u8>>) = match bincode::deserialize(&serialized).unwrap() {
            (0u32, body) => body,
            _ => unreachable!(),
        };
        let key = ed25519_dalek::PublicKey::from_bytes(&id.0).unwrap();
        let handles = batch.into_iter().map(|tx| {
            tokio::task::spawn_blocking(move || {
                let (msg, sig) = tx.split_at(tx.len() - 64);
                let signature = ed25519::signature::Signature::from_bytes(sig).unwrap();
                key.verify_strict(&msg.digest().0, &signature).is_ok()
            })
        });
        let results = try_join_all(handles).await.unwrap();
        assert!(results.iter().all(|ok| *ok));
        verified += results.len();
    }
    verified
}

async fn new_approach(batches: Vec<Vec<u8>>) -> usize {
    let passed = stream::iter(batches)
        .map(worker::verify_peer_batch)
        .buffered(16)
        .filter(|batch| futures::future::ready(batch.is_some()))
        .count()
        .await;
    passed
}

#[tokio::main]
async fn main() {
    for &size in &[1024usize, 16] {
        for &per_batch in &[20usize, 500] {
            let (_, batches) = make_batches(size, per_batch);
            let expected = batches.len();

            let start = Instant::now();
            assert_eq!(old_approach(batches.clone()).await, TRANSACTIONS);
            let old = TRANSACTIONS as f64 / start.elapsed().as_secs_f64();

            let start = Instant::now();
            assert_eq!(new_approach(batches).await, expected);
            let new = TRANSACTIONS as f64 / start.elapsed().as_secs_f64();

            println!(
                "{:>4} B, {:>3} txs/batch: old {:>9.0} req/s, new {:>9.0} req/s ({:.1}x)",
                size, per_batch, old, new, new / old
            );
        }
    }
}
