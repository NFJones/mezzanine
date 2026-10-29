//! Native instruction snapshots remain bound to accepted workspace generations.
//!
//! These fixtures supply prepared in-memory discovery through the internal
//! recorder. They qualify cache/context consumers and real loop ingress, not
//! filesystem discovery, registered navigation, or native child process fate.

use super::*;
use mez_agent::instructions::DiscoveredInstructionFile;

/// Late root bootstrap writes and clears cannot replace another workspace's
/// accepted discovery. Empty and unknown snapshots stay distinct, stale
/// generations cannot adopt data, and pane mode still reads actual root data.
#[test]
fn native_instruction_snapshot_is_owned_by_directory_generation() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_native_shell_mode_for_tests("%1");
    let target = vec![instruction_file("/target", "native-rule")];
    service.set_pane_agent_instruction_files("%1", vec![instruction_file("/pane", "root-old")]);
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .record_native_working_directory_with_instructions("/target".into(), 0, target.clone())
        .unwrap();
    assert_eq!(
        service.pane_agent_instruction_files("%1"),
        Some(target.as_slice())
    );
    service.set_pane_agent_instruction_files("%1", vec![instruction_file("/pane", "root-late")]);
    service.clear_pane_agent_instruction_files("%1");
    assert_eq!(
        service.pane_agent_instruction_files("%1"),
        Some(target.as_slice())
    );
    let retained = service.agent_shell_store().get("%1").unwrap().clone();
    assert!(
        service
            .agent_shell_store_mut()
            .ensure_session("%1")
            .unwrap()
            .record_native_working_directory_with_instructions("/stale".into(), 0, Vec::new())
            .is_err()
    );
    assert_eq!(service.agent_shell_store().get("%1"), Some(&retained));
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .record_native_working_directory_with_instructions("/empty".into(), 1, Vec::new())
        .unwrap();
    let root = vec![instruction_file("/pane", "root-new")];
    service.set_pane_agent_instruction_files("%1", root.clone());
    assert_eq!(
        service.pane_agent_instruction_files("%1"),
        Some([].as_slice())
    );
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    assert_eq!(
        service.pane_agent_instruction_files("%1"),
        Some(root.as_slice())
    );
    service.set_agent_native_shell_mode_for_tests("%1");
    assert_eq!(
        service.pane_agent_instruction_files("%1"),
        Some([].as_slice())
    );
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .record_native_working_directory("/unknown".into(), 2)
        .unwrap();
    assert_eq!(service.pane_agent_instruction_files("%1"), None);
    service
        .agent_shell_store_mut()
        .restore_session("%1", retained)
        .unwrap();
    assert_eq!(
        service.pane_agent_instruction_files("%1"),
        Some(target.as_slice())
    );
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage_and_transcript_source(
            "%1",
            "unrelated",
            0,
            None,
            None,
            0,
        )
        .unwrap();
    assert_eq!(
        service.pane_agent_instruction_files("%1"),
        Some(root.as_slice())
    );
}

/// Real loop binding carries the parent's entire directory/discovery snapshot
/// through fork/new isolation, not a mutable pane cache. Empty discovery is an
/// explicit model-visible successor in every mode, including fresh history.
#[test]
fn native_instruction_snapshot_follows_actual_loop_work_binding() {
    for mode in ["--fork", "--new", ""] {
        for empty in [false, true] {
            let root = temp_root("native-instruction-loop");
            fs::create_dir_all(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let mut service = test_runtime_service();
            service.set_agent_native_shell_mode_for_tests("%1");
            service.set_pane_current_working_directory("%1", root.clone());
            service.set_pane_screen(
                "%1".to_string(),
                TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap(),
            );
            service
                .agent_shell_store_mut()
                .enter_or_resume("%1")
                .unwrap();
            let files = if empty {
                Vec::new()
            } else {
                [".", "./src", "./src/nested"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, scope)| instruction_file(scope, &format!("native-rule-{index}")))
                    .collect()
            };
            service
                .agent_shell_store_mut()
                .ensure_session("%1")
                .unwrap()
                .record_native_working_directory_with_instructions(root.clone(), 0, files.clone())
                .unwrap();
            service.set_pane_agent_instruction_files(
                "%1",
                vec![instruction_file("/pane", "root-stale")],
            );
            service
                .execute_agent_shell_loop_command(
                    "%1",
                    &format!("/loop {mode} --limit 1 inspect guidance"),
                )
                .unwrap();
            assert_eq!(
                service.pane_agent_instruction_files("%1"),
                Some(files.as_slice())
            );
            let turn = service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .clone()
                .unwrap();
            let context = service.agent_turn_contexts().get(&turn).unwrap();
            let guidance = context
                .blocks()
                .iter()
                .find(|block| block.source == ContextSourceKind::ProjectGuidance)
                .unwrap();
            assert!(!guidance.content.contains("root-stale"));
            if empty {
                assert_eq!(guidance.content, mez_agent::EMPTY_PROJECT_GUIDANCE_CONTEXT);
            } else {
                for index in 0..3 {
                    assert!(guidance.content.contains(&format!("native-rule-{index}")));
                }
            }
            service.clear_pane_agent_instruction_files("%1");
            assert_eq!(
                service.pane_agent_instruction_files("%1"),
                Some(files.as_slice())
            );
            service
                .agent_shell_store_mut()
                .finish_turn("%1", &turn)
                .unwrap();
            let state = service.agent_loop_state("%1").unwrap().clone();
            service
                .restore_agent_loop_parent_conversation("%1", &state, None)
                .unwrap();
            assert_eq!(
                service.pane_agent_instruction_files("%1"),
                Some(files.as_slice())
            );
            let _ = fs::remove_dir_all(root);
        }
    }
}

