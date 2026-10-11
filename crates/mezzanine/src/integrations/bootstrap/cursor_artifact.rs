//! Cursor CLI hook members, separate from shared document/settings authority.
//!
//! CLI changelog confirms sessionStart/sessionEnd/stop and stdin hooks. The shared
//! hooks reference supplies version1 command entries and neutral empty outputs.
//! Only exact members are owned; version1 is initialized only for an absent file.
//! Installation never attests runtime session association or usage availability.

use super::{
    installer::Manifest,
    reconciliation::{Artifact, Entry, JsonArrayMember},
};

/// Compiles only a fixed installing executable, never vendor paths or PATH/env.
pub(super) fn manifest() -> Option<Manifest> {
    let helper = std::env::current_exe().ok()?;
    let text = helper.to_str()?;
    if !helper.is_absolute() || text.len() > 4096 || text.chars().any(char::is_control) {
        return None;
    }
    let quoted = shlex::try_quote(text).ok()?;
    Some(Manifest { harness:"cursor".into(), revision:1, vendor_version:"best-effort".into(), entries:vec![Entry {
        path:"hooks.json".into(), artifact:Artifact::JsonArrayEntries { entries:["sessionStart","stop","sessionEnd"].into_iter().map(|event| JsonArrayMember {
            pointer:format!("/hooks/{event}"), value:serde_json::json!({"command":format!("test -x {quoted} && exec {quoted} cursor-hook {event} || printf '{{}}\\n'"),"timeout":1}),
        }).collect() },
    }] })
}

/// Validates existing shared format without overwriting authored version fields.
/// Only a missing install target receives minimal version1 seed bytes; missing
/// uninstall targets stay missing and existing JSON is retained byte-for-byte.
pub(super) fn document(current: Option<&[u8]>, installing: bool) -> crate::Result<Option<Vec<u8>>> {
    match current {
        Some(bytes) => {
            let value = super::strict_json::decode(bytes)?;
            if value["version"] != 1 {
                return Err(crate::MezError::conflict(
                    "Cursor shared hook version is unsupported",
                ));
            }
            Ok(Some(bytes.to_vec()))
        }
        None if installing => Ok(Some(b"{\"version\":1,\"hooks\":{}}\n".to_vec())),
        None => Ok(None),
    }
}

/// Reduces already-qualified previous ownership to exact currently present
/// members. Missing objects/arrays/members create no removal authority; foreign
/// values are retained. Wrong-type containers still reject, including uninstall.
pub(super) fn present_ownership(
    current: Option<&[u8]>,
    previous: Option<&Artifact>,
) -> crate::Result<Option<Artifact>> {
    let (Some(bytes), Some(previous)) = (current, previous) else {
        return Ok(None);
    };
    let Artifact::JsonArrayEntries { entries } = previous else {
        return Err(crate::MezError::conflict(
            "Cursor shared ownership kind changed",
        ));
    };
    let document = super::strict_json::decode(bytes)?;
    if document
        .get("hooks")
        .is_some_and(|hooks| !hooks.is_object())
    {
        return Err(crate::MezError::conflict(
            "Cursor shared hooks must be an object",
        ));
    }
    let mut present = Vec::new();
    for entry in entries {
        if let Some(values) = document.pointer(&entry.pointer) {
            let values = values
                .as_array()
                .ok_or_else(|| crate::MezError::conflict("Cursor shared hook must be an array"))?;
            if values.iter().any(|value| value == &entry.value) {
                present.push(entry.clone());
            }
        }
    }
    Ok((!present.is_empty()).then_some(Artifact::JsonArrayEntries { entries: present }))
}

