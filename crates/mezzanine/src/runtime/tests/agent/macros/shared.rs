//! Shared macro live-trust, catalog parity, and existing step lifecycle coverage.
//!
//! Fixtures use ordinary project trust, pane cwd, slash commands and macro
//! orchestration. Shared placement supplies no new action or child authority.

use super::*;
use crate::security::project::{ProjectTrustStore, TrustDecision};

/// Writes one existing-format macro with exactly one nonexecuted prompt step.
fn write_shared_macro(project: &std::path::Path, prompt: &str) {
    let directory = project.join(".agents/macros/review");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("MACRO.md"),
        format!("---\nname: review\ndescription: Shared review\n---\n## Steps\n1. {prompt}\n"),
    )
    .unwrap();
}

/// Installs a stored pending record in this fixture's private SQLite database.
/// Pending is not an allowed user decision, so tests must not loosen decide().
fn pending_trust_fixture(
    mut trust: ProjectTrustStore,
    root: &std::path::Path,
    base: &std::path::Path,
) -> ProjectTrustStore {
    trust
        .decide(root.to_path_buf(), TrustDecision::Trusted, None)
        .unwrap();
    let database = base.join("pending-source.db");
    trust.save_to_file(&database).unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE project_trust SET state = 'pending' WHERE project_root = ?1",
                [root.to_str().unwrap()],
            )
            .unwrap(),
        1
    );
    drop(connection);
    let trust = ProjectTrustStore::load_from_file(&database).unwrap();
    assert_eq!(trust.get(root).unwrap().state, TrustDecision::Pending);
    trust
}

/// Current deepest trust and cwd must fence shared discovery and invocation.
/// Withheld project content cannot start provider work, and trust is rechecked
/// after an earlier valid catalog rather than inherited from that metadata.
#[test]
fn runtime_macro_shared_catalog_and_invocation_follow_live_trust_and_cwd() {
    for state in [
        "untrusted",
        "pending",
        "rejected",
        "revoked",
        "deeper",
        "deeper-pending",
        "deeper-revoked",
        "cwd",
    ] {
        let base = temp_root(&format!("macro-shared-trust-{state}"));
        let project = base.join("project");
        write_shared_macro(&project, "PROJECT_ONLY");
        let nested = project.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let project = fs::canonicalize(project).unwrap();
        let nested = fs::canonicalize(nested).unwrap();
        let mut service = test_runtime_service();
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service.set_config_root(base.join("user"));
        service.set_pane_current_working_directory("%1", project.clone());
        let mut trust = ProjectTrustStore::default();
        trust
            .decide(project.clone(), TrustDecision::Trusted, None)
            .unwrap();
        service.set_project_trust_store(trust.clone(), None);
        assert!(
            service
                .effective_macro_catalog_for_pane("%1")
                .get("review")
                .is_some()
        );
        match state {
            "untrusted" => service.set_project_trust_store(ProjectTrustStore::default(), None),
            "pending" => {
                service.set_project_trust_store(pending_trust_fixture(trust, &project, &base), None)
            }
            "rejected" | "revoked" => {
                trust
                    .decide(
                        project.clone(),
                        match state {
                            "rejected" => TrustDecision::Rejected,
                            _ => TrustDecision::Revoked,
                        },
                        None,
                    )
                    .unwrap();
                service.set_project_trust_store(trust, None);
            }
            "deeper-pending" => {
                service.set_project_trust_store(pending_trust_fixture(trust, &nested, &base), None);
                service.set_pane_current_working_directory("%1", nested);
            }
            "deeper" | "deeper-revoked" => {
                trust
                    .decide(
                        nested.clone(),
                        match state {
                            "deeper-revoked" => TrustDecision::Revoked,
                            _ => TrustDecision::Rejected,
                        },
                        None,
                    )
                    .unwrap();
                service.set_project_trust_store(trust, None);
                service.set_pane_current_working_directory("%1", nested);
            }
            "cwd" => service.set_pane_current_working_directory("%1", base.clone()),
            _ => unreachable!(),
        }
        assert!(
            service
                .effective_macro_catalog_for_pane("%1")
                .get("review")
                .is_none(),
            "{state}"
        );
        assert!(
            service
                .start_agent_macro_prompt_turn("%1", "#review")
                .is_err(),
            "{state}"
        );
        assert!(service.agent_turn_ledger().turns().is_empty());
        assert!(service.macro_managed_subagent_ids().is_empty());
        service.terminate_all_pane_processes().unwrap();
        fs::remove_dir_all(base).unwrap();
    }
}

