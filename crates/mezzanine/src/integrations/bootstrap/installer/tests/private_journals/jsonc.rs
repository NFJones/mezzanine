//! OpenCode JSONC byte-preservation and selected-file ownership fixtures.
//!
//! Temporary roots/Home drive the actual private planner and journal owner. No
//! vendor process, user configuration, dependency installation or provider is used.

use super::*;

/// Both review-derived transitions must recover every artifact/receipt edge.
/// Old exact ownership is mandatory, an unowned exact target match is a conflict,
/// and no partly edited in-memory migration can leak through failed planning.
#[test]
fn bootstrap_jsonc_review_transitions_recover_and_reject_ambiguous_ownership() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    let old = crate::integrations::bootstrap::opencode_artifact::historical_manifest();
    for nested in [false, true] {
        for boundary in 1..=if nested { 2 } else { 5 } {
            let fixture = Fixture::new();
            if nested {
                fs::write(
                    fixture.root.join("tui.json"),
                    b"{\"tui\":{\"plugin\":[\"nested-user\",],},}",
                )
                .unwrap();
                fixture
                    .plan(&current, Operation::Install)
                    .unwrap()
                    .apply()
                    .unwrap();
                fs::write(fixture.root.join("tui.json"), b"{/*note*/\"tui\":{\"plugin\":[\"nested-user\",\"./plugins/mezzanine/opencode_tui.mjs\",],},\"plugin\":[\"root-user\",],}").unwrap();
            } else {
                fixture
                    .plan(&old, Operation::Install)
                    .unwrap()
                    .apply()
                    .unwrap();
                fs::write(
                    fixture.root.join("tui.json"),
                    b"{/*lower*/\"plugin\":[\"user\",\"./plugins/mezzanine/opencode_tui.mjs\",],}",
                )
                .unwrap();
                fs::write(
                    fixture.root.join("tui.jsonc"),
                    b"{/*higher*/\"plugin\":[\"higher-user\",],}",
                )
                .unwrap();
            }
            let accepted = fixture.plan(&current, Operation::Install).unwrap();
            assert_eq!(accepted.changes.len(), if nested { 2 } else { 5 });
            accepted.fixture_interrupt_after(boundary);
            assert!(accepted.apply().is_err());
            assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
            assert!(!recover_private(&fixture.root, &fixture.home, &current).unwrap());
            assert!(
                fixture
                    .plan(&current, Operation::Install)
                    .unwrap()
                    .changed_paths()
                    .is_empty()
            );
            assert!(
                fs::read_to_string(fixture.root.join("tui.json"))
                    .unwrap()
                    .contains(if nested { "/*note*/" } else { "/*lower*/" })
            );
        }
    }
    for fault in ["missing", "edited", "target-match"] {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("tui.json"),
            b"{\"tui\":{\"plugin\":[\"nested-user\",],},}",
        )
        .unwrap();
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let nested = if fault == "missing" {
            "nested-user"
        } else if fault == "edited" {
            "edited-owned"
        } else {
            "./plugins/mezzanine/opencode_tui.mjs"
        };
        let root = if fault == "target-match" {
            "./plugins/mezzanine/opencode_tui.mjs"
        } else {
            "root-user"
        };
        let text = format!(
            "{{/*keep*/\"tui\":{{\"plugin\":[{}],}},\"plugin\":[{}],}}",
            serde_json::to_string(nested).unwrap(),
            serde_json::to_string(root).unwrap()
        );
        fs::write(fixture.root.join("tui.json"), text).unwrap();
        let before = tree_snapshot(&fixture.workspace);
        assert!(
            fixture.plan(&current, Operation::Install).is_err(),
            "{fault}"
        );
        assert_eq!(tree_snapshot(&fixture.workspace), before);
    }
}

