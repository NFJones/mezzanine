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
mod v41_v42;
mod v42_v43;
mod v43_v44;
mod v44_v45;
mod v45_v46;
mod v46_v47;
mod v47_v48;
mod v48_v49;
mod v49_v50;
mod v50_v51;
mod v51_v52;
mod v52_v53;
mod v53_v54;
mod v54_v55;
mod v55_v56;
mod v56_v57;
mod v57_v58;
mod v58_v59;
mod v59_v60;
mod v60_v61;
mod v61_v62;
mod v62_v63;
mod v63_v64;
mod v64_v65;
mod v65_v66;
mod v66_v67;
mod v67_v68;
mod v68_v69;
mod v69_v70;
mod v70_v71;
mod v71_v72;
mod v72_v73;
mod v73_v74;
mod v74_v75;
mod v75_v76;
mod v76_v77;
mod v77_v78;
mod v78_v79;
mod v79_v80;
mod v80_v81;
mod v81_v82;
mod v82_v83;
mod v83_v84;

#[cfg(test)]
pub use driver::migrate_config_text;
pub(in crate::config) use driver::parse_config_schema_version;
pub use driver::{CURRENT_CONFIG_SCHEMA_VERSION, migrate_config_file};
