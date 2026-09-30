//! Native lifecycle failure settlement and bounded model-feedback regressions.
//!
//! Trusted status facts must survive the worker boundary without causing the
//! runtime to repeat commands whose effects may already have happened.

use super::*;

/// A real child can perform effects and omit its trusted completion record.
/// Settlement must retain diagnostic facts in model feedback, reject duplicate
/// outcomes, and never redispatch that uncertain action automatically.
#[test]
fn runtime_native_incomplete_lifecycle_preserves_diagnostics_without_replay() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("native-lifecycle-transcript"));
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .execute_agent_shell_command(&primary, "inspect native status")
        .unwrap();
    let counter = temp_root("native-incomplete-effect").join("effects");
    fs::create_dir_all(counter.parent().unwrap()).unwrap();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native diagnostic fixture".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "inspect once".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Inspect once".to_string(),
                        command: format!("printf x >> '{}'", counter.display()),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(5_000),
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let mut dispatch = service
        .claim_native_shell_action("turn-1", "shell-1")
        .unwrap()
        .unwrap();
    dispatch.sandbox_backend = Some(crate::runtime::SandboxBackend::Bubblewrap);
    dispatch.request.transaction = dispatch.request.transaction.with_child_launch(
        mez_agent::ShellChildLaunch::new("/bin/sh", vec![
            mez_agent::ShellChildArgument::Literal("-c".to_string()),
            mez_agent::ShellChildArgument::Literal("printf '{\"child-pid\":%s}\\n' \"$$\" >&3; /bin/sh \"$1\"; printf '%s\\n' 'launcher detail' '{\"nested\":{\"password\":\"opaque-secret\",\"access_token\":\"opaque-credential\"}}' >&2; exit 7".to_string()),
            mez_agent::ShellChildArgument::Literal("sh".to_string()),
            mez_agent::ShellChildArgument::MaterializedCommandFile,
        ]).unwrap().with_status_fd(3).unwrap(),
    );
    let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
    let evidence = outcome
        .result
        .as_ref()
        .unwrap_err()
        .lifecycle
        .as_ref()
        .unwrap();
    assert_eq!(evidence.child_record_present, Some(true));
    assert_eq!(evidence.outer_exit_code, Some(7));
    assert!(!evidence.stderr.contains("opaque-secret"));
    assert!(!evidence.stderr.contains("opaque-credential"));
    assert!(
        service
            .complete_native_shell_action(outcome.clone())
            .unwrap()
    );
    assert!(!service.complete_native_shell_action(outcome).unwrap());
    assert!(service.pending_native_shell_actions().is_empty());
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = store.inspect(&conversation).unwrap();
    let feedback = entries
        .iter()
        .find(|entry| {
            entry.role == TranscriptRole::Tool && entry.content.contains("sandbox_lifecycle")
        })
        .expect("bounded lifecycle evidence must survive the model-facing action-error projection");
    assert!(
        feedback.content.contains("missing_exit"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("launcher detail"),
        "{}",
        feedback.content
    );
    assert!(
        feedback.content.contains("\"automatic_replay\":false"),
        "{}",
        feedback.content
    );
    assert_eq!(fs::read(&counter).unwrap(), b"x");
    assert!(
        entries
            .iter()
            .all(|entry| !entry.content.contains("opaque-secret")
                && !entry.content.contains("opaque-credential"))
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(counter.parent().unwrap());
}