/// An instruction-only disk reload must update the live total byte projection.
/// Metadata for later scopes survives budget exhaustion; a larger reloaded
/// budget admits their contents without replacing the discovery snapshot.
#[test]
fn runtime_instruction_context_budget_updates_on_instruction_only_reload() {
    let root = temp_root("instruction-budget-config");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    let initial = "[instructions]\nmax_bytes = 2\n";
    fs::write(&path, initial).unwrap();
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "instruction-budget".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: initial.to_string(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_agent_instruction_files(
        "%1",
        vec![
            instruction_file(".", "éx"),
            instruction_file("./src", "abc"),
        ],
    );
    let context = service
        .agent_context_for_pane_prompt("%1", "inspect limited", 0)
        .unwrap();
    let guidance = context
        .blocks()
        .iter()
        .find(|block| block.source == ContextSourceKind::ProjectGuidance)
        .unwrap();
    assert!(guidance.content.contains("\né\n</repository_instructions>"));
    assert!(!guidance.content.contains("abc"));
    assert!(
        guidance
            .content
            .contains(r#"scope="./src" bytes="3" truncated="true""#)
    );
    fs::write(&path, "[instructions]\nmax_bytes = 6\n").unwrap();
    service.reload_config_layers_from_disk().unwrap();
    assert_eq!(service.agent_instruction_context_max_bytes(), 6);
    let context = service
        .agent_context_for_pane_prompt("%1", "inspect expanded", 0)
        .unwrap();
    let guidance = context
        .blocks()
        .iter()
        .find(|block| block.source == ContextSourceKind::ProjectGuidance)
        .unwrap();
    assert!(guidance.content.contains("éx"));
    assert!(guidance.content.contains("abc"));
    assert!(!guidance.content.contains("truncated or omitted"));
    let _ = fs::remove_dir_all(root);
}

/// Empty discovery before the first provider response appends a current clear
/// fact rather than leaving the old instructions active or rewriting history.
/// This tests the root publication cache as well as the native empty snapshots.
#[test]
fn runtime_instruction_empty_refresh_preserves_prior_context_history() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_agent_instruction_files("%1", vec![instruction_file(".", "old-root-rule")]);
    let started = service
        .start_agent_prompt_turn("%1", "continue with current guidance")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    let original = service
        .agent_turn_contexts()
        .get(&turn.turn_id)
        .unwrap()
        .clone();
    let prior = original
        .chronology()
        .iter()
        .find(|event| event.block().source == ContextSourceKind::ProjectGuidance)
        .unwrap()
        .clone();
    service.set_pane_agent_instruction_files("%1", Vec::new());
    assert_eq!(
        service.pane_agent_instruction_files("%1"),
        Some([].as_slice())
    );
    service
        .refresh_agent_turn_project_guidance_context(&turn)
        .unwrap();
    let changed = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert!(changed.chronology().contains(&prior));
    let guidance = changed
        .blocks()
        .iter()
        .filter(|block| block.source == ContextSourceKind::ProjectGuidance)
        .collect::<Vec<_>>();
    assert_eq!(guidance.len(), 2);
    assert!(guidance[0].content.contains("old-root-rule"));
    assert_eq!(
        guidance[1].content,
        mez_agent::EMPTY_PROJECT_GUIDANCE_CONTEXT
    );
    assert_ne!(
        changed.metadata().prompt_cache_lineage_id,
        original.metadata().prompt_cache_lineage_id
    );
    let refreshed = changed.clone();
    service
        .refresh_agent_turn_project_guidance_context(&turn)
        .unwrap();
    assert_eq!(
        service.agent_turn_contexts().get(&turn.turn_id),
        Some(&refreshed)
    );
}

/// Builds discovery data shared by cache, loop and budget consumers above.
fn instruction_file(scope: &str, content: &str) -> DiscoveredInstructionFile {
    DiscoveredInstructionFile {
        path: format!("{scope}/AGENTS.md"),
        scope_root: scope.to_string(),
        bytes: content.len(),
        truncated: false,
        content: content.to_string(),
    }
}
