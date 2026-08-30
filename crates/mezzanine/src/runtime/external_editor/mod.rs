//! Runtime-owned external-editor sessions.
//!
//! This subsystem owns private draft artifacts, pane-scoped edit leases,
//! editor command resolution, direct server-local PTY processes, independent
//! terminal screens, and completion records. Pane shells are never involved.

#![allow(
    dead_code,
    reason = "target-specific prompt and durable-record adapters land in dependent issues"
)]

mod artifacts;
mod command;
mod durable;
mod recovery;
mod runner;
mod service;
mod session;

pub(crate) use runner::run_internal_process as run_internal_editor_process;
pub(crate) use session::{
    ExternalEditTarget, ExternalEditorCompletion, ExternalEditorSessionStart,
    RuntimeExternalEditorComponent,
};
