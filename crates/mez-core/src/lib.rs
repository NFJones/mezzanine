//! Stable contracts shared by independent Mezzanine subsystems.
//!
//! This crate is the dependency root for the Mezzanine workspace. It will own
//! only low-dependency value types that have multiple lower-level consumers;
//! product policy, I/O, persistence, and general-purpose helpers do not belong
//! here. The initial empty facade establishes that boundary before production
//! types are extracted from the root package.