/// Newly requested v7 migration/removal of a genuine v6 receipt must use token
/// edits even when no new artifact remains at the old filename. Comments/BOM and
/// trailing commas remain authored; this is not reinterpretation of an old journal.
#[test]
fn bootstrap_jsonc_direct_strict_receipt_filename_move_and_uninstall_preserve_bytes() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    let old = crate::integrations::bootstrap::opencode_artifact::historical_manifest();
    for operation in [Operation::Install, Operation::Uninstall] {
        let fixture = Fixture::new();
        fixture
            .plan(&old, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let text = "\u{feff}{/*lower*/\"plugin\":[\"user\",\"./plugins/mezzanine/opencode_tui.mjs\",],\"theme\":\"雪\",}\n";
        fs::write(fixture.root.join("tui.json"), text).unwrap();
        let higher = b"{/*higher*/\"plugin\":[\"higher-user\",],}";
        fs::write(fixture.root.join("tui.jsonc"), higher).unwrap();
        fixture
            .plan(&current, operation)
            .expect("new v7 removal must preserve old authored JSONC")
            .apply()
            .unwrap();
        let lower = fs::read_to_string(fixture.root.join("tui.json")).unwrap();
        assert!(lower.starts_with('\u{feff}'));
        assert!(lower.contains("/*lower*/"));
        assert!(lower.contains("\"user\""));
        assert!(lower.ends_with("\"theme\":\"雪\",}\n"));
        assert!(!lower.contains("./plugins/mezzanine/opencode_tui.mjs"));
        if matches!(operation, Operation::Uninstall) {
            assert_eq!(fs::read(fixture.root.join("tui.jsonc")).unwrap(), higher);
        } else {
            assert!(
                fs::read_to_string(fixture.root.join("tui.jsonc"))
                    .unwrap()
                    .contains("./plugins/mezzanine/opencode_tui.mjs")
            );
        }
    }
}

/// A newly authored root list overrides a previously effective nested list. With
/// old exact membership intact, repeat moves only its registration to the root,
/// preserves both user lists/trivia and then remains a byte-exact current noop.
#[test]
fn bootstrap_jsonc_effective_pointer_transition_preserves_intact_owned_registration() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    for name in ["tui.json", "tui.jsonc"] {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join(name),
            b"{\"tui\":{\"plugin\":[\"nested-user\",],},}",
        )
        .unwrap();
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        fs::write(fixture.root.join(name), b"{/*root-note*/\"tui\":{\"plugin\":[\"nested-user\",\"./plugins/mezzanine/opencode_tui.mjs\",],},\"plugin\":[\"root-user\",],}").unwrap();
        fixture
            .plan(&current, Operation::Install)
            .expect("intact old member can migrate to effective pointer")
            .apply()
            .unwrap();
        let text = fs::read_to_string(fixture.root.join(name)).unwrap();
        assert!(text.contains("/*root-note*/"));
        assert!(text.contains("nested-user"));
        assert!(text.contains("root-user"));
        assert_eq!(
            text.matches("./plugins/mezzanine/opencode_tui.mjs").count(),
            1
        );
        assert!(
            text.find("./plugins/mezzanine/opencode_tui.mjs").unwrap()
                > text.find("root-user").unwrap()
        );
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        assert_eq!(fs::read_to_string(fixture.root.join(name)).unwrap(), text);
    }
}

/// Genuine version-6 strict OpenCode intent remains recoverable without invoking
/// the new JSONC grammar. New kind/filename/pointer identities relabelled as v6
/// cannot gain authority; malformed authored data never causes publication.
#[test]
fn bootstrap_jsonc_genuine_v6_and_invalid_authored_state_remain_strict() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    let old = crate::integrations::bootstrap::opencode_artifact::historical_manifest();
    let fixture = Fixture::new();
    let accepted = fixture.plan(&old, Operation::Install).unwrap();
    accepted.fixture_interrupt_after(1);
    assert!(accepted.apply().is_err());
    let mut journal: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.journal()).unwrap()).unwrap();
    journal["version"] = 6.into();
    fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
    assert!(recover_private(&fixture.root, &fixture.home, &current).unwrap());
    let receipt: Receipt =
        serde_json::from_slice(&fs::read(fixture.receipt("opencode")).unwrap()).unwrap();
    assert_eq!(receipt.manifest, old);
    assert!(!recover_private(&fixture.root, &fixture.home, &current).unwrap());
    for text in [
        b"{/*bad".as_slice(),
        b"{\"plugin\":[],\"plugin\":[]}",
        b"{\"plugin\":false}",
        b"{'plugin':[]}",
        b"{\"tui\":{\"plugin\":false}}",
    ] {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("tui.jsonc"), text).unwrap();
        let before = tree_snapshot(&fixture.workspace);
        assert!(fixture.plan(&current, Operation::Install).is_err());
        assert_eq!(tree_snapshot(&fixture.workspace), before);
    }
}

