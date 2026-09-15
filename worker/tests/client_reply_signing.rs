//! Signed client replies: sign each request's tag, then verify against the
//! replier's key and index.

use crypto::{generate_keypair, PublicKey, Signer};
use rand::rngs::StdRng;
use rand::SeedableRng as _;
use worker::client_reply::{
    sign_reply_entries, verify_reply_entries, KIND_SAMPLE, REPLY_SIGNATURE_LEN,
    SIGNED_REPLY_ENTRY_LEN, TAG_LEN,
};

fn keypair(seed: u8) -> (Signer, PublicKey) {
    let (public, secret) = generate_keypair(&mut StdRng::from_seed([seed; 32]));
    (Signer::new(&secret), public)
}

/// Replier 3's signed entries for two requests.
fn signed_entries(signer: &Signer) -> Vec<u8> {
    let mut tags = vec![KIND_SAMPLE, 0, 0, 0, 0, 0, 0, 0, 7];
    tags.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 8]);
    sign_reply_entries(signer, 3, &tags)
}

fn entries(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.chunks_exact(SIGNED_REPLY_ENTRY_LEN).collect()
}

#[test]
fn signed_entries_verify() {
    let (signer, key) = keypair(1);
    let signed = signed_entries(&signer);
    assert_eq!(signed.len(), 2 * (TAG_LEN + REPLY_SIGNATURE_LEN));
    assert_eq!(&signed[..TAG_LEN], &[KIND_SAMPLE, 0, 0, 0, 0, 0, 0, 0, 7]);
    assert_eq!(verify_reply_entries(&key, 3, &entries(&signed)), vec![true, true]);
}

#[test]
fn a_tampered_tag_fails_alone() {
    let (signer, key) = keypair(1);
    let mut signed = signed_entries(&signer);
    signed[SIGNED_REPLY_ENTRY_LEN + TAG_LEN - 1] ^= 1;
    assert_eq!(verify_reply_entries(&key, 3, &entries(&signed)), vec![true, false]);
}

#[test]
fn the_wrong_replier_index_fails() {
    let (signer, key) = keypair(1);
    let signed = signed_entries(&signer);
    assert_eq!(verify_reply_entries(&key, 2, &entries(&signed)), vec![false, false]);
}

#[test]
fn the_wrong_key_fails() {
    let (signer, _) = keypair(1);
    let (_, other) = keypair(2);
    let signed = signed_entries(&signer);
    assert_eq!(verify_reply_entries(&other, 3, &entries(&signed)), vec![false, false]);
}

#[test]
fn a_short_entry_fails() {
    let (signer, key) = keypair(1);
    let signed = signed_entries(&signer);
    let short = &signed[..SIGNED_REPLY_ENTRY_LEN - 1];
    let whole = &signed[SIGNED_REPLY_ENTRY_LEN..];
    assert_eq!(verify_reply_entries(&key, 3, &[short, whole]), vec![false, true]);
}
