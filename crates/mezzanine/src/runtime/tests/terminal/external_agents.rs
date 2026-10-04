//! Restricted external-agent lifecycle ingress regressions.
//!
//! Hook credentials authorize only their server-issued launch, not client roles,
//! pane reassignment, approvals or input. Tests exercise the framed runtime owner
//! with kernel-authenticated peer metadata rather than payload identity claims.

use super::*;

/// Canonical retired harness launch is rejected before capability allocation.
/// Rejection cannot consume a launch generation or create an identity; valid
/// other-harness launches retain their existing behavior.
#[test]
fn runtime_external_retired_gemini_cannot_allocate_launch() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"retired","method":"agent/external/launch","params":{"pane_id":"%1","harness":"gemini","version":"fixture"}}"#, &primary,
    );
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert!(response.get("error").is_some());
    assert!(response.to_string().contains("retired"));
    assert!(service.external_agent_rows().is_empty());
    let accepted = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"active","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#, &primary,
    );
    let accepted: serde_json::Value = serde_json::from_str(&accepted).unwrap();
    assert_eq!(accepted["result"]["generation"], 1);
    service.terminate_all_pane_processes().unwrap();
}

/// Retiring an observational registration must preserve shared pane, window and
/// session traffic, including acceptance receipts used to deduplicate retries.
#[test]
fn runtime_external_retirement_preserves_shared_message_traffic() {
    use mez_agent::messaging::{Envelope, MessageScope, Recipient};
    use mez_core::ids::PaneId;
    for operation in ["deregister", "expire", "restore"] {
        let mut service = test_runtime_service();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service.start_initial_pane_process(None).unwrap();
        let launch: serde_json::Value = serde_json::from_str(&service.dispatch_runtime_control_body(
            r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#, &primary,
        )).unwrap();
        let params = serde_json::json!({"launch_token":launch["result"]["launch_token"],"generation":launch["result"]["generation"],"external_session_id":"shared-run","display_name":"Shared Fixture"});
        let mut hook = ControlConnectionState::new(true, false);
        hook.bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
            crate::runtime::current_effective_uid(),
        ))
        .unwrap();
        let registered = external_request(
            &mut service,
            &mut hook,
            "agent/external/register",
            params.clone(),
        );
        let external_id = registered["result"]["agent_id"].as_str().unwrap();
        let sender = service
            .message_service_mut()
            .register_agent(None, None, "native", Vec::new());
        let descriptor = service.find_pane_descriptor("%1").unwrap();
        let sibling = service.message_service_mut().register_agent(
            PaneId::opaque("%1".to_string()),
            Some(descriptor.window_id.clone()),
            "native",
            Vec::new(),
        );
        service
            .message_service_mut()
            .subscribe_from_retained_start(&sibling.agent_id)
            .unwrap();
        for (index, recipient) in [
            Recipient::Pane(PaneId::opaque("%1".to_string()).unwrap()),
            Recipient::Window(descriptor.window_id),
            Recipient::Session,
        ]
        .into_iter()
        .enumerate()
        {
            service
                .message_service_mut()
                .accept_at_with_scope(
                    &sender.agent_id,
                    Envelope {
                        protocol: "mmp/1",
                        id: format!("shared-{index}"),
                        message_type: "send".to_string(),
                        time: "runtime:0".to_string(),
                        sender: sender.clone(),
                        recipient,
                        correlation_id: None,
                        ttl_ms: None,
                        content_type: "text/plain; charset=utf-8".to_string(),
                        payload: format!("shared payload {index}"),
                        extension_fields: Vec::new(),
                    },
                    MessageScope::Session,
                    0,
                )
                .unwrap();
        }
        let before = service.message_service().snapshot_state();
        match operation {
            "deregister" => {
                assert!(external_request(&mut service, &mut hook, "agent/external/deregister", serde_json::json!({"launch_token":params["launch_token"],"generation":params["generation"],"external_session_id":"shared-run"})).get("result").is_some());
            }
            "expire" => {
                service.expire_external_agent_for_tests(external_id);
                service.reconcile_external_agent_registrations();
            }
            _ => {
                let mut snapshot = crate::storage::snapshot::SessionSnapshotPayload::from_session(
                    service.session(),
                );
                snapshot.message_state = Some(before.clone());
                service
                    .restore_message_state_for_restored_snapshot(&snapshot)
                    .unwrap();
            }
        }
        let after = service.message_service().snapshot_state();
        assert_eq!(
            after.retained_messages, before.retained_messages,
            "operation={operation}"
        );
        assert_eq!(
            after.accepted_messages, before.accepted_messages,
            "operation={operation}"
        );
        assert!(
            service
                .message_service()
                .registered_identity(&sibling.agent_id)
                .is_some()
        );
        service.terminate_all_pane_processes().unwrap();
    }
}

