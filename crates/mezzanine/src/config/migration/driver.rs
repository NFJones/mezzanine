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
use super::v22_v23::migrate_v22_to_v23;
use super::v23_v24::migrate_v23_to_v24;
use super::v24_v25::migrate_v24_to_v25;
use super::v25_v26::migrate_v25_to_v26;
use super::v26_v27::migrate_v26_to_v27;
use super::v27_v28::migrate_v27_to_v28;
use super::v28_v29::migrate_v28_to_v29;
use super::v29_v30::migrate_v29_to_v30;
use super::v30_v31::migrate_v30_to_v31;
use super::v31_v32::migrate_v31_to_v32;
use super::v32_v33::migrate_v32_to_v33;
use super::v33_v34::migrate_v33_to_v34;
use super::v34_v35::migrate_v34_to_v35;
use super::v35_v36::migrate_v35_to_v36;
use super::v36_v37::migrate_v36_to_v37;
use super::v37_v38::migrate_v37_to_v38;
use super::v38_v39::migrate_v38_to_v39;
use super::v39_v40::migrate_v39_to_v40;
use super::v40_v41::migrate_v40_to_v41;
use super::v41_v42::migrate_v41_to_v42;
use super::v42_v43::migrate_v42_to_v43;
use super::v43_v44::migrate_v43_to_v44;
use super::v44_v45::migrate_v44_to_v45;
use super::v45_v46::migrate_v45_to_v46;
use super::v46_v47::migrate_v46_to_v47;
use super::v47_v48::migrate_v47_to_v48;
use super::v48_v49::migrate_v48_to_v49;
use super::v49_v50::migrate_v49_to_v50;
use super::v50_v51::migrate_v50_to_v51;
use super::v51_v52::migrate_v51_to_v52;
use super::v52_v53::migrate_v52_to_v53;
use super::v53_v54::migrate_v53_to_v54;
use super::v54_v55::migrate_v54_to_v55;
use super::v55_v56::migrate_v55_to_v56;
use super::v56_v57::migrate_v56_to_v57;
use super::v57_v58::migrate_v57_to_v58;
use super::v58_v59::migrate_v58_to_v59;
use super::v59_v60::migrate_v59_to_v60;
use super::v60_v61::migrate_v60_to_v61;
use super::v61_v62::migrate_v61_to_v62;
use super::v62_v63::migrate_v62_to_v63;
use super::v63_v64::migrate_v63_to_v64;
use super::v64_v65::migrate_v64_to_v65;
use super::v65_v66::migrate_v65_to_v66;
use super::v66_v67::migrate_v66_to_v67;
use super::v67_v68::migrate_v67_to_v68;
use super::v68_v69::migrate_v68_to_v69;
use super::v69_v70::migrate_v69_to_v70;
use super::v70_v71::migrate_v70_to_v71;
use super::v71_v72::migrate_v71_to_v72;
use super::v72_v73::migrate_v72_to_v73;
use super::v73_v74::migrate_v73_to_v74;
use super::v74_v75::migrate_v74_to_v75;
use super::{
    ConfigFormat, MezError, Path, Result, extract_config_values, fs, write_private_config_file,
};

