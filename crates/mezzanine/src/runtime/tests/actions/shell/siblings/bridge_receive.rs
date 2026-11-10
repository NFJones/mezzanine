//! Real persistent-spawn status delivery must not schedule over native custody.

use super::*;
use crate::runtime::current_unix_millis;
use mez_agent::messaging::{Envelope, MessageScope};

/// The ordered shell/spawn response emits its started status synchronously
/// before native dispatch stores the new execution. Check the original pending
/// intent before actor Prepare can retire it via stale-offer containment.
#[tokio::test(flavor = "current_thread")]
async fn runtime_native_persistent_bridge_does_not_offer_parent_provider() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};

    for spawn in [true, false] {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir().join(format!(
            "mez-native-bridge-receive-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let mut service = test_runtime_service();
        // Claim preparation uses an explicitly offline configured provider.
        // This fixture never executes its HTTP transport or the child's task.
        service.replace_config_layers(vec![ConfigLayer {
            name: "native-bridge-claim".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nrouting=false\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"native\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\n[permissions]\nsandbox=\"policy-only\"\n".into(),
        }]).unwrap();
        service.set_auth_store(crate::security::auth::AuthStore::new(
            crate::security::auth::AuthPaths::under_config_root(&root.join("auth")),
        ));
        service.set_agent_transcript_store(AgentTranscriptStore::new(root.join("transcripts")));
        service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
        service
            .attach_primary("primary", true, Size::new(100, 30).unwrap(), 200)
            .unwrap();
        service.start_initial_pane_process(None).unwrap();
        wait_until_primary_shell_foreground(&mut service, "%1");
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
        service.permission_policy_mut().set_approval_bypass(true);
        let parent = service
            .start_agent_prompt_turn("%1", "run shell and start reviewer")
            .unwrap();
        service.remove_pending_agent_provider_task(&parent.turn_id);
        let mut actions = vec![mez_agent::AgentAction {
            id: "bridge-shell".into(),
            payload: mez_agent::AgentActionPayload::ShellCommand {
                summary: "Execute original shell once".into(),
                command: format!(
                    "printf 'original-bridge-shell\\n' >> {}",
                    shlex::try_quote(root.join("effect").to_str().unwrap()).unwrap()
                ),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        }];
        if spawn {
            let mut action = runtime_spawn_agent_action("bridge-child", "review via MMP");
            let mez_agent::AgentActionPayload::SpawnAgent {
                lifetime,
                objective,
                ..
            } = &mut action.payload
            else {
                unreachable!()
            };
            *lifetime = mez_agent::SubagentLifetime::Persistent;
            *objective = Some("Review assigned work".into());
            actions.push(action);
        }
        let provider = RuntimeBatchProvider {
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".into(),
                model: "test".into(),
                raw_text: "native shell and persistent reviewer".into(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "Keep original native result ownership".into(),
                    actions,
                }),
                provider_transcript_events: Vec::new(),
            },
        };
        let applied = service
            .execute_agent_turn_with_provider_async(
                &parent.turn_id,
                &provider,
                runtime_model_profile("runtime-batch", "test"),
            )
            .await
            .unwrap();
        if applied.action_results[0].status != ActionStatus::Running {
            service.terminate_all_pane_processes().unwrap();
            fs::remove_dir_all(&root).unwrap();
            assert!(!root.exists());
            panic!(
                "native action was not admitted: {:?}",
                applied.action_results
            );
        }
        let pending_during_native = service.agent_provider_task_is_pending(&parent.turn_id);
        let status_count = service.agent_turn_contexts()[&parent.turn_id]
            .chronology()
            .iter()
            .filter(|event| {
                event.block().content.contains("task_status")
                    && event.block().content.contains("subagent task started")
            })
            .count();
        let children = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| turn.turn_id != parent.turn_id)
            .count();
        let agent = mez_core::ids::AgentId::opaque(parent.agent_id.clone()).unwrap();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let client = async {
            let claim = handle
                .claim_configured_agent_provider_task(agent.clone(), parent.turn_id.clone())
                .await;
            let dispatch = handle
                .claim_native_shell_action(parent.turn_id.clone(), "bridge-shell".into())
                .await
                .unwrap()
                .unwrap();
            let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
            let mut batch = crate::runtime::RuntimeEventBatch::new();
            batch.push(crate::runtime::RuntimeEvent::NativeShell(outcome.clone()));
            let completed = handle.submit_runtime_events(batch).await;
            let mut batch = crate::runtime::RuntimeEventBatch::new();
            batch.push(crate::runtime::RuntimeEvent::NativeShell(outcome));
            let duplicate = handle.submit_runtime_events(batch).await;
            let next_native = handle
                .claim_native_shell_action(parent.turn_id.clone(), "bridge-shell".into())
                .await
                .unwrap();
            let writer = crate::host::async_runtime::run_async_persistence_side_effect_service(
                &handle,
                crate::host::async_runtime::AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 100,
                    drain_limit: 32,
                    idle_interval: std::time::Duration::from_millis(1),
                },
                |_, _| false,
            )
            .await
            .unwrap();
            assert_eq!(writer.failed, 0);
            let pending_after_completion = handle.pending_agent_provider_tasks().await.unwrap();
            let continuation = handle
                .claim_configured_agent_provider_task(agent.clone(), parent.turn_id.clone())
                .await
                .map(|dispatch| dispatch.map(|dispatch| dispatch.context.into_agent_context()));
            let repeated_claim = handle
                .claim_configured_agent_provider_task(agent, parent.turn_id.clone())
                .await;
            handle.shutdown().await.unwrap();
            (
                claim,
                completed,
                duplicate,
                next_native,
                pending_after_completion,
                continuation,
                repeated_claim,
            )
        };
        let (
            (
                claim,
                completed,
                duplicate,
                next_native,
                pending_after_completion,
                continuation,
                repeated_claim,
            ),
            mut exit,
        ) = tokio::join!(client, actor.run());
        exit.service.terminate_all_pane_processes().unwrap();
        drop((exit, handle));
        let effect = fs::read_to_string(root.join("effect")).unwrap();
        fs::remove_dir_all(&root).unwrap();
        assert!(!root.exists());
        assert_eq!(effect, "original-bridge-shell\n");
        assert!(
            !pending_during_native,
            "spawn={spawn}: bridge created an unqualified parent offer"
        );
        assert!(
            claim.unwrap().is_none(),
            "native owner must retain parent admission"
        );
        assert_eq!(applied.terminal_state, AgentTurnState::Running);
        assert_eq!(children, usize::from(spawn));
        assert_eq!(
            status_count,
            usize::from(spawn),
            "actual started status must commit once"
        );
        assert_eq!(completed.unwrap().applied, 1);
        assert_eq!(duplicate.unwrap().applied, 0);
        assert!(next_native.is_none());
        assert_eq!(
            pending_after_completion
                .iter()
                .filter(|task| task.turn_id == parent.turn_id)
                .count(),
            1,
            "actual shell completion must release one legitimate continuation"
        );
        let context = continuation
            .unwrap()
            .expect("actual post-ACK claim must succeed");
        assert_eq!(
            context
                .blocks()
                .iter()
                .filter(|block| {
                    block.source == mez_agent::ContextSourceKind::ActionResult
                        && block.content.contains("bridge-shell")
                        && block.content.contains("succeeded")
                })
                .count(),
            1,
            "claimed continuation must consume the original settled shell result"
        );
        assert!(
            repeated_claim.unwrap().is_none(),
            "one continuation cannot be claimed twice"
        );
    }
}