#[cfg(test)]
mod tests {
    /// Public compiled Cursor hooks must initialize a new version1 document and
    /// own only exact event members. Authored siblings/metadata/order survive
    /// repeat/uninstall, while unsupported versions reject without mutation.
    #[test]
    fn bootstrap_cursor_shared_hooks_preserve_authors_and_version() {
        use super::super::installer::{Operation, plan_private};
        let manifest =
            super::super::compiled_manifest("cursor", None).expect("compiled Cursor adapter");
        let home = std::env::temp_dir().join(format!(
            "mez-cursor-shared-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&home).unwrap();
        let root = home.join("vendor");
        std::fs::create_dir(&root).unwrap();
        let plan = |operation| plan_private(&root, &home, &manifest, operation);
        plan(Operation::Install).unwrap().apply().unwrap();
        let file = root.join("hooks.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(value["version"], 1);
        value["authored"] = true.into();
        let own = value["hooks"]["stop"][0].clone();
        value["hooks"]["stop"] = serde_json::json!([{"command":"AUTHOR BEFORE","timeout":9},own,{"command":"AUTHOR AFTER"}]);
        std::fs::write(&file, value.to_string()).unwrap();
        assert!(plan(Operation::Install).unwrap().changed_paths().is_empty());
        // Partial losses keep the shared file and qualified receipt intact.
        for loss in ["member", "array", "parent"] {
            let mut missing = value.clone();
            match loss {
                "member" => {
                    missing["hooks"]["stop"].as_array_mut().unwrap().remove(1);
                }
                "array" => {
                    missing["hooks"].as_object_mut().unwrap().remove("stop");
                }
                _ => {
                    missing.as_object_mut().unwrap().remove("hooks");
                }
            }
            std::fs::write(&file, missing.to_string()).unwrap();
            plan(Operation::Install).unwrap().apply().unwrap();
            let restored: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            assert_eq!(restored["version"], 1);
            assert_eq!(restored["authored"], true);
            assert!(
                restored["hooks"]["stop"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry == &value["hooks"]["stop"][1])
            );
            if loss == "member" {
                assert_eq!(restored["hooks"]["stop"][0], value["hooks"]["stop"][0]);
                assert_eq!(restored["hooks"]["stop"][1], value["hooks"]["stop"][2]);
            }
            std::fs::write(&file, value.to_string()).unwrap();
        }
        for loss in ["member", "array", "parent"] {
            let mut missing = value.clone();
            match loss {
                "member" => {
                    missing["hooks"]["stop"].as_array_mut().unwrap().remove(1);
                }
                "array" => {
                    missing["hooks"].as_object_mut().unwrap().remove("stop");
                }
                _ => {
                    missing.as_object_mut().unwrap().remove("hooks");
                }
            }
            std::fs::write(&file, missing.to_string()).unwrap();
            plan(Operation::Uninstall).unwrap().apply().unwrap();
            let remaining: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            assert_eq!(remaining["authored"], true);
            if loss == "member" {
                assert_eq!(
                    remaining["hooks"]["stop"],
                    serde_json::json!([value["hooks"]["stop"][0], value["hooks"]["stop"][2]])
                );
            }
            plan(Operation::Install).unwrap().apply().unwrap();
            std::fs::write(&file, value.to_string()).unwrap();
        }
        for malformed in [
            serde_json::json!({"version":1,"hooks":"FOREIGN"}),
            serde_json::json!({"version":1,"hooks":{"stop":"FOREIGN"}}),
        ] {
            let before = malformed.to_string();
            std::fs::write(&file, &before).unwrap();
            assert!(plan(Operation::Install).is_err());
            assert!(plan(Operation::Uninstall).is_err());
            assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
        }
        std::fs::write(&file, value.to_string()).unwrap();
        // Receipt ownership permits collapsing only exact duplicate members.
        let mut duplicate = value.clone();
        duplicate["hooks"]["stop"]
            .as_array_mut()
            .unwrap()
            .push(value["hooks"]["stop"][1].clone());
        std::fs::write(&file, duplicate.to_string()).unwrap();
        plan(Operation::Install).unwrap().apply().unwrap();
        let repaired: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(repaired["hooks"]["stop"].as_array().unwrap().len(), 3);
        plan(Operation::Uninstall).unwrap().apply().unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["authored"], true);
        assert_eq!(
            value["hooks"]["stop"],
            serde_json::json!([{"command":"AUTHOR BEFORE","timeout":9},{"command":"AUTHOR AFTER"}])
        );
        std::fs::write(&file, b"{\"version\":99,\"hooks\":{}}").unwrap();
        assert!(plan(Operation::Install).is_err());
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"{\"version\":99,\"hooks\":{}}"
        );
        std::fs::remove_file(&file).unwrap();
        plan(Operation::Install).unwrap().apply().unwrap();
        std::fs::remove_file(&file).unwrap();
        plan(Operation::Install).unwrap().apply().unwrap();
        let repaired: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(repaired["version"], 1);
        std::fs::remove_dir_all(home).unwrap();
    }
}
