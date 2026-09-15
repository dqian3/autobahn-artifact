//! Signed client reply frames: sign, then verify against the replier's key.

use crypto::{generate_keypair, PublicKey, Signer};
use rand::rngs::StdRng;
use rand::SeedableRng as _;
use worker::client_reply::{
    sign_reply_frame, verify_reply_frames, KIND_SAMPLE, REPLY_HEADER_LEN, REPLY_SIGNATURE_LEN,
    TAG_LEN,
};

fn keypair(seed: u8) -> (Signer, PublicKey) {
    let (public, secret) = generate_keypair(&mut StdRng::from_seed([seed; 32]));
    (Signer::new(&secret), public)
}

/// Replier 3 acknowledging two requests.
fn signed_frame(signer: &Signer) -> Vec<u8> {
    let mut frame = vec![3u8];
    frame.extend_from_slice(&[KIND_SAMPLE, 0, 0, 0, 0, 0, 0, 0, 7]);
    frame.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 8]);
    sign_reply_frame(signer, &mut frame);
    frame
}

#[test]
fn a_signed_frame_verifies() {
    let (signer, key) = keypair(1);
    let frame = signed_frame(&signer);
    assert_eq!(frame.len(), REPLY_HEADER_LEN + 2 * TAG_LEN + REPLY_SIGNATURE_LEN);
    assert_eq!(verify_reply_frames(&key, &[&frame]), vec![true]);
}

#[test]
fn a_tampered_tag_fails() {
    let (signer, key) = keypair(1);
    let good = signed_frame(&signer);
    let mut bad = good.clone();
    bad[REPLY_HEADER_LEN + TAG_LEN - 1] ^= 1;
    assert_eq!(verify_reply_frames(&key, &[&good, &bad]), vec![true, false]);
}

#[test]
fn a_tampered_replier_index_fails() {
    let (signer, key) = keypair(1);
    let mut frame = signed_frame(&signer);
    frame[0] = 2;
    assert_eq!(verify_reply_frames(&key, &[&frame]), vec![false]);
}

#[test]
fn the_wrong_key_fails() {
    let (signer, _) = keypair(1);
    let (_, other) = keypair(2);
    let frame = signed_frame(&signer);
    assert_eq!(verify_reply_frames(&other, &[&frame]), vec![false]);
}

#[test]
fn a_frame_shorter_than_a_signature_fails() {
    let (_, key) = keypair(1);
    assert_eq!(verify_reply_frames(&key, &[vec![0u8; 10]]), vec![false]);
}
