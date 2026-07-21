//! Config migration planning, schema detection, file persistence, and step dispatch.

use super::v01_v06::{
    migrate_v1_to_v2, migrate_v2_to_v3, migrate_v3_to_v4, migrate_v4_to_v5, migrate_v5_to_v6,
    migrate_v6_to_v7, migrate_v7_to_v8, migrate_v8_to_v9, migrate_v9_to_v10,
};
use super::v07_v12::{migrate_v10_to_v11, migrate_v11_to_v12, migrate_v12_to_v13};
use super::v13_v19::{
    migrate_v13_to_v14, migrate_v14_to_v15, migrate_v15_to_v16, migrate_v16_to_v17,
    migrate_v17_to_v18, migrate_v18_to_v19, migrate_v19_to_v20,
};
use super::v20_v21::migrate_v20_to_v21;
use super::v21_v22::migrate_v21_to_v22;
use super::{
    ConfigFormat, MezError, Path, Result, extract_config_values, fs, write_private_config_file,
};

/// The newest configuration schema version understood by this binary.
pub const CURRENT_CONFIG_SCHEMA_VERSION: u64 = 22;

/// Describes the result of migrating one configuration document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigMigrationPlan {
    /// The schema version detected before migration.
    pub from_version: u64,
    /// The schema version after applying all known migrations.
    pub to_version: u64,
    /// Whether the migration produced different config text.
    pub changed: bool,
    /// The migrated configuration text.
    pub text: String,
}

/// Migrates a primary configuration file to the current schema version.
///
/// # Parameters
/// - `path`: The primary config file to inspect and update if needed.
pub fn migrate_config_file(path: &Path) -> Result<ConfigMigrationPlan> {
    let format = ConfigFormat::from_path(path)?;
    let text = fs::read_to_string(path)?;
    let plan = migrate_config_text(format, &text)?;
    if plan.changed {
        write_private_config_file(path, &plan.text)?;
    }
    Ok(plan)
}

/// Migrates one configuration document to the current schema version.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to migrate.
pub fn migrate_config_text(format: ConfigFormat, text: &str) -> Result<ConfigMigrationPlan> {
    let from_version = config_schema_version(format, text)?;
    if from_version > CURRENT_CONFIG_SCHEMA_VERSION {
        return Err(MezError::config(format!(
            "configuration schema version {from_version} is newer than this mez binary supports ({CURRENT_CONFIG_SCHEMA_VERSION})"
        )));
    }

    let mut current_version = from_version;
    let mut current_text = text.to_string();
    while current_version < CURRENT_CONFIG_SCHEMA_VERSION {
        match current_version {
            1 => {
                current_text = migrate_v1_to_v2(format, &current_text)?;
                current_version = 2;
            }
            2 => {
                current_text = migrate_v2_to_v3(format, &current_text)?;
                current_version = 3;
            }
            3 => {
                current_text = migrate_v3_to_v4(format, &current_text)?;
                current_version = 4;
            }
            4 => {
                current_text = migrate_v4_to_v5(format, &current_text)?;
                current_version = 5;
            }
            5 => {
                current_text = migrate_v5_to_v6(format, &current_text)?;
                current_version = 6;
            }
            6 => {
                current_text = migrate_v6_to_v7(format, &current_text)?;
                current_version = 7;
            }
            7 => {
                current_text = migrate_v7_to_v8(format, &current_text)?;
                current_version = 8;
            }
            8 => {
                current_text = migrate_v8_to_v9(format, &current_text)?;
                current_version = 9;
            }
            9 => {
                current_text = migrate_v9_to_v10(format, &current_text)?;
                current_version = 10;
            }
            10 => {
                current_text = migrate_v10_to_v11(format, &current_text)?;
                current_version = 11;
            }
            11 => {
                current_text = migrate_v11_to_v12(format, &current_text)?;
                current_version = 12;
            }
            12 => {
                current_text = migrate_v12_to_v13(format, &current_text)?;
                current_version = 13;
            }
            13 => {
                current_text = migrate_v13_to_v14(format, &current_text)?;
                current_version = 14;
            }
            14 => {
                current_text = migrate_v14_to_v15(format, &current_text)?;
                current_version = 15;
            }
            15 => {
                current_text = migrate_v15_to_v16(format, &current_text)?;
                current_version = 16;
            }
            16 => {
                current_text = migrate_v16_to_v17(format, &current_text)?;
                current_version = 17;
            }
            17 => {
                current_text = migrate_v17_to_v18(format, &current_text)?;
                current_version = 18;
            }
            18 => {
                current_text = migrate_v18_to_v19(format, &current_text)?;
                current_version = 19;
            }
            19 => {
                current_text = migrate_v19_to_v20(format, &current_text)?;
                current_version = 20;
            }
            20 => {
                current_text = migrate_v20_to_v21(format, &current_text)?;
                current_version = 21;
            }
            21 => {
                current_text = migrate_v21_to_v22(format, &current_text)?;
                current_version = 22;
            }
            unsupported => {
                return Err(MezError::config(format!(
                    "no migration path is available from configuration schema version {unsupported}"
                )));
            }
        }
    }

    Ok(ConfigMigrationPlan {
        from_version,
        to_version: CURRENT_CONFIG_SCHEMA_VERSION,
        changed: current_text != text,
        text: current_text,
    })
}

/// Reads the schema version recorded in one config document.
///
/// # Parameters
/// - `format`: The concrete config file format.
/// - `text`: The document text to inspect.
pub(super) fn config_schema_version(format: ConfigFormat, text: &str) -> Result<u64> {
    let values = extract_config_values(format, text);
    parse_config_schema_version(values.get("version").map(String::as_str))
}

/// Parses an optional config schema version value.
///
/// # Parameters
/// - `value`: The raw extracted version value, if present.
pub(in crate::config) fn parse_config_schema_version(value: Option<&str>) -> Result<u64> {
    let Some(value) = value else {
        return Ok(1);
    };
    match value.parse::<u64>() {
        Ok(version) if version > 0 => Ok(version),
        _ => Err(MezError::config(
            "configuration schema version must be a positive integer",
        )),
    }
}
