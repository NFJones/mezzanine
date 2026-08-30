//! Runtime-owned external-editor sessions.
//!
//! This subsystem owns private draft artifacts, pane-scoped edit leases,
//! editor command resolution, and completion records. Process launch remains
//! on the managed pane-shell transaction path so a blocking editor inherits
//! the pane PTY without becoming a model-authored shell action.

#![allow(
    dead_code,
    reason = "target-specific prompt and durable-record adapters land in dependent issues"
)]

mod artifacts;
mod command;
mod recovery;
mod runner;
mod service;
mod session;

pub(crate) use runner::run_internal_process as run_internal_editor_process;
pub(crate) use session::{
    ExternalEditTarget, ExternalEditorCompletion, ExternalEditorSessionStart,
    RuntimeExternalEditorComponent,
};
