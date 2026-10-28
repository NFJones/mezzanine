//! Codex shared-hook ownership through real private planning/publication/recovery.
//!
//! Test-owned HOME/root fixtures preserve authored callback fields and array order;
//! no vendor callback, credential, actual user configuration or provider is used.

use super::*;
use crate::integrations::bootstrap::reconciliation::Artifact;

/// Malformed JSON, ambiguous keys/types and unreceipted exact registrations
/// never become ownership. Under a valid receipt, missing or edited members in
/// an existing shared document still refuse publication; genuine whole-file
/// absence has separate qualified repair coverage.
#[test]
fn bootstrap_codex_shared_hooks_conflicts_remain_non_mutating() {
    let current = crate::integrations::bootstrap::compiled_manifest("codex", None).unwrap();
    for input in [
        b"// authored comment\n{}".as_slice(),
        br#"{"hooks":[],"authored":true}"#,
        br#"{"hooks":{"Stop":false}}"#,
        br#"{"hooks":{},"hooks":{}}"#,
        br#"{"hooks":{"Stop":[],"\u0053top":[]}}"#,
    ] {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("hooks.json"), input).unwrap();
        let before = tree_snapshot(&fixture.workspace);
        assert!(fixture.plan(&current, Operation::Install).is_err());
        assert_eq!(tree_snapshot(&fixture.workspace), before);
    }
    let old = crate::integrations::bootstrap::compiled_history(&current)
        .pop()
        .unwrap();
    let old_bytes = match &old.entries[0].artifact {
        crate::integrations::bootstrap::reconciliation::Artifact::File { bytes } => bytes,
        _ => unreachable!(),
    };
    let fixture = Fixture::new();
    fs::write(fixture.root.join("hooks.json"), old_bytes).unwrap();
    let before = tree_snapshot(&fixture.workspace);
    assert!(fixture.plan(&current, Operation::Install).is_err());
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    for old_receipt in [false, true] {
        for fault in ["missing", "edited"] {
            let fixture = Fixture::new();
            fixture
                .plan(
                    if old_receipt { &old } else { &current },
                    Operation::Install,
                )
                .unwrap()
                .apply()
                .unwrap();
            let file = fixture.root.join("hooks.json");
            let mut document: serde_json::Value =
                serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
            if fault == "missing" {
                document["hooks"]["Stop"] = serde_json::json!([]);
            } else {
                document["hooks"]["Stop"][0]["hooks"][0]["timeout"] = 42.into();
            }
            fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
            let before = tree_snapshot(&fixture.workspace);
            for operation in [Operation::Install, Operation::Uninstall] {
                assert!(
                    fixture.plan(&current, operation).is_err(),
                    "{old_receipt}/{fault}"
                );
                assert_eq!(tree_snapshot(&fixture.workspace), before);
            }
        }
    }
}

