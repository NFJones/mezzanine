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
mod v20_v21;
mod v21_v22;
mod v22_v23;
mod v23_v24;
mod v24_v25;
mod v25_v26;
mod v26_v27;
mod v27_v28;
mod v28_v29;
mod v29_v30;
mod v30_v31;
mod v31_v32;
mod v32_v33;
mod v33_v34;
mod v34_v35;
mod v35_v36;
mod v36_v37;
mod v37_v38;
mod v38_v39;
mod v39_v40;
mod v40_v41;

#[cfg(test)]
pub use driver::migrate_config_text;
pub(in crate::config) use driver::parse_config_schema_version;
pub use driver::{CURRENT_CONFIG_SCHEMA_VERSION, migrate_config_file};
