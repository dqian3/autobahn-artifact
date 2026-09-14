// Copyright(C) Facebook, Inc. and its affiliates.
use super::*;
use ed25519_dalek::Digest as _;
use ed25519_dalek::Sha512;
use rand::rngs::StdRng;
use rand::SeedableRng as _;

impl PartialEq for SecretKey {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "{}", self.encode_base64())
    }
}

pub fn keys() -> Vec<(PublicKey, SecretKey)> {
    let mut rng = StdRng::from_seed([0; 32]);
    (0..4).map(|_| generate_keypair(&mut rng)).collect()
}

#[test]
fn import_export_public_key() {
    let (public_key, _) = keys().pop().unwrap();
    let export = public_key.encode_base64();
    let import = PublicKey::decode_base64(&export);
    assert!(import.is_ok());
    assert_eq!(import.unwrap(), public_key);
}

#[test]
fn import_export_secret_key() {
    let (_, secret_key) = keys().pop().unwrap();
    let export = secret_key.encode_base64();
    let import = SecretKey::decode_base64(&export);
    assert!(import.is_ok());
    assert_eq!(import.unwrap(), secret_key);
}

#[test]
fn verify_valid_signature() {
    // Get a keypair.
    let (public_key, secret_key) = keys().pop().unwrap();

    // Make signature.
    let message: &[u8] = b"Hello, world!";
    let digest = message.digest();
    let signature = Signature::new(&digest, &secret_key);

    // Verify the signature.
    assert!(signature.verify(&digest, &public_key).is_ok());
}

#[test]
fn verify_invalid_signature() {
    // Get a keypair.
    let (public_key, secret_key) = keys().pop().unwrap();

    // Make signature.
    let message: &[u8] = b"Hello, world!";
    let digest = message.digest();
    let signature = Signature::new(&digest, &secret_key);

    // Verify the signature.
    let bad_message: &[u8] = b"Bad message!";
    let digest = bad_message.digest();
    assert!(signature.verify(&digest, &public_key).is_err());
}

#[test]
fn verify_valid_batch() {
    // Make signatures.
    let message: &[u8] = b"Hello, world!";
    let digest = message.digest();
    let mut keys = keys();
    let signatures: Vec<_> = (0..3)
        .map(|_| {
            let (public_key, secret_key) = keys.pop().unwrap();
            (public_key, Signature::new(&digest, &secret_key))
        })
        .collect();

    // Verify the batch.
    assert!(Signature::verify_batch(&digest, &signatures).is_ok());
}

#[test]
fn verify_invalid_batch() {
    // Make 2 valid signatures.
    let message: &[u8] = b"Hello, world!";
    let digest = message.digest();
    let mut keys = keys();
    let mut signatures: Vec<_> = (0..2)
        .map(|_| {
            let (public_key, secret_key) = keys.pop().unwrap();
            (public_key, Signature::new(&digest, &secret_key))
        })
        .collect();

    // Add an invalid signature.
    let (public_key, _) = keys.pop().unwrap();
    signatures.push((public_key, Signature::default()));

    // Verify the batch.
    assert!(Signature::verify_batch(&digest, &signatures).is_err());
}

#[tokio::test]
async fn signature_service() {
    // Get a keypair.
    let (public_key, secret_key) = keys().pop().unwrap();

    // Spawn the signature service.
    let mut service = SignatureService::new(secret_key);

    // Request signature from the service.
    let message: &[u8] = b"Hello, world!";
    let digest = message.digest();
    let signature = service.request_signature(digest.clone()).await;

    // Verify the signature we received.
    assert!(signature.verify(&digest, &public_key).is_ok());
}

fn signed_transactions(secret_key: &SecretKey, count: usize) -> Vec<Vec<u8>> {
    let signer = Signer::new(secret_key);
    (0..count)
        .map(|i| {
            let mut tx = vec![i as u8; 32];
            let signature = signer.sign(&tx.as_slice().digest()).flatten();
            tx.extend_from_slice(&signature);
            tx
        })
        .collect()
}

#[test]
fn verify_transactions_all_valid() {
    let (public_key, secret_key) = keys().pop().unwrap();
    let transactions = signed_transactions(&secret_key, 10);
    assert_eq!(verify_transactions(&public_key, &transactions), vec![true; 10]);
}

#[test]
fn verify_transactions_one_corrupted() {
    let (public_key, secret_key) = keys().pop().unwrap();
    let mut transactions = signed_transactions(&secret_key, 10);
    transactions[3][0] ^= 1;
    let mut expected = vec![true; 10];
    expected[3] = false;
    assert_eq!(verify_transactions(&public_key, &transactions), expected);
}

#[test]
fn verify_transactions_malformed() {
    let (public_key, secret_key) = keys().pop().unwrap();
    let mut transactions = signed_transactions(&secret_key, 3);
    transactions[1].truncate(10);
    assert_eq!(verify_transactions(&public_key, &transactions), vec![true, false, true]);
}

#[test]
fn verify_transactions_empty() {
    let (public_key, _) = keys().pop().unwrap();
    assert!(verify_transactions::<Vec<u8>>(&public_key, &[]).is_empty());
}