/// Historical layout enumeration must not admit receipt-selected filenames,
/// arbitrary pointers/values, or unknown helper source. Even a syntactically
/// valid receipt containing such a manifest rejects both install and uninstall
/// before changing the vendor or private state trees.
#[test]
fn bootstrap_jsonc_historical_layout_forgery_is_non_destructive() {
    use crate::integrations::bootstrap::{opencode_artifact, reconciliation::Artifact};
    let current = opencode_artifact::manifest();
    let old = crate::integrations::bootstrap::compiled_history(&current)[0].clone();
    for forgery in ["path", "pointer", "value", "helper"] {
        let fixture = Fixture::new();
        fixture
            .plan(&old, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let mut forged = old.clone();
        let registration = forged
            .entries
            .iter_mut()
            .find(|entry| matches!(entry.artifact, Artifact::JsoncArrayEntry { .. }))
            .unwrap();
        match forgery {
            "path" => registration.path = "custom.jsonc".into(),
            "pointer" => {
                let Artifact::JsoncArrayEntry { pointer, .. } = &mut registration.artifact else {
                    unreachable!()
                };
                *pointer = "/other/plugin".into();
            }
            "value" => {
                let Artifact::JsoncArrayEntry { value, .. } = &mut registration.artifact else {
                    unreachable!()
                };
                *value = serde_json::json!("./foreign-plugin.mjs");
            }
            "helper" => {
                let helper = forged
                    .entries
                    .iter_mut()
                    .find(|entry| entry.path.ends_with("/peer_helper.mjs"))
                    .unwrap();
                helper.artifact = Artifact::File {
                    bytes: b"export const peerHelper = '/unknown/mez';\n".to_vec(),
                };
            }
            _ => unreachable!(),
        }
        fs::write(fixture.receipt("opencode"), receipt_bytes(&forged).unwrap()).unwrap();
        let before = tree_snapshot(&fixture.workspace);
        for operation in [Operation::Install, Operation::Uninstall] {
            let error = fixture.plan(&current, operation).err().unwrap();
            assert!(
                error
                    .to_string()
                    .contains("not a recognized compiled manifest revision")
            );
            assert_eq!(tree_snapshot(&fixture.workspace), before);
        }
    }
}

/// New format kinds must retain the actual strict v6 predecessor and its shipped
/// bytes. Drift in a newer private payload cannot redefine old authority, and
/// current filename/pointer alternatives remain compiled variants, not fake history.
#[test]
fn bootstrap_jsonc_history_keeps_strict_predecessors_and_rejects_payload_drift() {
    use crate::integrations::bootstrap::{opencode_artifact, reconciliation::Artifact};
    let current = opencode_artifact::manifest();
    let history = crate::integrations::bootstrap::compiled_history(&current);
    assert_eq!(history.len(), 22);
    assert!(history.contains(&opencode_artifact::historical_manifest()));
    let jsonc = history
        .iter()
        .filter(|manifest| {
            manifest
                .entries
                .iter()
                .any(|entry| matches!(entry.artifact, Artifact::JsoncArrayEntry { .. }))
        })
        .collect::<Vec<_>>();
    assert_eq!(jsonc.len(), 16);
    assert_eq!(history.len() - jsonc.len(), 6);
    for revision in [7, 8, 9, 10] {
        assert_eq!(
            jsonc
                .iter()
                .filter(|manifest| manifest.revision == revision)
                .count(),
            4
        );
    }
    assert!(
        jsonc
            .iter()
            .all(|manifest| [7, 8, 9, 10].contains(&manifest.revision))
    );
    let tui = include_bytes!("../../../opencode_tui.mjs");
    let client = include_bytes!("../../../history/persistent_client_v5.mjs");
    assert!(opencode_artifact::historical_payloads_match(tui, client));
    let mut edited = tui.to_vec();
    edited.push(0);
    assert!(!opencode_artifact::historical_payloads_match(
        &edited, client
    ));
    let mut edited = client.to_vec();
    edited.push(0);
    assert!(!opencode_artifact::historical_payloads_match(tui, &edited));
}

/// The vendor flattens an authored nested tui block only when a root plugin field
/// is absent. Bootstrap must preserve that effective plugin list rather than
/// shadow it with a newly created root array; root plugin precedence stays exact.
#[test]
fn bootstrap_jsonc_nested_tui_plugin_normalization_preserves_effective_authors() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    for name in ["tui.json", "tui.jsonc"] {
        for root_plugin in [false, true] {
            let fixture = Fixture::new();
            let text = if root_plugin {
                "{/*outer*/\"tui\":{\"plugin\":[\"nested-user\",],\"theme\":\"nested\",},\"plugin\":[\"root-user\",],}"
            } else {
                "\u{feff}{/*outer*/\"tui\":{\"plugin\":[\"nested-user\",],\"theme\":\"nested\",},}"
            };
            fs::write(fixture.root.join(name), text).unwrap();
            fixture
                .plan(&current, Operation::Install)
                .unwrap()
                .apply()
                .unwrap();
            let installed = fs::read_to_string(fixture.root.join(name)).unwrap();
            let receipt: Receipt =
                serde_json::from_slice(&fs::read(fixture.receipt("opencode")).unwrap()).unwrap();
            let entry = receipt
                .manifest
                .entries
                .iter()
                .find(|entry| entry.path == name)
                .unwrap();
            assert!(
                matches!(&entry.artifact, crate::integrations::bootstrap::reconciliation::Artifact::JsoncArrayEntry { pointer, .. } if pointer == if root_plugin { "/plugin" } else { "/tui/plugin" })
            );
            assert!(installed.contains("nested-user"));
            assert!(installed.contains("/*outer*/"));
            if root_plugin {
                assert!(
                    installed
                        .contains("\"tui\":{\"plugin\":[\"nested-user\",],\"theme\":\"nested\",}")
                );
            } else {
                assert!(installed.starts_with('\u{feff}'));
                assert_eq!(installed.matches("\"plugin\"").count(), 1);
            }
            fixture
                .plan(&current, Operation::Uninstall)
                .unwrap()
                .apply()
                .unwrap();
            assert!(
                !fs::read_to_string(fixture.root.join(name))
                    .unwrap()
                    .contains("./plugins/mezzanine/opencode_tui.mjs")
            );
        }
    }
}

