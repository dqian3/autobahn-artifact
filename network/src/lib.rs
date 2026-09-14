// Copyright(C) Facebook, Inc. and its affiliates.
mod error;
mod receiver;
mod reliable_sender;
mod simple_sender;

#[cfg(test)]
#[path = "tests/common.rs"]
pub mod common;

pub use crate::receiver::{MessageHandler, Receiver, Writer};
pub use crate::reliable_sender::{CancelHandler, ReliableSender};
pub use crate::simple_sender::SimpleSender;

use std::sync::atomic::{AtomicBool, Ordering};

/// When true, every socket this crate opens or accepts sets TCP_NODELAY,
/// so small frames go out immediately instead of waiting for the previous
/// segment's ACK. Set once at startup from `Parameters::tcp_nodelay` (see
/// `node/src/main.rs`), before any connection is made.
static TCP_NODELAY: AtomicBool = AtomicBool::new(false);

pub fn set_tcp_nodelay(enabled: bool) {
    TCP_NODELAY.store(enabled, Ordering::Relaxed);
}

#[inline]
pub fn tcp_nodelay() -> bool {
    TCP_NODELAY.load(Ordering::Relaxed)
}
