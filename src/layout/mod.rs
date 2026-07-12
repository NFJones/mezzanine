//! Transitional product facade for the extracted multiplexer layout domain.
//!
//! New lower-level code should import layout contracts from `mez_mux::layout`.
//! This facade keeps existing product adapters stable while session ownership
//! is moved into the mux crate.

pub use mez_mux::layout::*;