/// Foreground fallback is useful for shell classification but cannot prove the
/// pane-root incarnation required to issue an external launch capability.
#[test]
fn runtime_external_launch_rejects_foreground_identity_fallback() {
    use crate::runtime::processes::{RuntimePaneProcessIdentityInjection, RuntimePaneProcessRole};
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let started = service.start_initial_pane_process(None).unwrap();
    service.inject_pane_process_identity_for_tests(
        "%1",
        RuntimePaneProcessIdentityInjection::Identity {
            role: RuntimePaneProcessRole::ForegroundProcessGroupLeader,
            generation: None,
            process_id: started.primary_pid,
            start_token: 1,
            executable_path: PathBuf::from("/bin/sh"),
            live_start_token: None,
        },
    );
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#, &primary,
    );
    service.terminate_all_pane_processes().unwrap();
    assert!(
        response.contains("error"),
        "foreground identity admitted a launch"
    );
}

/// Real Unix control framing must admit only the issued capability, leave the
/// hook connection uninitialized, and preserve its registration after EOF.
#[tokio::test(flavor = "current_thread")]
async fn runtime_external_registration_round_trips_authenticated_unix_transport() {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
        serve_async_runtime_control_connection,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let launch: serde_json::Value = serde_json::from_str(&service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#, &primary,
    )).unwrap();
    let clients_before = service.session().clients().len();
    let request = serde_json::json!({"jsonrpc":"2.0","id":"register","method":"agent/external/register",
        "params":{"launch_token":launch["result"]["launch_token"],"generation":launch["result"]["generation"],
            "external_session_id":"unix-run","display_name":"Unix Fixture"}}).to_string();
    let input = crate::control::encode_control_body(&request);
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let (mut client_stream, mut server_stream) = tokio::net::UnixStream::pair().unwrap();
    let client = async {
        client_stream.write_all(&input).await.unwrap();
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        let (body, _) = crate::control::decode_control_frame(&output[..read], 4096).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["result"]["registered"], true, "{value}");
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, false);
        serve_async_runtime_control_connection(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, crate::runtime::current_effective_uid())
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(!connection.initialized());
        assert!(connection.caller_client_id().is_none());
        handle.shutdown().await.unwrap();
    };
    let ((), (), mut exit) = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        tokio::join!(client, server, actor.run())
    })
    .await
    .unwrap();
    assert_eq!(exit.service.session().clients().len(), clients_before);
    assert_eq!(exit.service.external_agent_rows().len(), 1);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// A replaced root loses its external projection immediately, even when cleanup