/// The newest configuration schema version understood by this binary.
pub const CURRENT_CONFIG_SCHEMA_VERSION: u64 = 75;

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
            22 => {
                current_text = migrate_v22_to_v23(format, &current_text)?;
                current_version = 23;
            }
            23 => {
                current_text = migrate_v23_to_v24(format, &current_text)?;
                current_version = 24;
            }
            24 => {
                current_text = migrate_v24_to_v25(format, &current_text)?;
                current_version = 25;
            }
            25 => {
                current_text = migrate_v25_to_v26(format, &current_text)?;
                current_version = 26;
            }
            26 => {
                current_text = migrate_v26_to_v27(format, &current_text)?;
                current_version = 27;
            }
            27 => {
                current_text = migrate_v27_to_v28(format, &current_text)?;
                current_version = 28;
            }
            28 => {
                current_text = migrate_v28_to_v29(format, &current_text)?;
                current_version = 29;
            }
            29 => {
                current_text = migrate_v29_to_v30(format, &current_text)?;
                current_version = 30;
            }
            30 => {
                current_text = migrate_v30_to_v31(format, &current_text)?;
                current_version = 31;
            }
            31 => {
                current_text = migrate_v31_to_v32(format, &current_text)?;
                current_version = 32;
            }
            32 => {
                current_text = migrate_v32_to_v33(format, &current_text)?;
                current_version = 33;
            }
            33 => {
                current_text = migrate_v33_to_v34(format, &current_text)?;
                current_version = 34;
            }
            34 => {
                current_text = migrate_v34_to_v35(format, &current_text)?;
                current_version = 35;
            }
            35 => {
                current_text = migrate_v35_to_v36(format, &current_text)?;
                current_version = 36;
            }
            36 => {
                current_text = migrate_v36_to_v37(format, &current_text)?;
                current_version = 37;
            }
            37 => {
                current_text = migrate_v37_to_v38(format, &current_text)?;
                current_version = 38;
            }
            38 => {
                current_text = migrate_v38_to_v39(format, &current_text)?;
                current_version = 39;
            }
            39 => {
                current_text = migrate_v39_to_v40(format, &current_text)?;
                current_version = 40;
            }
            40 => {
                current_text = migrate_v40_to_v41(format, &current_text)?;
                current_version = 41;
            }
            41 => {
                current_text = migrate_v41_to_v42(format, &current_text)?;
                current_version = 42;
            }
            42 => {
                current_text = migrate_v42_to_v43(format, &current_text)?;
                current_version = 43;
            }
            43 => {
                current_text = migrate_v43_to_v44(format, &current_text)?;
                current_version = 44;
            }
            44 => {
                current_text = migrate_v44_to_v45(format, &current_text)?;
                current_version = 45;
            }
            45 => {
                current_text = migrate_v45_to_v46(format, &current_text)?;
                current_version = 46;
            }
            46 => {
                current_text = migrate_v46_to_v47(format, &current_text)?;
                current_version = 47;
            }
            47 => {
                current_text = migrate_v47_to_v48(format, &current_text)?;
                current_version = 48;
            }
            48 => {
                current_text = migrate_v48_to_v49(format, &current_text)?;
                current_version = 49;
            }
            49 => {
                current_text = migrate_v49_to_v50(format, &current_text)?;
                current_version = 50;
            }
            50 => {
                current_text = migrate_v50_to_v51(format, &current_text)?;
                current_version = 51;
            }
            51 => {
                current_text = migrate_v51_to_v52(format, &current_text)?;
                current_version = 52;
            }
            52 => {
                current_text = migrate_v52_to_v53(format, &current_text)?;
                current_version = 53;
            }
            53 => {
                current_text = migrate_v53_to_v54(format, &current_text)?;
                current_version = 54;
            }
            54 => {
                current_text = migrate_v54_to_v55(format, &current_text)?;
                current_version = 55;
            }
            55 => {
                current_text = migrate_v55_to_v56(format, &current_text)?;
                current_version = 56;
            }
            56 => {
                current_text = migrate_v56_to_v57(format, &current_text)?;
                current_version = 57;
            }
            57 => {
                current_text = migrate_v57_to_v58(format, &current_text)?;
                current_version = 58;
            }
            58 => {
                current_text = migrate_v58_to_v59(format, &current_text)?;
                current_version = 59;
            }
            59 => {
                current_text = migrate_v59_to_v60(format, &current_text)?;
                current_version = 60;
            }
            60 => {
                current_text = migrate_v60_to_v61(format, &current_text)?;
                current_version = 61;
            }
            61 => {
                current_text = migrate_v61_to_v62(format, &current_text)?;
                current_version = 62;
            }
            62 => {
                current_text = migrate_v62_to_v63(format, &current_text)?;
                current_version = 63;
            }
            63 => {
                current_text = migrate_v63_to_v64(format, &current_text)?;
                current_version = 64;
            }
            64 => {
                current_text = migrate_v64_to_v65(format, &current_text)?;
                current_version = 65;
            }
            65 => {
                current_text = migrate_v65_to_v66(format, &current_text)?;
                current_version = 66;
            }
            66 => {
                current_text = migrate_v66_to_v67(format, &current_text)?;
                current_version = 67;
            }
            67 => {
                current_text = migrate_v67_to_v68(format, &current_text)?;
                current_version = 68;
            }
            68 => {
                current_text = migrate_v68_to_v69(format, &current_text)?;
                current_version = 69;
            }
            69 => {
                current_text = migrate_v69_to_v70(format, &current_text)?;
                current_version = 70;
            }
            70 => {
                current_text = migrate_v70_to_v71(format, &current_text)?;
                current_version = 71;
            }
            71 => {
                current_text = migrate_v71_to_v72(format, &current_text)?;
                current_version = 72;
            }
            72 => {
                current_text = migrate_v72_to_v73(format, &current_text)?;
                current_version = 73;
            }
            73 => {
                current_text = migrate_v73_to_v74(format, &current_text)?;
                current_version = 74;
            }
            74 => {
                current_text = migrate_v74_to_v75(format, &current_text)?;
                current_version = 75;
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
