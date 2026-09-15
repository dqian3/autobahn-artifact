//! Wire format shared by the benchmark client and the replicas that answer it.
//!
//! Autobahn's published client is send-only: it writes transactions at a
//! target rate and never reads its socket, so a request is never *finished*
//! from the client's point of view and its latency has to be reconstructed
//! offline by matching client send logs against commit lines in a primary's
//! log — two clocks on two machines. Every other protocol in the evaluation
//! answers its clients and measures latency at one of them. This is the reply
//! path that lets autobahn do the same.
//!
//! **Where a reply goes.** Each client's reply address travels inside the
//! transaction itself, at a fixed offset. Putting it in the bytes rather than
//! in a map on the receiving worker is what lets *any* replica reply: every
//! replica stores every batch, so every replica can read the address. A
//! worker-side map would only work on the one worker the client happens to be
//! connected to. It also means nothing has to be remembered per in-flight
//! request, so there is no map to bound and no leak when a client
//! disconnects.
//!
//! Transaction layout, ahead of the trailing 64-byte client signature:
//!
//! ```text
//! byte  0        kind: 0 = sampled (latency-tracked), 1 = ordinary
//! bytes 1..9     transaction id (u64, big endian)
//! bytes 9..13    client reply address, IPv4
//! bytes 13..15   client reply port (u16, big endian); 0 = do not reply
//! bytes 15..     padding out to the configured transaction size
//! ```
//!
//! Bytes 0..9 already existed. The 6 address bytes are new and land inside
//! the configured transaction size, so the wire size of a run is unchanged;
//! this widens the existing header-inside-the-payload deviation from 9 B to
//! 15 B, which is why the minimum transaction size is what it is.
//!
//! A port of 0 means the sender wants no reply, which is what a client built
//! before this change produces: it zero-fills the payload after the id, so
//! old clients and `client_reply_count: 0` both come out as "no reply".
//!
//! **What a reply says.** Nothing beyond "this committed": a reply is the
//! request's own first 9 bytes echoed back, so the client can match it. By
//! default it is not signed and carries no proof. Each frame starts with one
//! byte, the replying replica's index in committee order, so a client waiting
//! for more than one reply can tell two replicas apart from one replica
//! answering twice.
//!
//! ```text
//! unsigned:  [replier index][tag]...
//! signed:    [replier index]([tag][64-byte Ed25519 signature])...
//! ```
//!
//! With `client_reply_signed` on, each tag carries the replier's own
//! signature over the digest of `replier index || tag`, binding the reply to
//! both the replica and the request: one signature per request per replying
//! replica. A verifying client checks each against that replica's committee
//! key before counting the tag.
//!
//! That is a deliberate choice about what to charge autobahn for. A per-
//! request signature is the shape *aspen's* fast path is obliged to use,
//! because it runs no consensus and the client's only evidence is a set of
//! matching signed replies. Autobahn runs consensus and already produces a
//! quorum certificate per commit, so making it sign per request would be
//! billing it for a mechanism its design does not need. Banyan's client
//! reply is a bare unsigned ack for the same reason, so this also puts the
//! two competitors on the same footing.
//!
//! What remains, and what a run with this path measures, is the cost of
//! answering at all: the commit notification from primary to worker, reading
//! the batch back out of the store, and a reply per request on the wire.
//!
//! Replies for one committed batch bound for the same client are concatenated
//! into a single frame, so network cost stays proportional to batches rather
//! than to requests.

use crypto::{is_crypto_disabled, verify_transactions, Hash as _, PublicKey, Signer};
use std::convert::TryInto;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Bytes of the transaction echoed back so the client can match the reply.
pub const TAG_LEN: usize = 9;

/// One reply. The tag is the whole of it.
pub const REPLY_ENTRY_LEN: usize = TAG_LEN;

/// Leading byte of every reply frame: the replier's index in committee order.
pub const REPLY_HEADER_LEN: usize = 1;

/// Signature following each tag in a signed reply frame.
pub const REPLY_SIGNATURE_LEN: usize = 64;

/// One signed reply: a tag and its signature.
pub const SIGNED_REPLY_ENTRY_LEN: usize = TAG_LEN + REPLY_SIGNATURE_LEN;

/// Largest committee whose repliers a client can tell apart.
pub const MAX_REPLIERS: usize = 64;

/// Offset of the client's reply address within a transaction.
pub const REPLY_ADDR_OFFSET: usize = TAG_LEN;

/// IPv4 address plus port.
pub const REPLY_ADDR_LEN: usize = 6;

/// Smallest transaction payload that can carry a reply address.
pub const MIN_TX_LEN: usize = REPLY_ADDR_OFFSET + REPLY_ADDR_LEN;

/// Kind byte marking a transaction whose latency the client tracks.
pub const KIND_SAMPLE: u8 = 0;