/// Every current JSONC install/uninstall effect boundary in both root-local
/// filenames can settle original intent then honor the requested operation.
/// Previews are write-free, comments survive, v6 labels cannot grant v7 semantics,
/// and current private missing-file repair remains available under exact receipts.
#[test]
fn bootstrap_jsonc_private_all_operations_and_publication_edges_recover() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    for name in ["tui.json", "tui.jsonc"] {
        for original in [Operation::Install, Operation::Uninstall] {
            for requested in [Operation::Install, Operation::Uninstall] {
                let probe = Fixture::new();
                fs::write(
                    probe.root.join(name),
                    b"{/*authored*/\"plugin\":[\"user\",],\"theme\":true,}",
                )
                .unwrap();
                if matches!(original, Operation::Uninstall) {
                    probe
                        .plan(&current, Operation::Install)
                        .unwrap()
                        .apply()
                        .unwrap();
                }
                let effects = probe.plan(&current, original).unwrap().changes.len();
                for boundary in 1..=effects {
                    let fixture = Fixture::new();
                    fs::write(
                        fixture.root.join(name),
                        b"{/*authored*/\"plugin\":[\"user\",],\"theme\":true,}",
                    )
                    .unwrap();
                    if matches!(original, Operation::Uninstall) {
                        fixture
                            .plan(&current, Operation::Install)
                            .unwrap()
                            .apply()
                            .unwrap();
                    }
                    let accepted = fixture.plan(&current, original).unwrap();
                    accepted.fixture_interrupt_after(boundary);
                    assert!(accepted.apply().is_err());
                    let original_bytes = fs::read(fixture.journal()).unwrap();
                    let mut journal: serde_json::Value =
                        serde_json::from_slice(&original_bytes).unwrap();
                    assert_eq!(journal["version"], 8);
                    journal["version"] = 6.into();
                    fs::write(fixture.journal(), serde_json::to_vec(&journal).unwrap()).unwrap();
                    let before = tree_snapshot(&fixture.workspace);
                    assert!(recover_private(&fixture.root, &fixture.home, &current).is_err());
                    assert_eq!(tree_snapshot(&fixture.workspace), before);
                    fs::write(fixture.journal(), original_bytes).unwrap();
                    let before = tree_snapshot(&fixture.workspace);
                    let retry = fixture.plan(&current, requested).unwrap();
                    assert!(retry.recovery_pending());
                    assert_eq!(tree_snapshot(&fixture.workspace), before);
                    retry.apply().unwrap();
                    let text = fs::read_to_string(fixture.root.join(name)).unwrap();
                    assert!(text.contains("/*authored*/"));
                    assert!(text.contains("\"user\""));
                    assert!(text.ends_with("\"theme\":true,}"));
                    assert_eq!(
                        text.contains("./plugins/mezzanine/opencode_tui.mjs"),
                        matches!(requested, Operation::Install)
                    );
                    assert!(
                        fixture
                            .plan(&current, requested)
                            .unwrap()
                            .changed_paths()
                            .is_empty()
                    );
                    if matches!(requested, Operation::Install) {
                        let helper = fixture.root.join("plugins/mezzanine/opencode_tui.mjs");
                        fs::remove_file(&helper).unwrap();
                        fixture
                            .plan(&current, Operation::Install)
                            .unwrap()
                            .apply()
                            .unwrap();
                        assert!(helper.is_file());
                    }
                }
            }
        }
    }
}

