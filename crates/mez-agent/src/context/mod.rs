//! Stable facade for provider-independent context contracts.
//!
//! The canonical owner retains private typed records and candidate-before-commit
//! mutation. Its focused children handle chronology, compaction, legacy history,
//! validation, request projection, and diagnostics without independent stores.
//! Product transcript persistence and provider execution stay outside this crate.

mod canonical;

pub(crate) use canonical::ProviderRequestEpoch;
pub use canonical::*;
