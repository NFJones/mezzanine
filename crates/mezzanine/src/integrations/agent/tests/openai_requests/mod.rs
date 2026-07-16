//! Agent tests for openai requests behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

mod cache_shape;
mod messages;
mod options;
mod schemas;