/// A genuine strict revision-6 receipt can upgrade against newly authored JSONC
/// trivia without changing the member or any document bytes. Moving to the later
/// root-local JSONC file removes only the old exact member; both authored files
/// remain, and the new receipt pins the actual enumerated destination.
#[test]
fn bootstrap_jsonc_historical_receipt_upgrade_and_filename_transition_preserve_authors() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    let old = crate::integrations::bootstrap::opencode_artifact::historical_manifest();
    let fixture = Fixture::new();
    fixture
        .plan(&old, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    let text = b"{// usercomment\n\"plugin\":[\"user\",\"./plugins/mezzanine/opencode_tui.mjs\",],\"theme\":true,}";
    fs::write(fixture.root.join("tui.json"), text).unwrap();
    fixture
        .plan(&current, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(fs::read(fixture.root.join("tui.json")).unwrap(), text);
    fs::write(
        fixture.root.join("tui.jsonc"),
        b"{/*higher*/\"plugin\":[\"higher-user\",],\"theme\":false,}",
    )
    .unwrap();
    fixture
        .plan(&current, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    let lower = fs::read_to_string(fixture.root.join("tui.json")).unwrap();
    assert!(lower.contains("// usercomment"));
    assert!(lower.contains("\"user\""));
    assert!(!lower.contains("./plugins/mezzanine/opencode_tui.mjs"));
    let higher = fs::read_to_string(fixture.root.join("tui.jsonc")).unwrap();
    assert!(higher.contains("/*higher*/"));
    assert!(higher.contains("higher-user"));
    assert!(higher.contains("./plugins/mezzanine/opencode_tui.mjs"));
    let receipt: Receipt =
        serde_json::from_slice(&fs::read(fixture.receipt("opencode")).unwrap()).unwrap();
    assert_eq!(
        receipt.manifest,
        crate::integrations::bootstrap::opencode_artifact::jsonc_manifest()
    );
    fixture
        .plan(&current, Operation::Uninstall)
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join("tui.json")).unwrap(),
        lower
    );
    assert!(
        !fs::read_to_string(fixture.root.join("tui.jsonc"))
            .unwrap()
            .contains("./plugins/mezzanine/opencode_tui.mjs")
    );
}

