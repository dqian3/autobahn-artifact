//! Wire format shared by the benchmark client and the replicas that answer it.
//!
//! Autobahn's published client is send-only: it writes transactions at a
//! target rate and never reads its socket, so a replica pays nothing to
//! "finish" a request. Aspen's replicas sign a reply per request so the
//! client can assemble a commit certificate, which is a per-request
//! signature on every replica. Comparing the two on throughput means making
//! autobahn do the same work, which needs two things the published protocol
//! does not have: somewhere to send the reply, and something to send.
//!
//! **Where.** Each client's reply address travels inside the transaction
//! itself, at a fixed offset. Putting it in the bytes rather than in a map
//! on the receiving worker is what lets *any* replica reply: every replica
//! stores every batch, so every replica can read the address. A worker-side
//! map would only work on the one worker the client happens to be connected
//! to. It also means nothing has to be remembered per in-flight request, so
//! there is no map to bound and no leak when a client disconnects.
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
//! **What.** A reply is a signature over the request digest — the same
//! digest the client signed — tagged with the request's first 9 bytes so the
//! client can match it. Replies for one committed batch destined for the
//! same client are concatenated into a single frame, so the network cost
//! stays proportional to batches while the signing cost stays proportional
//! to requests, which is the cost being measured.

use std::convert::TryInto;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Bytes of the transaction echoed back so the client can match the reply.
pub const TAG_LEN: usize = 9;

/// An ed25519 signature.
pub const SIG_LEN: usize = 64;

/// One reply: the request tag followed by the replica's signature.
pub const REPLY_ENTRY_LEN: usize = TAG_LEN + SIG_LEN;

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
}
