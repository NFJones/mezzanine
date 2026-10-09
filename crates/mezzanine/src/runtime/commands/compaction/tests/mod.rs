//! Regression tests for private manual compaction projections.
//!
//! These tests exercise valid typed context without exposing owner APIs or
//! relaxing canonical causal validation for runtime consumers.

use super::*;

mod replay;