/// Authentic model self-mail is allowed to commit during response application,
/// but neither missing nor older settled execution can authorize a new offer.
/// The ordinary native completion remains the single continuation owner.
#[tokio::test(flavor = "current_thread")]
async fn runtime_native_reentrant_model_mail_requires_current_settled_execution() {
    for older in [false, true] {
        let mut service = test_runtime_service();
        service
            .attach_primary("primary", true, Size::new(100, 30).unwrap(), 200)
            .unwrap();
        service.start_initial_pane_process(None).unwrap();
        wait_until_primary_shell_foreground(&mut service, "%1");
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
        service.permission_policy_mut().set_approval_bypass(true);
        let parent = service
            .start_agent_prompt_turn("%1", "receive authentic model mail")
            .unwrap();
        if older {
            service
                .execute_agent_turn_with_provider(
                    &parent.turn_id,
                    &RuntimeBatchProvider {
                        response: runtime_say_response(
                            &parent.turn_id,
                            "earlier settled response",
                            false,
                        ),
                    },
                    runtime_model_profile("runtime-batch", "test"),
                )
                .unwrap();
        }
        service.remove_pending_agent_provider_task(&parent.turn_id);
        let mut response = runtime_say_response(&parent.turn_id, "new applying response", false);
        response.action_batch.as_mut().unwrap().actions = vec![
            mez_agent::AgentAction {
                id: "mail-shell".into(),
                payload: mez_agent::AgentActionPayload::ShellCommand {
                    summary: "Own the original native result".into(),
                    command: "printf 'native-model-mail\\n'".into(),
                    interactive: false,
                    stateful: false,
                    timeout_ms: None,
                },
            },
            mez_agent::AgentAction {
                id: "model-self-mail".into(),
                payload: mez_agent::AgentActionPayload::SendMessage {
                    expect_reply: false,
                    recipient: format!("agent:{}", parent.agent_id),
                    scope: Some("session".into()),
                    content_type: "text/plain; charset=utf-8".into(),
                    payload: "authentic reentrant model mail".into(),
                    correlation_id: None,
                },
            },
        ];
        let applied = service
            .execute_agent_turn_with_provider_async(
                &parent.turn_id,
                &RuntimeBatchProvider { response },
                runtime_model_profile("runtime-batch", "test"),
            )
            .await
            .unwrap();
        let pending = service.agent_provider_task_is_pending(&parent.turn_id);
        let mail_count = service.agent_turn_contexts()[&parent.turn_id]
            .chronology()
            .iter()
            .filter(|event| {
                event.block().source == mez_agent::ContextSourceKind::PeerMessage
                    && event
                        .block()
                        .content
                        .contains("authentic reentrant model mail")
            })
            .count();
        let dispatch = service
            .claim_native_shell_action(&parent.turn_id, "mail-shell")
            .unwrap()
            .unwrap();
        let completed = service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(dispatch));
        let continuation = service.agent_provider_task_is_pending(&parent.turn_id);
        service.terminate_all_pane_processes().unwrap();
        assert_eq!(applied.action_results[1].status, ActionStatus::Succeeded);
        assert_eq!(mail_count, 1);
        assert!(
            !pending,
            "older={older}: reentrant mail borrowed absent/old execution"
        );
        assert!(completed.unwrap());
        assert!(
            continuation,
            "held model mail must be consumed by legitimate native continuation"
        );
    }
}