/// has not run. Independent client-owned status remains available underneath.
#[test]
fn runtime_external_presentation_immediate_root_replacement_hides_old_owner() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let root = temp_root("external-browser-project");
    fs::create_dir_all(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let mut trust = crate::security::project::ProjectTrustStore::default();
    trust
        .decide_at(
            root.clone(),
            crate::security::project::TrustDecision::Trusted,
            None,
            1,
        )
        .unwrap();
    service.set_project_trust_store(trust, None);
    service.set_pane_current_working_directory("%1", root.clone());
    let launch: serde_json::Value = serde_json::from_str(&service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#, &primary)).unwrap();
    let base = serde_json::json!({"launch_token":launch["result"]["launch_token"],"generation":launch["result"]["generation"],"external_session_id":"replaced-run"});
    let mut hook = ControlConnectionState::new(true, false);
    hook.bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
        crate::runtime::current_effective_uid(),
    ))
    .unwrap();
    let mut register = base.clone();
    register["display_name"] = serde_json::json!("Replaced fixture");
    assert!(
        external_request(&mut service, &mut hook, "agent/external/register", register)
            .get("result")
            .is_some()
    );
    service.presentation.set_pane_harness_status(
        "%1",
        "independent-client",
        Some(crate::runtime::RuntimePaneHarnessStatus {
            state: "complete".into(),
            text: Some("Independent".into()),
        }),
    );
    let mut update = base;
    update["sequence"] = serde_json::json!(1);
    update["state"] = serde_json::json!("running");
    update["title"] = serde_json::json!("Old root task");
    assert!(
        external_request(
            &mut service,
            &mut hook,
            "agent/external/presentation",
            update
        )
        .get("result")
        .is_some()
    );
    assert_eq!(
        service.external_agent_pane_title("%1").as_deref(),
        Some("Old root task")
    );
    let (browser, targets) = service.agent_management_browser(&primary).unwrap();
    let external = browser
        .records()
        .iter()
        .find(|record| record.title == "Replaced fixture")
        .unwrap();
    assert!(
        external
            .metadata
            .contains(&("Project".into(), root.to_string_lossy().into_owned()))
    );
    assert!(targets[&external.id].lifecycle.is_none());
    service.terminate_all_pane_processes().unwrap();
    let descriptor = service.find_pane_descriptor("%1").unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("cat"), None)
        .unwrap();
    let frame = service.terminal_frame_context();
    assert!(frame.panes["%1"].pane_title_override.is_none());
    assert_eq!(
        frame.panes["%1"].pane_status_state.as_deref(),
        Some("complete")
    );
    assert_eq!(
        frame.panes["%1"].pane_status_text.as_deref(),
        Some("Independent")
    );
    let (browser, _) = service.agent_management_browser(&primary).unwrap();
    assert!(
        !browser
            .records()
            .iter()
            .any(|record| record.title == "Replaced fixture")
    );
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Multiple launches retain independent title/status ownership after old end
/// events; expiry removes only the matching lease without claiming process death.
#[test]
fn runtime_external_presentation_retirement_preserves_other_launch() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let mut hook = ControlConnectionState::new(true, false);
    hook.bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
        crate::runtime::current_effective_uid(),
    ))
    .unwrap();
    let mut runs = Vec::new();
    for name in ["first", "second"] {
        let launch: serde_json::Value = serde_json::from_str(&service.dispatch_runtime_control_body(
            r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#, &primary)).unwrap();
        let base = serde_json::json!({"launch_token":launch["result"]["launch_token"],"generation":launch["result"]["generation"],"external_session_id":name});
        let mut register = base.clone();
        register["display_name"] = serde_json::json!(name);
        let registered =
            external_request(&mut service, &mut hook, "agent/external/register", register);
        let id = registered["result"]["agent_id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut update = base.clone();
        update["sequence"] = serde_json::json!(1);
        update["state"] = serde_json::json!("running");
        update["title"] = serde_json::json!(name);
        assert!(
            external_request(
                &mut service,
                &mut hook,
                "agent/external/presentation",
                update
            )
            .get("result")
            .is_some()
        );
        runs.push((base, id));
    }
    assert_eq!(
        service.external_agent_pane_title("%1").as_deref(),
        Some("second")
    );
    assert!(
        external_request(
            &mut service,
            &mut hook,
            "agent/external/deregister",
            runs[0].0.clone()
        )
        .get("result")
        .is_some()
    );
    assert_eq!(
        service.external_agent_pane_title("%1").as_deref(),
        Some("second")
    );
    assert_eq!(
        service.terminal_frame_context().panes["%1"]
            .pane_status_state
            .as_deref(),
        Some("running")
    );
    service.expire_external_agent_for_tests(&runs[1].1);
    assert!(service.external_agent_pane_title("%1").is_none());
    assert!(
        service.terminal_frame_context().panes["%1"]
            .pane_status_state
            .is_none()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Reconnecting hooks retain registration-owned presentation, reject stale
/// sequence updates, preserve explicit title pins, and clear only their run.
#[test]
fn runtime_external_presentation_is_registration_owned_and_sequence_fenced() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let launch: serde_json::Value = serde_json::from_str(&service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#, &primary)).unwrap();
    let base = serde_json::json!({"launch_token":launch["result"]["launch_token"],"generation":launch["result"]["generation"],"external_session_id":"presentation-run"});
    let mut hook = ControlConnectionState::new(true, false);
    hook.bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
        crate::runtime::current_effective_uid(),
    ))
    .unwrap();
    let mut register = base.clone();
    register["display_name"] = serde_json::json!("External Fixture");
    assert!(
        external_request(&mut service, &mut hook, "agent/external/register", register)
            .get("result")
            .is_some()
    );
    let mut update = base.clone();
    update["sequence"] = serde_json::json!(1);
    update["state"] = serde_json::json!("running");
    update["title"] = serde_json::json!("External task");
    let result = external_request(
        &mut service,
        &mut hook,
        "agent/external/presentation",
        update.clone(),
    );
    assert!(result.get("result").is_some(), "{result}");
    assert_eq!(
        service.terminal_frame_context().panes["%1"]
            .pane_title_override
            .as_deref(),
        Some("External task")
    );
    assert!(!hook.initialized());
    assert_eq!(
        external_request(
            &mut service,
            &mut hook,
            "agent/external/presentation",
            update.clone()
        )["result"]["changed"],
        false
    );
    update["state"] = serde_json::json!("failed");
    assert!(
        external_request(
            &mut service,
            &mut hook,
            "agent/external/presentation",
            update.clone()
        )
        .get("error")
        .is_some()
    );
    service
        .session
        .set_pane_title_explicit("%1", "Pinned")
        .unwrap();
    assert!(
        service.terminal_frame_context().panes["%1"]
            .pane_title_override
            .is_none()
    );
    assert!(
        external_request(
            &mut service,
            &mut hook,
            "agent/external/deregister",
            base.clone()
        )
        .get("result")
        .is_some()
    );
    update["sequence"] = serde_json::json!(2);
    assert!(
        external_request(
            &mut service,
            &mut hook,
            "agent/external/presentation",
            update
        )
        .get("error")
        .is_some()
    );
    assert!(
        service.terminal_frame_context().panes["%1"]
            .pane_status_state
            .is_none()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Sends one bounded capability request through the runtime connection owner.
fn external_request(
    service: &mut RuntimeSessionService,
    connection: &mut ControlConnectionState,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let request = serde_json::json!({"jsonrpc":"2.0","id":"external-fixture","method":method,"params":params}).to_string();
    serde_json::from_str(
        &service.dispatch_runtime_control_body_for_connection(&request, connection),
    )
    .unwrap()
}

/// Identical registration retries and reconnects preserve identity. Wrong UID,
/// generation, metadata or initialized role cannot acquire or broaden the lease.
/// An old end event retires only its own run, leaving another registration alive.
#[test]
fn runtime_external_registration_is_restricted_retry_safe_and_exact() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let launch_request = r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#;
    let launch: serde_json::Value =
        serde_json::from_str(&service.dispatch_runtime_control_body(launch_request, &primary))
            .unwrap();
    let token = launch["result"]["launch_token"].as_str().unwrap();
    let generation = launch["result"]["generation"].as_u64().unwrap();
    let params = serde_json::json!({"launch_token":token,"generation":generation,
        "external_session_id":"run-one","display_name":"External Fixture","objective":"Bounded fixture"});
    let mut hook = ControlConnectionState::new(true, false);
    hook.bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
        crate::runtime::current_effective_uid(),
    ))
    .unwrap();
    let registered = external_request(
        &mut service,
        &mut hook,
        "agent/external/register",
        params.clone(),
    );
    assert!(registered.get("result").is_some(), "{registered}");
    assert!(!hook.initialized());
    let agent_id = registered["result"]["agent_id"]
        .as_str()
        .unwrap()
        .to_string();
    let retry = external_request(
        &mut service,
        &mut hook,
        "agent/external/register",
        params.clone(),
    );
    assert_eq!(registered["result"], retry["result"]);
    let mut wrong_uid = ControlConnectionState::new(true, false);
    wrong_uid
        .bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
            crate::runtime::current_effective_uid().saturating_add(1),
        ))
        .unwrap();
    assert!(
        external_request(
            &mut service,
            &mut wrong_uid,
            "agent/external/register",
            params.clone()
        )
        .get("error")
        .is_some()
    );
    let mut changed = params.clone();
    changed["generation"] = serde_json::json!(generation + 1);
    assert!(
        external_request(&mut service, &mut hook, "agent/external/register", changed)
            .get("error")
            .is_some()
    );
    let mut changed = params.clone();
    changed["pane_id"] = serde_json::json!("%2");
    assert!(
        external_request(&mut service, &mut hook, "agent/external/register", changed)
            .get("error")
            .is_some()
    );
    let mut changed = params.clone();
    changed["display_name"] = serde_json::json!("conflicting name");
    assert!(
        external_request(&mut service, &mut hook, "agent/external/register", changed)
            .get("error")
            .is_some()
    );
    let mut initialized = ControlConnectionState::trusted_existing_client(primary.clone());
    initialized
        .bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
            crate::runtime::current_effective_uid(),
        ))
        .unwrap();
    assert!(
        external_request(
            &mut service,
            &mut initialized,
            "agent/external/register",
            params.clone()
        )
        .get("error")
        .is_some()
    );
    assert!(
        external_request(
            &mut service,
            &mut hook,
            "pane/close",
            serde_json::json!({"pane_id":"%1","force":true,"idempotency_key":"forbidden"})
        )
        .get("error")
        .is_some()
    );
    assert!(service.find_pane_descriptor("%1").is_some());
    let list: serde_json::Value = serde_json::from_str(&service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"agent/list","params":{}}"#,
        &primary,
    ))
    .unwrap();
    let row = list["result"]["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["agent_id"] == agent_id)
        .unwrap();
    assert_eq!(row["harness"], "codex");
    assert_eq!(row["controls"], serde_json::json!([]));
    assert!(!list.to_string().contains(token));
    let second: serde_json::Value =
        serde_json::from_str(&service.dispatch_runtime_control_body(launch_request, &primary))
            .unwrap();
    let second_params = serde_json::json!({"launch_token":second["result"]["launch_token"],"generation":second["result"]["generation"],"external_session_id":"run-two","display_name":"Second Fixture"});
    let second_registered = external_request(
        &mut service,
        &mut hook,
        "agent/external/register",
        second_params.clone(),
    );
    assert!(second_registered.get("result").is_some());
    let end = serde_json::json!({"launch_token":token,"generation":generation,"external_session_id":"run-one"});
    assert_eq!(
        external_request(
            &mut service,
            &mut hook,
            "agent/external/deregister",
            end.clone()
        )["result"]["changed"],
        true
    );
    assert_eq!(
        external_request(&mut service, &mut hook, "agent/external/deregister", end)["result"]["changed"],
        false
    );
    assert!(
        external_request(&mut service, &mut hook, "agent/external/renew", params)
            .get("error")
            .is_some()
    );
    assert_eq!(service.external_agent_rows().len(), 1);
    let second_id = second_registered["result"]["agent_id"].as_str().unwrap();
    service.expire_external_agent_for_tests(second_id);
    assert!(service.external_agent_rows().is_empty());
    assert_eq!(service.reconcile_external_agent_registrations(), 1);
    assert!(
        service
            .message_service()
            .registered_identity(&AgentId::opaque(second_id.to_string()).unwrap())
            .is_none()
    );
    service
        .dispatch_runtime_pane_close(&primary, r#"{"pane_id":"%1","force":true}"#)
        .unwrap();
    assert!(service.external_agent_rows().is_empty());
    assert!(
        external_request(
            &mut service,
            &mut hook,
            "agent/external/renew",
            second_params
        )
        .get("error")
        .is_some()
    );
}

/// Restoring message metadata cannot revive an external run whose launch
/// capability was runtime-only. Both old credentials and live discovery are
/// retired while unrelated native identities remain owned by their lifecycle.
#[test]
fn runtime_external_registration_snapshot_restore_invalidates_credentials() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let launch: serde_json::Value = serde_json::from_str(&service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#, &primary,
    )).unwrap();
    let params = serde_json::json!({"launch_token":launch["result"]["launch_token"],"generation":launch["result"]["generation"],"external_session_id":"snapshot-run","display_name":"Snapshot Fixture"});
    let mut hook = ControlConnectionState::new(true, false);
    hook.bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
        crate::runtime::current_effective_uid(),
    ))
    .unwrap();
    let registered = external_request(
        &mut service,
        &mut hook,
        "agent/external/register",
        params.clone(),
    );
    assert!(registered.get("result").is_some(), "{registered}");
    let mut snapshot =
        crate::storage::snapshot::SessionSnapshotPayload::from_session(service.session());
    snapshot.message_state = Some(service.message_service().snapshot_state());
    service
        .restore_message_state_for_restored_snapshot(&snapshot)
        .unwrap();
    assert!(service.external_agent_rows().is_empty());
    assert!(
        service
            .message_service()
            .discover_agents_filtered_session_wide(
                None,
                None,
                None,
                Some("external-harness"),
                None,
                &[]
            )
            .is_empty()
    );
    assert!(
        external_request(&mut service, &mut hook, "agent/external/renew", params)
            .get("error")
            .is_some()
    );
    service.terminate_all_pane_processes().unwrap();
}
