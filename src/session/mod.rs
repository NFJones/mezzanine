//! Product compatibility facade for the mux-owned session domain.

pub use mez_mux::session::*;

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