/// Once the native batch has really settled, status is still reference-only.
/// Model mail, unlike the same bridge payload, may offer the settled group.
#[test]
fn runtime_native_settled_receive_distinguishes_bridge_from_model_mail() {
    let (mut service, _) = admit_siblings(Discovery::Absent, AgentLogLevel::Verbose, None);
    for action in ["shell-a", "shell-b"] {
        let dispatch = service
            .claim_native_shell_action("turn-1", action)
            .unwrap()
            .unwrap();
        service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(dispatch))
            .unwrap();
    }
    service.remove_pending_agent_provider_task("turn-1");
    let now = current_unix_millis();
    let sender = service
        .ensure_runtime_message_identity("agent-late-mail", None, "agent", &[], now)
        .unwrap();
    let recipient = AgentId::opaque(
        service
            .agent_turn_ledger()
            .turn("turn-1")
            .unwrap()
            .agent_id
            .clone(),
    )
    .unwrap();
    let mut offers = Vec::new();
    for bridge in [true, false] {
        let envelope = Envelope {
            protocol: "mmp/1",
            id: format!("settled-mail-{bridge}"),
            message_type: if bridge { "task_status" } else { "send" }.into(),
            time: format!("runtime:{now}"),
            sender: sender.clone(),
            recipient: mez_agent::messaging::Recipient::Agent(recipient.clone()),
            correlation_id: None,
            ttl_ms: None,
            content_type: if bridge {
                "application/json"
            } else {
                "text/plain"
            }
            .into(),
            payload: if bridge {
                r#"{"task_id":"late-status","state":"running","summary":"reference only"}"#.into()
            } else {
                "authentic post-settlement model mail".into()
            },
            extension_fields: if bridge {
                crate::runtime::control::runtime_bridge_extension_fields()
            } else {
                Vec::new()
            },
        };
        service
            .message_service_mut()
            .accept_at_with_scope(&sender.agent_id, envelope, MessageScope::Session, now)
            .unwrap();
        service.deliver_pending_runtime_agent_messages(now).unwrap();
        offers.push(service.agent_provider_task_is_pending("turn-1"));
    }
    service.terminate_all_pane_processes().unwrap();
    assert_eq!(offers, [false, true]);
}
