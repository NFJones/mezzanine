//! Receive commit and presentation settlement regression owners.
//!
//! Committed peer context and acknowledgement precede durable presentation;
//! neither recovery path may replay a canonical event.

use super::*;

mod context_commit;
mod presentation_settlement;