/// Listing a shared macro must expose the same winner through inline and
/// deferred paths. Invocation then reloads the current document, submits its
/// exact prompt to one ordinary macro-managed child, and finishes through the
/// existing child-result/judge lifecycle without another authority mechanism.
#[test]
fn runtime_macro_shared_listing_and_current_step_complete_existing_lifecycle() {
    let base = temp_root("macro-shared-lifecycle");
    let project = base.join("project");
    write_shared_macro(&project, "OLD_PROMPT");
    let project = fs::canonicalize(project).unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(base.join("user"));
    service.set_pane_current_working_directory("%1", project.clone());
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(project.clone(), TrustDecision::Trusted, None)
        .unwrap();
    service.set_project_trust_store(trust, None);
    let body = crate::runtime::commands::lists::runtime_agent_macro_catalog_body(
        &service.effective_macro_catalog_for_pane("%1"),
    );
    assert!(body.contains("| `#review` | project | 1 | Shared review |"));
    let ack = service
        .execute_agent_shell_command(&primary, "/list-macros")
        .unwrap();
    assert!(ack.contains(r#""body":null"#));
    let deferred = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    let deferred: serde_json::Value = serde_json::from_str(&deferred).unwrap();
    assert_eq!(deferred["body"], body);
    write_shared_macro(&project, "CURRENT_EXACT_PROMPT");
    let response = service
        .execute_agent_shell_command(&primary, "#review current context")
        .unwrap();
    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    assert_eq!(service.macro_managed_subagent_ids().len(), 1);
    let parent = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == "agent-%1")
        .unwrap()
        .clone();
    let child = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.cooperation_mode.as_deref() == Some("macro-step"))
        .unwrap()
        .clone();
    let run = service.macro_run_for_tests(&parent.turn_id).unwrap();
    assert_eq!(run.steps[0].scripted_prompt, "CURRENT_EXACT_PROMPT");
    assert_eq!(run.child_agent_id, child.agent_id);
    assert_eq!(child.policy_profile, parent.policy_profile);
    let child_context = service.agent_turn_contexts().get(&child.turn_id).unwrap();
    let text = child_context
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("CURRENT_EXACT_PROMPT"));
    assert!(text.contains("current context"));
    assert!(!text.contains("OLD_PROMPT"));
    let child_pid = service
        .primary_pid_for_live_pane_process(&child.pane_id)
        .unwrap();
    let child_token = mez_mux::process::process_start_token_for_pid(child_pid).unwrap();
    service
        .complete_running_agent_turn_and_start_ready(
            &child,
            AgentTurnState::Completed,
            "generated shared macro child completed",
        )
        .unwrap();
    let judge = RuntimeBatchProvider { response: mez_agent::ModelResponse {
        provider: "runtime-batch".into(), model: "test".into(),
        raw_text: r#"{"outcome":"finish_success","step_success":true,"rationale":"shared scripted step completed","adapted_prompt":null,"user_message":null}"#.into(),
        usage: Default::default(), latest_request_usage: None, quota_usage: Default::default(), action_batch: None, provider_transcript_events: vec![],
    } };
    service
        .execute_agent_turn_with_provider(
            &parent.turn_id,
            &judge,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert!(service.macro_managed_subagent_ids().is_empty());
    assert_ne!(
        mez_mux::process::process_start_token_for_pid(child_pid),
        Some(child_token)
    );
    assert!(!service.has_macro_run(&parent.turn_id));
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(base).unwrap();
}

/// Cached human metadata cannot choose a stale shared winner after cwd changes
/// to another trusted project or a native duplicate appears. Unsafe alias
/// replacement after listing must fail before creating any turn or child.
#[test]
fn runtime_macro_shared_invocation_reselects_current_winner_and_rejects_swaps() {
    for change in ["project", "native", "unsafe"] {
        let base = temp_root(&format!("macro-shared-reselect-{change}"));
        let first = base.join("first");
        let second = base.join("second");
        write_shared_macro(&first, "FIRST_SHARED");
        write_shared_macro(&second, "SECOND_SHARED");
        let first = fs::canonicalize(first).unwrap();
        let second = fs::canonicalize(second).unwrap();
        let mut service = test_runtime_service();
        service
            .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service.set_config_root(base.join("user"));
        service.set_pane_current_working_directory("%1", first.clone());
        let mut trust = ProjectTrustStore::default();
        trust
            .decide(first.clone(), TrustDecision::Trusted, None)
            .unwrap();
        trust
            .decide(second.clone(), TrustDecision::Trusted, None)
            .unwrap();
        service.set_project_trust_store(trust, None);
        assert!(
            service
                .effective_macro_catalog_for_pane("%1")
                .get("review")
                .unwrap()
                .path
                .starts_with(&first)
        );
        match change {
            "project" => service.set_pane_current_working_directory("%1", second),
            "native" => {
                let native = first.join(".mezzanine/macros/review");
                fs::create_dir_all(&native).unwrap();
                fs::write(
                    native.join("MACRO.md"),
                    "---\nname: review\ndescription: Native\n---\n## Steps\n1. NATIVE_CURRENT\n",
                )
                .unwrap();
            }
            "unsafe" => {
                let path = first.join(".agents/macros/review/MACRO.md");
                fs::remove_file(&path).unwrap();
                std::os::unix::fs::symlink(second.join(".agents/macros/review/MACRO.md"), path)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let started = service.start_agent_macro_prompt_turn("%1", "#review");
        if change == "unsafe" {
            assert!(started.is_err());
            assert!(service.agent_turn_ledger().turns().is_empty());
            assert!(service.macro_managed_subagent_ids().is_empty());
        } else {
            let started = started.unwrap();
            let run = service.macro_run_for_tests(&started.turn_id).unwrap();
            assert_eq!(
                run.steps[0].scripted_prompt,
                if change == "project" {
                    "SECOND_SHARED"
                } else {
                    "NATIVE_CURRENT"
                }
            );
            assert_eq!(service.macro_managed_subagent_ids().len(), 1);
        }
        service.terminate_all_pane_processes().unwrap();
        fs::remove_dir_all(base).unwrap();
    }
}
