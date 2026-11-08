//! Shared crate-internal runtime test support.
//!
//! This module contains only runtime fixtures used by more than one owning test
//! tree. Single-owner fixtures stay beside their subsystem tests.
#![allow(dead_code)]

pub(crate) mod compaction_publication;
pub(crate) mod compaction_source;
pub(crate) mod replay_epoch;
pub(crate) mod runtime;
