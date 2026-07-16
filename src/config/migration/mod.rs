//! Config schema migration implementation.
//!
//! This module owns durable primary-config upgrades. Runtime config loading
//! calls this before normal validation so user config files can move forward
//! through schema versions while project overlays remain validated against the
//! current schema.

use super::{
    ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Path, Result, extract_config_values, fs,
    parse_config_json_object, write_private_config_file,
};

mod driver;
mod ops;
mod v01_v06;
mod v07_v12;
mod v13_v19;

#[cfg(test)]
pub use driver::migrate_config_text;
pub(in crate::config) use driver::parse_config_schema_version;
pub use driver::{CURRENT_CONFIG_SCHEMA_VERSION, migrate_config_file};
