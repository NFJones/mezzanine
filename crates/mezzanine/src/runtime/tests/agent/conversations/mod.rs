//! Runtime tests for agent conversations behavior.

use super::*;

mod bookkeeping;
mod fork_epoch;
/// Exact resumed-source setup shared by source and inherited-child owners.
mod fork_fixtures;
/// Child initial projection, model context and deferred publication ownership.
mod fork_inheritance;
/// Immutable compacted-parent source and cutoff qualification.
mod fork_source;
mod lifecycle;
mod metadata;
mod positions;
mod resume_cleanup;
mod resume_prompt;
mod saved_sessions;
mod session_titles;
mod snapshots;