/// JSONC comments/trailing commas in either loader filename are valid authored
/// configuration, not a conflict or permission to reserialize the document. The
/// compiled exact registration is added once, byte-exact repeat remains readonly,
/// and uninstall preserves comments, sibling values, theme and the shared file.
#[test]
fn bootstrap_jsonc_opencode_preserves_comments_and_selected_file() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    for name in ["tui.json", "tui.jsonc"] {
        let fixture = Fixture::new();
        let text = "// header\r\n{\r\n  \"plugin\": [\"user-a\", /* inside */],\r\n  \"theme\": \"雪\", // theme note\r\n}\r\n";
        fs::write(fixture.root.join(name), text).unwrap();
        let before = tree_snapshot(&fixture.workspace);
        let accepted = fixture
            .plan(&current, Operation::Install)
            .expect("loader-supported JSONC must remain authored bytes");
        assert_eq!(tree_snapshot(&fixture.workspace), before);
        accepted.apply().unwrap();
        let installed = fs::read(fixture.root.join(name)).unwrap();
        let installed_text = String::from_utf8(installed.clone()).unwrap();
        assert!(installed_text.starts_with("// header\r\n{\r\n"));
        assert!(installed_text.contains("\"user-a\", /* inside */"));
        assert!(installed_text.ends_with("\"theme\": \"雪\", // theme note\r\n}\r\n"));
        assert_eq!(
            installed_text
                .matches("./plugins/mezzanine/opencode_tui.mjs")
                .count(),
            1
        );
        fixture
            .plan(&current, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        assert_eq!(fs::read(fixture.root.join(name)).unwrap(), installed);
        if name == "tui.jsonc" {
            assert!(!fixture.root.join("tui.json").exists());
        }
        fixture
            .plan(&current, Operation::Uninstall)
            .unwrap()
            .apply()
            .unwrap();
        let removed = fs::read_to_string(fixture.root.join(name)).unwrap();
        assert!(removed.contains("// header\r\n"));
        assert!(removed.contains("/* inside */"));
        assert!(removed.contains("\"user-a\""));
        assert!(removed.ends_with("\"theme\": \"雪\", // theme note\r\n}\r\n"));
        assert!(!removed.contains("./plugins/mezzanine/opencode_tui.mjs"));
    }
}

/// Root-local loader order is JSON then JSONC. Selection must write only the
/// higher-priority existing JSONC file, preserve the entire lower authored file,
/// and fence a change to either selection observation before publication.
#[test]
fn bootstrap_jsonc_opencode_selection_preserves_lower_file_and_fences_drift() {
    let current = crate::integrations::bootstrap::compiled_manifest("opencode", None).unwrap();
    let fixture = Fixture::new();
    let lower = b"{\"plugin\":[\"lower-user\"],\"theme\":\"lower\"}";
    fs::write(fixture.root.join("tui.json"), lower).unwrap();
    fs::write(
        fixture.root.join("tui.jsonc"),
        b"{/*higher*/\"plugin\":[\"higher-user\",],}",
    )
    .unwrap();
    fixture
        .plan(&current, Operation::Install)
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(fs::read(fixture.root.join("tui.json")).unwrap(), lower);
    assert!(
        fs::read_to_string(fixture.root.join("tui.jsonc"))
            .unwrap()
            .contains("higher-user")
    );
    let accepted = fixture.plan(&current, Operation::Uninstall).unwrap();
    fs::write(fixture.root.join("tui.json"), b"authored late lower edit").unwrap();
    let before = tree_snapshot(&fixture.workspace);
    assert!(accepted.apply().is_err());
    assert_eq!(tree_snapshot(&fixture.workspace), before);
}