/// Current and frozen historical receipts may recreate only a genuinely absent
/// shared hook file from compiled current members. Missing-file uninstall is a
/// no-op; sibling data and preview trees remain untouched. No old byte/pointer
/// is copied from a journal or receipt into new filesystem authority.
#[test]
fn bootstrap_codex_shared_missing_file_repairs_current_and_historical_receipts() {
    let current = crate::integrations::bootstrap::compiled_manifest("codex", None).unwrap();
    for source in std::iter::once(current.clone())
        .chain(crate::integrations::bootstrap::compiled_history(&current))
    {
        for operation in [Operation::Install, Operation::Uninstall] {
            let fixture = Fixture::new();
            fixture
                .plan(&source, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            let file = fixture.root.join("hooks.json");
            fs::remove_file(&file).unwrap();
            fs::write(fixture.root.join("unrelated.json"), b"AUTHORED SIBLING").unwrap();
            let before = tree_snapshot(&fixture.workspace);
            let plan = fixture.plan(&current, operation).unwrap();
            assert_eq!(tree_snapshot(&fixture.workspace), before);
            plan.apply().unwrap();
            if matches!(operation, Operation::Install) {
                let document: serde_json::Value =
                    serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
                assert_eq!(document["hooks"]["Stop"].as_array().unwrap().len(), 1);
                assert!(
                    fixture
                        .plan(&current, Operation::Install)
                        .unwrap()
                        .changed_paths()
                        .is_empty()
                );
            } else {
                assert!(!file.exists());
            }
            assert_eq!(
                fs::read(fixture.root.join("unrelated.json")).unwrap(),
                b"AUTHORED SIBLING"
            );
        }
    }
}

/// Unknown/future versions, relabelled new semantics, forged shared payloads and
/// omitted required hook edits reject before recovery publication. A restored
/// original journal still completes once; serialized input is never authority.
#[test]
fn bootstrap_codex_shared_hooks_recovery_rejects_forged_semantics() {
    let current = crate::integrations::bootstrap::compiled_manifest("codex", None).unwrap();
    for fault in [
        "v2", "v3", "v4", "v9", "payload", "omit", "pointer", "history",
    ] {
        let fixture = Fixture::new();
        let accepted = fixture.plan(&current, Operation::Install).unwrap();
        accepted.fixture_interrupt_after(1);
        assert!(accepted.apply().is_err());
        let original = fs::read(fixture.journal()).unwrap();
        let mut journal: serde_json::Value = serde_json::from_slice(&original).unwrap();
        match fault {
            "v2" => journal["version"] = 2.into(),
            "v3" => journal["version"] = 3.into(),
            "v4" => journal["version"] = 4.into(),
            "v9" => journal["version"] = 9.into(),
            "payload" => journal["changes"][0]["after"] = serde_json::json!(b"{}".to_vec()),
            "omit" => {
                journal["changes"].as_array_mut().unwrap().remove(0);
                fs::remove_file(fixture.root.join("hooks.json")).unwrap();
            }
            "pointer" => {
                journal["intent"]["manifest"]["entries"][0]["artifact"]["JsonArrayEntries"]["entries"]
                    [0]["pointer"] = "/authored/Stop".into()
            }
            "history" => journal["intent"]["manifest"]["revision"] = 99.into(),
            _ => unreachable!(),
        }
        fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
        let before = tree_snapshot(&fixture.workspace);
        assert!(
            recover_private(&fixture.root, &fixture.home, &current).is_err(),
            "{fault}"
        );
        assert_eq!(tree_snapshot(&fixture.workspace), before);
        fs::write(fixture.journal(), original).unwrap();
        assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
    }
}

/// Genuine v4 revision-1 install/uninstall remains whole-file original intent,
/// not reinterpretation into the new shared planner. The fixture restores exact
/// authorized old preimages and original deletion payload after a controlled edge;
/// current recovery must settle those immutable effects and refuse edited copies.
#[test]
fn bootstrap_codex_shared_hooks_preserve_genuine_v4_original_operations() {
    let current = crate::integrations::bootstrap::compiled_manifest("codex", None).unwrap();
    let old = crate::integrations::bootstrap::compiled_history(&current)
        .pop()
        .unwrap();
    let old_bytes = match &old.entries[0].artifact {
        crate::integrations::bootstrap::reconciliation::Artifact::File { bytes } => bytes,
        _ => unreachable!(),
    };
    for operation in [Operation::Install, Operation::Uninstall] {
        let fixture = Fixture::new();
        if matches!(operation, Operation::Uninstall) {
            fixture
                .plan(&old, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
        }
        let accepted = fixture.plan(&old, operation).unwrap();
        accepted.fixture_interrupt_after(1);
        assert!(accepted.apply().is_err());
        let mut journal: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.journal()).unwrap()).unwrap();
        journal["version"] = 4.into();
        if matches!(operation, Operation::Uninstall) {
            journal["changes"][0]["after"] = serde_json::Value::Null;
            fs::write(fixture.root.join("hooks.json"), old_bytes).unwrap();
        }
        fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
        if matches!(operation, Operation::Install) {
            assert_eq!(
                fs::read(fixture.root.join("hooks.json")).unwrap(),
                *old_bytes
            );
        } else {
            assert!(!fixture.root.join("hooks.json").exists());
        }
        assert!(!recover_private(&fixture.root, &fixture.home, &current).unwrap());
    }
}