/// Encode a reply address for the client to embed in its transactions.
///
/// Only IPv4 is supported: the address has to fit in the 16-byte minimum
/// transaction alongside the kind byte and the id, and every deployment this
/// benchmark runs on (loopback, and GCE internal addresses) is IPv4.
pub fn encode_reply_addr(addr: &SocketAddr) -> Option<[u8; REPLY_ADDR_LEN]> {
    let ip = match addr.ip() {
        IpAddr::V4(ip) => ip,
        IpAddr::V6(_) => return None,
    };
    let mut out = [0u8; REPLY_ADDR_LEN];
    out[..4].copy_from_slice(&ip.octets());
    out[4..].copy_from_slice(&addr.port().to_be_bytes());
    Some(out)
}

/// Read the reply address out of a transaction.
///
/// `None` when the transaction is too short to carry one or the client asked
/// for no reply (port 0).
pub fn decode_reply_addr(tx: &[u8]) -> Option<SocketAddr> {
    if tx.len() < MIN_TX_LEN {
        return None;
    }
    let octets: [u8; 4] = tx[REPLY_ADDR_OFFSET..REPLY_ADDR_OFFSET + 4]
        .try_into()
        .expect("slice is 4 bytes");
    let port = u16::from_be_bytes(
        tx[REPLY_ADDR_OFFSET + 4..REPLY_ADDR_OFFSET + REPLY_ADDR_LEN]
            .try_into()
            .expect("slice is 2 bytes"),
    );
    if port == 0 {
        return None;
    }
    Some(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3])),
        port,
    ))
}

/// Bytes a reply signature covers: the replier index then the tag.
fn reply_entry_message(replier: u8, tag: &[u8]) -> [u8; REPLY_HEADER_LEN + TAG_LEN] {
    let mut message = [0u8; REPLY_HEADER_LEN + TAG_LEN];
    message[0] = replier;
    message[REPLY_HEADER_LEN..].copy_from_slice(tag);
    message
}

/// Sign each tag in `tags` (concatenated, TAG_LEN apiece) as replier
/// `replier`, returning the signed entries (`tag || signature`).
pub fn sign_reply_entries(signer: &Signer, replier: u8, tags: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tags.len() / TAG_LEN * SIGNED_REPLY_ENTRY_LEN);
    for tag in tags.chunks_exact(TAG_LEN) {
        let message = reply_entry_message(replier, tag);
        let signature = signer.sign(&(&message[..]).digest()).flatten();
        out.extend_from_slice(tag);
        out.extend_from_slice(&signature);
    }
    out
}

/// Check signed entries (`tag || signature`) that claim to come from
/// replier `replier`, owning `key`.
///
/// One flag per entry. The entries are checked as one batch, then one by one
/// if the batch fails. Entries of the wrong length fail. Always true when
/// crypto is disabled, since replicas then sign with zeros.
pub fn verify_reply_entries<T: AsRef<[u8]>>(key: &PublicKey, replier: u8, entries: &[T]) -> Vec<bool> {
    if is_crypto_disabled() {
        return vec![true; entries.len()];
    }
    // Laid out as `message || signature`, the shape `verify_transactions` takes.
    let mut signed: Vec<[u8; REPLY_HEADER_LEN + SIGNED_REPLY_ENTRY_LEN]> =
        Vec::with_capacity(entries.len());
    let mut well_formed = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry = entry.as_ref();
        if entry.len() != SIGNED_REPLY_ENTRY_LEN {
            well_formed.push(false);
            continue;
        }
        let mut buf = [0u8; REPLY_HEADER_LEN + SIGNED_REPLY_ENTRY_LEN];
        buf[..REPLY_HEADER_LEN + TAG_LEN].copy_from_slice(&reply_entry_message(replier, &entry[..TAG_LEN]));
        buf[REPLY_HEADER_LEN + TAG_LEN..].copy_from_slice(&entry[TAG_LEN..]);
        signed.push(buf);
        well_formed.push(true);
    }
    let mut checked = verify_transactions(key, &signed).into_iter();
    well_formed
        .into_iter()
        .map(|ok| ok && checked.next().unwrap_or(false))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_an_address() {
        let addr: SocketAddr = "10.128.0.7:5107".parse().unwrap();
        let encoded = encode_reply_addr(&addr).unwrap();

        let mut tx = vec![0u8; 80];
        tx[REPLY_ADDR_OFFSET..REPLY_ADDR_OFFSET + REPLY_ADDR_LEN].copy_from_slice(&encoded);

        assert_eq!(decode_reply_addr(&tx), Some(addr));
    }

    #[test]
    fn a_zero_filled_payload_asks_for_no_reply() {
        // What a client built before this change sends: it zero-fills the
        // payload after the id, so the port reads as 0.
        assert_eq!(decode_reply_addr(&vec![0u8; 80]), None);
    }

    #[test]
    fn a_short_transaction_asks_for_no_reply() {
        assert_eq!(decode_reply_addr(&vec![1u8; MIN_TX_LEN - 1]), None);
    }

    #[test]
    fn ipv6_is_refused_rather_than_truncated() {
        assert!(encode_reply_addr(&"[::1]:5107".parse().unwrap()).is_none());
    }
}
