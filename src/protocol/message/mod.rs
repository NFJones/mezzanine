//! Product transport adapters for the local-agent message protocol.
//!
//! Canonical MMP contracts and deterministic delivery state live in
//! `mez_agent::messaging`. This adapter owns content-length framing and
//! concrete fanout writes because those operations depend on product errors
//! and the root protocol transport.

#[cfg(test)]
mod fanout;
mod framing;

#[cfg(test)]
pub use fanout::{MessageFanoutSink, flush_message_fanout, flush_message_fanout_for};
pub use framing::{decode_mmp_frame, encode_mmp_body, handle_mmp_frame};

#[cfg(test)]
mod tests;