/// Representative authored registrations contain commands, matchers and timeouts
/// which must remain opaque siblings, not be adopted by a generated-name heuristic.
fn authored() -> serde_json::Value {
    serde_json::json!({"matcher":"authored-matcher","hooks":[{"type":"command","command":"echo user-mez-is-not-owned","timeout":7}],"authored":true})
}

/// First install must merge exact compiled lifecycle members into a real-shaped
/// authored document. Repeat stays byte-exact, receipted exact duplicates collapse,
/// and uninstall removes only owned members while preserving the shared document,
/// unrelated events, top-level policy/metadata, config and every authored field.
#[test]
fn bootstrap_codex_shared_hooks_install_repeat_dedup_and_uninstall() {
    let current = crate::integrations::bootstrap::compiled_manifest("codex", None).unwrap();
    let fixture = Fixture::new();
    let input = serde_json::json!({"description":"authored description","enabled":false,"hooks":{"SessionStart":[authored()],"OtherEvent":[authored()]},"custom":{"keep":"exact"}});
    fs::write(
        fixture.root.join("hooks.json"),
        serde_json::to_vec(&input).unwrap(),
    )
    .unwrap();
    fs::write(
        fixture.root.join("config.toml"),
        b"[features]\nhooks = false\n",
    )
    .unwrap();
    let before = tree_snapshot(&fixture.workspace);
    let accepted = fixture
        .plan(&current, Operation::Install)
        .expect("authored hook arrays are shared");
    assert_eq!(tree_snapshot(&fixture.workspace), before);
    accepted.apply().unwrap();
    let bytes = fs::read(fixture.root.join("hooks.json")).unwrap();
    let mut installed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(installed["description"], input["description"]);
    assert_eq!(installed["enabled"], false);
    assert_eq!(installed["custom"], input["custom"]);
    assert_eq!(
        installed["hooks"]["OtherEvent"],
        input["hooks"]["OtherEvent"]
    );
    assert_eq!(installed["hooks"]["SessionStart"][0], authored());
    assert_eq!(
        installed["hooks"]["SessionStart"].as_array().unwrap().len(),
        2
    );
    fixture
        .plan(&current, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(fs::read(fixture.root.join("hooks.json")).unwrap(), bytes);
    let owned = installed["hooks"]["SessionStart"][1].clone();
    installed["hooks"]["SessionStart"]
        .as_array_mut()
        .unwrap()
        .extend([authored(), owned]);
    fs::write(
        fixture.root.join("hooks.json"),
        serde_json::to_vec(&installed).unwrap(),
    )
    .unwrap();
    fixture
        .plan(&current, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    let repaired: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.root.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(
        repaired["hooks"]["SessionStart"].as_array().unwrap().len(),
        3
    );
    assert_eq!(repaired["hooks"]["SessionStart"][0], authored());
    assert_eq!(repaired["hooks"]["SessionStart"][2], authored());
    fixture
        .plan(&current, Operation::Uninstall)
        .unwrap()
        .apply()
        .unwrap();
    let removed: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.root.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(
        removed["hooks"]["SessionStart"],
        serde_json::json!([authored(), authored()])
    );
    for name in ["UserPromptSubmit", "Stop", "Interrupt", "SessionEnd"] {
        assert_eq!(removed["hooks"][name], serde_json::json!([]));
    }
    assert_eq!(removed["description"], input["description"]);
    assert_eq!(removed["hooks"]["OtherEvent"], input["hooks"]["OtherEvent"]);
    assert_eq!(
        fs::read(fixture.root.join("config.toml")).unwrap(),
        b"[features]\nhooks = false\n"
    );
    assert!(
        fixture
            .plan(&current, Operation::Uninstall)
            .unwrap()
            .changed_paths()
            .is_empty()
    );
}

/// Every effect boundary of receipt-qualified whole-file/shared-member migration must recover
/// original new intent without deleting authored hook additions. Exact duplicates
/// are owned only with a compiled receipt; install replaces the receipt with new
/// array ownership and uninstall retains the shared file and sibling order.
#[test]
fn bootstrap_codex_shared_hooks_migrate_historical_receipt_across_every_edge() {
    let current = crate::integrations::bootstrap::compiled_manifest("codex", None).unwrap();
    let history = crate::integrations::bootstrap::compiled_history(&current);
    assert_eq!(
        history.iter().map(|old| old.revision).collect::<Vec<_>>(),
        vec![6, 5, 4, 3, 2, 1]
    );
    for old in history {
        for operation in [Operation::Install, Operation::Uninstall] {
            for boundary in 1..=2 {
                let fixture = Fixture::new();
                fixture
                    .plan(&old, Operation::Install)
                    .unwrap()
                    .apply()
                    .unwrap();
                let mut document: serde_json::Value =
                    serde_json::from_slice(&fs::read(fixture.root.join("hooks.json")).unwrap())
                        .unwrap();
                document["description"] = "user changed description".into();
                document["custom"] = serde_json::json!({"keep":true});
                for name in [
                    "SessionStart",
                    "UserPromptSubmit",
                    "Stop",
                    "Interrupt",
                    "SessionEnd",
                ] {
                    let owned = document["hooks"][name][0].clone();
                    document["hooks"][name] =
                        serde_json::json!([authored(), owned, authored(), owned]);
                }
                fs::write(
                    fixture.root.join("hooks.json"),
                    serde_json::to_vec(&document).unwrap(),
                )
                .unwrap();
                let before = tree_snapshot(&fixture.workspace);
                let accepted = fixture.plan(&current, operation).unwrap();
                assert_eq!(tree_snapshot(&fixture.workspace), before);
                assert_eq!(accepted.changes.len(), 2);
                assert!(accepted.intent.archives.is_empty());
                accepted.fixture_interrupt_after(boundary);
                assert!(accepted.apply().is_err());
                let journal: serde_json::Value =
                    serde_json::from_slice(&fs::read(fixture.journal()).unwrap()).unwrap();
                assert_eq!(journal["version"], 8);
                assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
                assert!(!recover_private(&fixture.root, &fixture.home, &current).unwrap());
                let after: serde_json::Value =
                    serde_json::from_slice(&fs::read(fixture.root.join("hooks.json")).unwrap())
                        .unwrap();
                for name in [
                    "SessionStart",
                    "UserPromptSubmit",
                    "Stop",
                    "Interrupt",
                    "SessionEnd",
                ] {
                    let expected = if matches!(operation, Operation::Install) {
                        let Artifact::JsonArrayEntries { entries } = &current.entries[0].artifact
                        else {
                            panic!("current Codex ownership must be shared members");
                        };
                        let member = entries
                            .iter()
                            .find(|entry| entry.pointer == format!("/hooks/{name}"))
                            .unwrap();
                        serde_json::json!([authored(), member.value, authored()])
                    } else {
                        serde_json::json!([authored(), authored()])
                    };
                    assert_eq!(after["hooks"][name], expected);
                }
                assert_eq!(after["description"], document["description"]);
                assert_eq!(after["custom"], document["custom"]);
                assert!(
                    fixture
                        .plan(&current, operation)
                        .unwrap()
                        .changed_paths()
                        .is_empty()
                );
            }
        }
    }
}
