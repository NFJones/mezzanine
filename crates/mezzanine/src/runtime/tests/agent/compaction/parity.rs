//! Common source selection at the real runtime/model/publication boundaries.
//! Active operations finish private inference and are cancelled before storage,
//! so all three paths see the same original closed source. Completing the live
//! task adds protected arrivals, which the final manual publication must retain.

use super::*;
use crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger;

fn frozen_selection(service: &RuntimeSessionService) -> (mez_agent::ReplayCompactionPlan, String) {
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    (
        task.replay_source.as_ref().unwrap().frozen_plan().clone(),
        crate::test_support::compaction_source::rendered_source(task).unwrap(),
    )
}

/// A changed registry selects a compactor with a smaller actual input allowance
/// than its running parent's retained profile. Each API/retention case exercises
/// both active trigger owners, real common inference, and the idle command's
/// actual durable publication. No fake completion or source authority is minted.
#[test]
fn runtime_all_compaction_triggers_preserve_source_tail_and_actual_projection() {
    for api in [
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
    ] {
        for retention in [1, 100] {
            let (mut service, store, turn) =
                crate::test_support::compaction_source::recovery_source(
                    test_runtime_service(),
                    temp_root("compaction-trigger-parity"),
                    &format!("ORIGINAL_SHARED_SOURCE {}", "source ".repeat(5_000)),
                    &format!("ORIGINAL_SHARED_TAIL {}", "tail ".repeat(140)),
                    retention,
                );
            service
                .replace_config_layers(vec![ConfigLayer {
                    name: "compaction-trigger-parity".into(),
                    path: None,
                    format: ConfigFormat::Toml,
                    scope: ConfigScope::Primary,
                    trusted: true,
                    text: format!(
                        r#"[agents]
default_provider="runtime-batch"
default_model_profile="provider-context-limit-test"
shell_mode="pane"
compaction_raw_retention_percent={retention}
[providers.runtime-batch]
kind="openai"
api="{}"
models=["test"]
default_model="test"
[model_profiles.provider-context-limit-test]
provider="runtime-batch"
model="test"
context_window_tokens=40000
max_input_tokens=20000
"#,
                        api.as_str()
                    ),
                }])
                .unwrap();
            let profile = service
                .provider_registry()
                .profile("provider-context-limit-test")
                .unwrap()
                .clone();
            assert_eq!(profile.max_input_tokens(), Some(20_000));
            assert_eq!(
                service
                    .agent_turn_model_profile(&turn)
                    .unwrap()
                    .max_input_tokens(),
                None
            );
            let original = service.agent_turn_contexts()[&turn].clone();
            let mut selections = Vec::new();
            for trigger in [
                RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                    observed_input_tokens: 20_000,
                    max_input_tokens: 20_000,
                },
                RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { attempt: 1 },
            ] {
                assert!(
                    service
                        .queue_agent_active_turn_compaction(
                            &turn,
                            "provider-context-limit-test".into(),
                            profile.clone(),
                            trigger,
                        )
                        .unwrap()
                );
                let selection = frozen_selection(&service);
                let calls = run_runtime_test_compaction_wave(
                    &mut service,
                    "%1",
                    Ok(runtime_test_compaction_response("PRIVATE_PARITY_HEAD")),
                );
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].max_input_tokens, Some(20_000));
                assert_eq!(service.agent_turn_contexts()[&turn], original);
                let attempt = service
                    .take_replay_epoch_attempt_for_tests()
                    .expect("original accepted private publication owner");
                assert!(
                    service
                        .cancel_current_agent_compaction_task("%1")
                        .had_task()
                );
                let retired = attempt.execute();
                assert!(retired.error().is_some());
                assert!(
                    !service
                        .apply_replay_epoch_completion(retired)
                        .unwrap()
                        .applied
                );
                assert!(!service.agent_is_compacting("%1"));
                selections.push(selection);
            }
            let (expected, rendered) = &selections[0];
            assert_eq!(selections[1].0.summary_input(), expected.summary_input());
            assert_eq!(selections[1].0.retained_tail(), expected.retained_tail());
            assert_eq!(
                selections[1].0.arrival_frontier(),
                expected.arrival_frontier()
            );
            assert_eq!(selections[1].1, *rendered);
            assert_eq!(expected.expected_parent().get(), 0);
            let projection =
                mez_agent::ReplayProviderProjection::new(api, "runtime-batch", "test").unwrap();
            assert_eq!(expected.provider_projection(), Some(&projection));
            assert_eq!(selections[1].0.provider_projection(), Some(&projection));

            crate::test_support::compaction_source::complete_task(
                &mut service,
                &turn,
                "actual foreground task completed",
                api,
            );
            let conversation = service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .session_id
                .clone();
            let primary = service.session.layout_owner_client_id().unwrap().clone();
            let result = service
                .execute_agent_shell_command(&primary, "/compact")
                .unwrap();
            assert!(result.contains("state=queued"), "{result}");
            let (manual, manual_rendered) = frozen_selection(&service);
            assert_eq!(manual.expected_parent(), expected.expected_parent());
            assert_eq!(manual.provider_projection(), Some(&projection));
            assert_eq!(manual.summary_input(), expected.summary_input());
            assert_eq!(manual_rendered, *rendered);
            // Closure adds actual source after the original frozen frontier;
            // every originally retained occurrence remains exact and ordered.
            assert!(manual.retained_tail().starts_with(expected.retained_tail()));
            assert!(manual.arrival_frontier() > expected.arrival_frontier());
            let calls = run_runtime_test_compaction_wave(
                &mut service,
                "%1",
                Ok(runtime_test_compaction_response("COMMITTED_PARITY_HEAD")),
            );
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].max_input_tokens, Some(20_000));
            let receipt = replay_publication::execute_pending_publication(&mut service);
            assert!(
                service
                    .apply_replay_epoch_completion(receipt)
                    .unwrap()
                    .applied
            );
            let (_, cold) = store
                .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                .unwrap()
                .unwrap();
            assert_eq!(cold.revision().get(), 1);
            assert_eq!(
                cold.context().chronology()[0].block().content,
                "COMMITTED_PARITY_HEAD"
            );
            assert_eq!(&cold.context().chronology()[1..], manual.retained_tail());
            assert!(!service.agent_is_compacting("%1"));
            assert!(service.pending_agent_provider_tasks().is_empty());
        }
    }
}
