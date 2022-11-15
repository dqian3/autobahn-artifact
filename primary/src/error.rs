// Copyright(C) Facebook, Inc. and its affiliates.
use crate::primary::{Round, View};
use crypto::{CryptoError, Digest, PublicKey};
use store::StoreError;
use thiserror::Error;

#[macro_export]
macro_rules! bail {
    ($e:expr) => {
        return Err($e);
    };
}

#[macro_export(local_inner_macros)]
macro_rules! ensure {
    ($cond:expr, $e:expr) => {
        if !($cond) {
            bail!($e);
        }
    };
}


pub type DagResult<T> = Result<T, DagError>;

#[derive(Debug, Error)]
pub enum DagError {
    #[error("Invalid signature")]
    InvalidSignature(#[from] CryptoError),

    #[error("Storage failure: {0}")]
    StoreError(#[from] StoreError),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] Box<bincode::ErrorKind>),

    #[error("Invalid header id")]
    InvalidHeaderId,

    #[error("Malformed header {0}")]
    MalformedHeader(Digest),

    #[error("Received message from unknown authority {0}")]
    UnknownAuthority(PublicKey),

    #[error("Authority {0} appears in quorum more than once")]
    AuthorityReuse(PublicKey),

    #[error("Received unexpected vote fo header {0}")]
    UnexpectedVote(Digest),

    #[error("Received certificate without a quorum")]
    CertificateRequiresQuorum,

    #[error("Parents of header {0} are not a quorum")]
    HeaderRequiresQuorum(Digest),

    #[error("Header {0} (round {1}) too old")]
    HeaderTooOld(Digest, Round),

    #[error("Vote {0} (round {1}) too old")]
    VoteTooOld(Digest, Round),

    #[error("Certificate {0} (round {1}) too old")]
    CertificateTooOld(Digest, Round),

    #[error("Invalid vote invalidation")]
    InvalidVoteInvalidation,

    #[error("Invalid special parent")]
    InvalidSpecialParent,
}



pub type ConsensusResult<T> = Result<T, ConsensusError>;

#[derive(Error, Debug)]
pub enum ConsensusError {
    #[error("Network error: {0}")]
    NetworkError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] Box<bincode::ErrorKind>),

    #[error("Store error: {0}")]
    StoreError(#[from] StoreError),

    #[error("Node {0} is not in the committee")]
    NotInCommittee(PublicKey),

    #[error("Invalid signature")]
    InvalidSignature(#[from] CryptoError),

    #[error("Received more than one vote from {0}")]
    AuthorityReuse(PublicKey),

    #[error("Received vote from unknown authority {0}")]
    UnknownAuthority(PublicKey),

    #[error("Received QC without a quorum")]
    QCRequiresQuorum,

    #[error("Received TC without a quorum")]
    TCRequiresQuorum,

    #[error("Malformed block {0}")]
    MalformedBlock(Digest),

    #[error("Received block {digest} from leader {leader} at view {view}")]
    WrongLeader {
        digest: Digest,
        leader: PublicKey,
        view: View,
    },

    #[error("Invalid payload")]
    InvalidPayload,

    #[error("Message {0} (view {1}) too old")]
    TooOld(Digest, View),

    #[error(transparent)]
    DagError(#[from] DagError),

    #[error("Header proposer != block leader")]
    WrongProposer,

    #[error("Received block for round {0} smaller than current_round {1}")]
    NonMonotonicRounds(Round, Round),
        
}