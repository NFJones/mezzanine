//! Shared crate-internal test support.
//!
//! These helpers collect high-reuse fixtures that previously lived inside
//! large subsystem test files. They are available only to test builds and
//! should stay focused on setup mechanics rather than behavior assertions.
#![allow(dead_code)]

pub(crate) mod agent;
pub(crate) mod async_runtime;
pub(crate) mod control;
pub(crate) mod runtime;
pub(crate) mod temp;
