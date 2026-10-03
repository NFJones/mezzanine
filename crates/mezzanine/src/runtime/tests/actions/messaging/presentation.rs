//! Peer-message labels, echo visibility and bridge provenance regressions.
//!
//! Echoes follow accepted/committed occurrences and never elevate peer payloads
//! into user instructions. Endpoint labels derive from trusted identity lineage.

use super::fixtures::peer_echo_pane_lines;
use super::*;

/// Verifies concrete-agent peer labels ignore mutable pane titles, prefer
/// trusted subagent names, and otherwise retain the canonical runtime id.
#[test]
fn runtime_peer_message_endpoint_labels_use_agent_identity_not_pane_titles() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(60, 24).unwrap(), 120)
        .unwrap();
    service
        .session
        .set_pane_title_explicit("%1", "  coordinator pane  ")
        .unwrap();

    assert_eq!(
        service.runtime_peer_message_endpoint_label("agent-%1"),
        "agent-%1"
    );
    service.set_subagent_lineage(
        "agent-%1",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "trusted worker".to_string(),
            terminal: false,
        },
    );
    service
        .session
        .set_pane_title_explicit("%1", "trusted worker title changed")
        .unwrap();
    assert_eq!(
        service.runtime_peer_message_endpoint_label("agent-%1"),
        "trusted worker"
    );
    service.set_subagent_lineage(
        "agent-%1",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "\n\0".to_string(),
            terminal: false,
        },
    );
    assert_eq!(
        service.runtime_peer_message_endpoint_label("agent-%1"),
        "agent-%1",
        "a name erased by sanitization must fall back to the canonical identity"
    );
    assert_eq!(
        service.runtime_peer_message_endpoint_label("agent-%9"),
        "agent-%9"
    );
    assert_eq!(
        service.runtime_peer_message_endpoint_label("external-agent"),
        "external-agent"
    );
}

/// Verifies delivered peer mail is logged prompt-style in the recipient pane
/// with the sender named at the destination end of the direction arrow.
///
/// An operator watching a pane must see interagent traffic the way user prompts
/// appear, including the same hanging-indent wrapping for a long payload, and the
/// echo must stay pure observation: the durable block keeps the peer trust domain,
/// no user instruction appears, no context block claims the echo, and a repeated
/// delivery pass neither re-echoes nor re-commits the message.
#[test]
fn runtime_peer_message_echo_logs_sender_prefix_without_user_trust_domain() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(24, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-%3", None, "agent", &[], now_ms)
        .unwrap();
    let peer_message = |id: &str, payload: &str| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: sender.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: payload.to_string(),
        extension_fields: Vec::new(),
    };
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            peer_message("peer-echo-1", "alpha beta gamma delta epsilon"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );

    let echoed = peer_echo_pane_lines(&service, "%1");
    assert!(
        echoed.iter().any(|line| line == "│ agent-%3> alpha beta"),
        "{echoed:#?}"
    );
    assert!(
        echoed.iter().any(|line| line == "│      gamma delta"),
        "{echoed:#?}"
    );
    assert!(
        echoed.iter().any(|line| line == "│      epsilon"),
        "{echoed:#?}"
    );
    assert_eq!(
        echoed.iter().filter(|line| line.contains("> ")).count(),
        1,
        "the sender label prints once instead of repeating on continuation rows: {echoed:#?}"
    );
    assert!(
        echoed.iter().all(|line| line.chars().count() <= 24),
        "wrapped peer rows must stay inside the pane width: {echoed:#?}"
    );

    // The idle delivery started a message-triggered turn, so this arrival takes
    // the active-turn append path and must echo there too.
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            peer_message("peer-echo-2", "cwd ok"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    let active_turn_echoed = peer_echo_pane_lines(&service, "%1");
    assert!(
        active_turn_echoed
            .iter()
            .any(|line| line == "│ agent-%3> cwd ok"),
        "{active_turn_echoed:#?}"
    );

    // Distinct accepted envelopes carrying identical text remain distinct
    // committed messages. Logging is keyed by the canonical delivery, never
    // by payload text.
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            peer_message("peer-echo-3", "cwd ok"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    let repeated_payload_echoed = peer_echo_pane_lines(&service, "%1");
    assert_eq!(
        repeated_payload_echoed
            .iter()
            .filter(|line| line == &"│ agent-%3> cwd ok")
            .count(),
        2,
        "identical payloads from separate committed envelopes both log: {repeated_payload_echoed:#?}"
    );

    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == recipient_identity.agent_id.as_str())
        .cloned()
        .expect("peer message turn");
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    let peer_blocks = context
        .blocks()
        .iter()
        .filter(|block| block.source == ContextSourceKind::PeerMessage)
        .collect::<Vec<_>>();
    assert_eq!(peer_blocks.len(), 3, "{peer_blocks:#?}");
    assert!(
        peer_blocks[0]
            .content
            .contains("alpha beta gamma delta epsilon")
    );
    assert!(peer_blocks[1].content.contains("cwd ok"));
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.source == ContextSourceKind::UserInstruction),
        "the echoed peer line must never create user-trust context"
    );
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.label.contains("agent-%3>")),
        "the pane echo is presentation-only and must not become provider context"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies accepted `send_message` actions create one sender-side row while
/// rejected and undeliverable sends remain absent from the sender transcript.
///
/// A sender row proves only message-service acceptance, never recipient
/// observation or processing, so failures must not leak their raw payload.
#[test]
fn runtime_send_message_echoes_at_sender_only_after_acceptance() {
    let (mut service, execution, _target) =
        execute_runtime_send_message_to("agent-%2", "text/plain", "ack, running now");
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let sent = peer_echo_pane_lines(&service, "%1");
    assert!(
        sent.iter()
            .any(|line| line.contains("agent-%2< ack, running now")),
        "{sent:#?}"
    );
    service.terminate_all_pane_processes().unwrap();

    let (mut rejected, execution, _target) =
        execute_runtime_send_message_to("parent", "text/plain", "handoff");
    assert_eq!(
        execution.action_results[0]
            .error
            .as_ref()
            .expect("recipient rejection")
            .code,
        "invalid_message_recipient"
    );
    let rejected_lines = peer_echo_pane_lines(&rejected, "%1");
    assert!(
        !rejected_lines.iter().any(|line| line.contains("handoff")),
        "{rejected_lines:#?}"
    );
    rejected.terminate_all_pane_processes().unwrap();

    let (mut undeliverable, execution, _target) =
        execute_runtime_send_message_to("agent:agent-nowhere", "text/plain", "handoff");
    assert_eq!(
        execution.action_results[0]
            .error
            .as_ref()
            .expect("unavailable recipient failure")
            .code,
        "message_recipient_unavailable"
    );
    let undeliverable_lines = peer_echo_pane_lines(&undeliverable, "%1");
    assert!(
        !undeliverable_lines
            .iter()
            .any(|line| line.contains("handoff")),
        "{undeliverable_lines:#?}"
    );
    undeliverable.terminate_all_pane_processes().unwrap();
}

/// Verifies pending peer mail committed into a user-started turn is logged
/// exactly once, without becoming user-trust context.
///
/// A user prompt commits the recipient's unread peer mail into the new turn
/// instead of starting a message-triggered turn, so it is a separate commit
/// site. A message committed there must be as operator-visible as one committed
/// at arrival time, while its durable block stays a peer reference event and the
/// logged line stays presentation-only.
#[test]
fn runtime_peer_message_echo_logs_one_line_for_user_prompt_turn_commit() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(60, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(60, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-%3", None, "agent", &[], now_ms)
        .unwrap();
    let delivery = service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "prompt-path-echo-1".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    recipient_identity.agent_id.clone(),
                ),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "pending peer evidence".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    let started = service
        .start_agent_prompt_turn("%1", "inspect the pending mail")
        .unwrap();
    let context = service.agent_turn_contexts().get(&started.turn_id).unwrap();
    let peer_blocks = context
        .blocks()
        .iter()
        .filter(|block| block.source == ContextSourceKind::PeerMessage)
        .collect::<Vec<_>>();
    assert_eq!(peer_blocks.len(), 1, "{peer_blocks:#?}");
    assert!(peer_blocks[0].content.contains("pending peer evidence"));
    assert!(
        !context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::UserInstruction
                && block.content.contains("pending peer evidence")
        }),
        "committed peer mail must never enter the turn as user input"
    );
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient_identity.agent_id)
            .unwrap()
            .last_sequence,
        delivery.sequence
    );

    let echoed = peer_echo_pane_lines(&service, "%1");
    assert_eq!(
        echoed
            .iter()
            .filter(|line| line.contains("agent-%3> "))
            .count(),
        1,
        "a message committed into a user-started turn logs exactly one line: {echoed:#?}"
    );
    assert!(
        echoed
            .iter()
            .any(|line| line == "│ agent-%3> pending peer evidence"),
        "{echoed:#?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies runtime-owned bridge traffic follows the same commit rule as
/// model-originated peer mail, so the pane log never depends on whether the
/// recipient happened to be busy.
///
/// An all-bridge batch starts no idle turn and commits nothing, so it logs
/// nothing. Commit membership is otherwise unchanged: a committed bridge
/// notification consumes no display row and no placeholder, even when its JSON
/// payload carries an `output` field, while the model-authored peer message it
/// was committed alongside still logs exactly once. `verbose` restores the
/// bridge echo and logs the full bounded payload for JSON traffic too.
#[test]
fn runtime_peer_message_echo_logs_committed_bridge_traffic_once() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(60, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(60, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let child = service
        .ensure_runtime_message_identity("agent-%3", None, "agent", &["agent-harness"], now_ms)
        .unwrap();
    let bridge = |id: &str, task_id: &str, summary: &str| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "task_status".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: Some(task_id.to_string()),
        ttl_ms: None,
        content_type: "application/json".to_string(),
        payload: mez_agent::messaging::TaskStatusPayload {
            task_id: task_id.to_string(),
            state: mez_agent::messaging::TaskState::Running,
            progress_percent: Some(0),
            summary: summary.to_string(),
        }
        .to_json(),
        extension_fields: crate::runtime::control::runtime_bridge_extension_fields(),
    };
    let model = |id: &str, payload: &str| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: payload.to_string(),
        extension_fields: Vec::new(),
    };
    let result = |id: &str, task_id: &str, output: &str| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "task_result".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: Some(task_id.to_string()),
        ttl_ms: None,
        content_type: "application/json".to_string(),
        payload: format!(
            r#"{{"task_id":"{task_id}","success":true,"summary":"bridge result","output":{output}}}"#
        ),
        extension_fields: crate::runtime::control::runtime_bridge_extension_fields(),
    };
    let accept = |service: &mut crate::runtime::RuntimeSessionService, envelope: Envelope| {
        let sender = envelope.sender.agent_id.clone();
        service
            .control
            .message_service_mut()
            .accept_at_with_scope(&sender, envelope, MessageScope::Session, now_ms)
            .unwrap();
    };
    let compact = |lines: Vec<String>| {
        lines
            .join("\n")
            .chars()
            .filter(|character| character.is_alphanumeric())
            .collect::<String>()
    };

    accept(
        &mut service,
        bridge("bridge-1", "bridge-turn-1", "bridge evidence one"),
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0,
        "an all-bridge batch starts no idle turn"
    );
    let idle = peer_echo_pane_lines(&service, "%1");
    assert!(
        !idle.iter().any(|line| line.contains("agent-%3>")),
        "nothing is committed, so nothing is logged: {idle:#?}"
    );

    accept(
        &mut service,
        bridge("bridge-2", "bridge-turn-2", "bridge evidence two"),
    );
    accept(&mut service, model("mixed-1", "mixed peer request"));
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        3,
        "the started turn commits the mixed batch and the earlier bridge message"
    );
    let committed = peer_echo_pane_lines(&service, "%1");
    assert_eq!(
        committed
            .iter()
            .filter(|line| line.contains("agent-%3> "))
            .count(),
        1,
        "only the canonical plaintext model message renders in normal mode: {committed:#?}"
    );
    let committed_text = compact(committed.clone());
    assert_eq!(
        committed_text.matches("mixedpeerrequest").count(),
        1,
        "the committed model message logs exactly once: {committed_text}"
    );
    for summary in ["bridgeevidenceone", "bridgeevidencetwo"] {
        assert_eq!(
            committed_text.matches(summary).count(),
            0,
            "normal presentation suppresses non-plaintext bridge payloads: {committed_text}"
        );
    }

    accept(
        &mut service,
        bridge("bridge-3", "bridge-turn-3", "bridge evidence three"),
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    let active = compact(peer_echo_pane_lines(&service, "%1"));
    assert_eq!(
        active.matches("mixedpeerrequest").count(),
        1,
        "an already-committed message is never echoed twice: {active}"
    );
    assert_eq!(
        active.matches("bridgeevidencethree").count(),
        0,
        "an active-turn committed task status remains presentation-silent: {active}"
    );

    // A task-result bridge payload remains presentation-silent because normal
    // mode admits only the canonical plaintext media type.
    accept(
        &mut service,
        result("bridge-4", "bridge-turn-4", "\"task complete\""),
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    let projected = peer_echo_pane_lines(&service, "%1");
    assert_eq!(
        projected
            .iter()
            .filter(|line| line.contains("agent-%3> "))
            .count(),
        1,
        "non-plaintext bridge status and result payloads remain presentation-silent: {projected:#?}"
    );
    let projected_text = compact(projected);
    assert_eq!(
        projected_text.matches("taskcomplete").count(),
        0,
        "normal mode no longer projects JSON result output: {projected_text}"
    );
    for omitted in ["bridgeresult", "success"] {
        assert_eq!(
            projected_text.matches(omitted).count(),
            0,
            "a suppressed bridge payload reaches no row, so {omitted} must not be logged: \
         {projected_text}"
        );
    }

    // Verbose mode logs the complete bounded raw bridge payload.
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "peer-message-log-mode-verbose".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\npeer_message_log_mode = \"verbose\"\n".to_string(),
        }])
        .unwrap();
    accept(
        &mut service,
        bridge("bridge-5", "bridge-turn-5", "verbose bridge evidence"),
    );
    accept(
        &mut service,
        result("bridge-6", "bridge-turn-6", "\"verbose task complete\""),
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        2,
        "verbose mode does not change the commit rule"
    );
    let verbose_text = compact(service.pane_screen("%1").unwrap().normal_content_lines());
    assert!(
        verbose_text.contains("summaryverbosebridgeevidence"),
        "verbose mode logs the bounded `task_status` payload: {verbose_text}"
    );
    assert!(
        verbose_text.contains("bridgeresult"),
        "verbose mode logs the whole bounded payload: \
     {verbose_text}"
    );
    assert!(
        verbose_text.contains("verbosetaskcomplete"),
        "{verbose_text}"
    );
    assert_eq!(
        verbose_text.matches("mixedpeerrequest").count(),
        1,
        "verbose mode never re-echoes a committed message: {verbose_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies model-authored peer mail logs at an accepted sender and committing
/// recipient in default normal mode, including a child with no subagent display
/// name.
///
/// Normal-mode presentation admits only the exact canonical
/// `text/plain; charset=utf-8` media type. Delegation lineage and the optional
/// `subagent_display_name` extension do not affect that decision, so canonical
/// model `send_message` traffic keeps its receiver-side `{name}> ` row.
#[test]
fn runtime_model_peer_mail_without_bridge_provenance_logs_at_sender_and_receiver() {
    // Parent -> child: acceptance creates one sender-side row.
    let (mut service, execution, _target) =
        execute_runtime_send_message_to("agent:agent-%2", "text/plain", "parent reply");
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let sent = peer_echo_pane_lines(&service, "%1");
    assert!(
        sent.iter()
            .any(|line| line.contains("agent-%2< parent reply")),
        "{sent:#?}"
    );
    service.terminate_all_pane_processes().unwrap();

    // Child -> parent: the committed inbound echo names the sender. The child
    // has no lineage and no display name, and one case carries a
    // `subagent_display_name` field on a canonical plaintext `send` envelope,
    // proving that metadata does not affect the media-type decision.
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(60, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(60, 24).unwrap(), 100).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let recipient_identity = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let child = service
        .ensure_runtime_message_identity("agent-%3", None, "agent", &["agent-harness"], now_ms)
        .unwrap();
    let model_mail = |id: &str, payload: &str, extension_fields: Vec<(String, String)>| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: payload.to_string(),
        extension_fields,
    };
    for (id, payload, extension_fields) in [
        ("model-mail-1", "child report", Vec::new()),
        (
            "model-mail-2",
            "named child report",
            vec![("subagent_display_name".to_string(), "\"kid\"".to_string())],
        ),
    ] {
        let envelope = model_mail(id, payload, extension_fields);
        let sender = envelope.sender.agent_id.clone();
        service
            .control
            .message_service_mut()
            .accept_at_with_scope(&sender, envelope, MessageScope::Session, now_ms)
            .unwrap();
    }
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        2,
        "model peer mail still starts the parent's message-triggered turn"
    );
    let received = peer_echo_pane_lines(&service, "%1");
    assert!(
        received
            .iter()
            .any(|line| line == "│ agent-%3> child report"),
        "a model-authored inbound message from a child with no display name keeps its \
     echo: {received:#?}"
    );
    assert!(
        received
            .iter()
            .any(|line| line == "│ agent-%3> named child report"),
        "a `subagent_display_name` extension on a `send` envelope never suppresses the \
     echo: {received:#?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies committed direct-parent messages retain the stable `parent` label
/// across a parent-pane rename while every non-direct sender falls back to its
/// identity-based label.
///
/// This drives the receiver commit path rather than only the echo helper. Two
/// parent envelopes commit into the child's active turn on opposite sides of a
/// parent title rename, proving presentation consults exact recipient lineage
/// instead of the mutable title. The predicate assertions separately preserve
/// exactness for sibling, unrelated, and grandparent identities, accept durable
/// restored lineage without treating it as live authority, and reject the same
/// edge once a parent conversation fence makes it stale.
#[test]
fn runtime_direct_parent_peer_message_uses_stable_label_only_for_valid_exact_lineage() {
    let mut service = test_runtime_service();
    service
        .attach_primary(
            "parent before rename",
            true,
            Size::new(60, 24).unwrap(),
            120,
        )
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .execute_terminal_command(
            &service.session.layout_owner_client_id().cloned().unwrap(),
            "split-window; rename-pane child pane",
        )
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%2")
        .unwrap();
    service.set_pane_screen(
        "%2".to_string(),
        TerminalScreen::new(Size::new(60, 24).unwrap(), 100).unwrap(),
    );
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 2,
            display_name: "child".to_string(),
            terminal: false,
        },
    );
    service.set_subagent_lineage(
        "agent-%1",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "parent".to_string(),
            terminal: false,
        },
    );
    service.set_subagent_lineage(
        "agent-%3",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 2,
            display_name: "sibling".to_string(),
            terminal: false,
        },
    );
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let parent = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    let child = service
        .ensure_runtime_message_identity(
            "agent-%2",
            PaneId::opaque("%2".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&child.agent_id)
        .unwrap();
    service
        .start_agent_prompt_turn("%2", "receive parent mail")
        .unwrap();
    let parent_message = |id: &str, payload: &str| Envelope {
        protocol: "mmp/1",
        id: id.to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: parent.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(child.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: payload.to_string(),
        extension_fields: Vec::new(),
    };
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &parent.agent_id,
            parent_message("direct-parent-before-rename", "first parent instruction"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    service
        .session
        .set_pane_title_explicit("%1", "parent after rename")
        .unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &parent.agent_id,
            parent_message("direct-parent-after-rename", "second parent instruction"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );

    let received = peer_echo_pane_lines(&service, "%2");
    assert!(
        received.iter().any(|line| line == "│ parent> first parent")
            && received.iter().any(|line| line == "│      instruction"),
        "the first committed parent message must use the stable label: {received:#?}"
    );
    assert!(
        received
            .iter()
            .any(|line| line == "│ parent> second parent")
            && received
                .iter()
                .filter(|line| line.as_str() == "│      instruction")
                .count()
                == 2,
        "the renamed parent must retain the stable label: {received:#?}"
    );
    assert!(
        !received
            .iter()
            .any(|line| line.contains("parent before rename")
                || line.contains("parent after rename")),
        "parent pane titles must never replace the stable direct-parent label: {received:#?}"
    );

    for sender in ["agent-%3", "external-agent", "agent-root"] {
        assert!(
            !service.runtime_peer_message_sender_is_direct_parent("agent-%2", sender),
            "only the exact immediate parent can use the parent label: {sender}"
        );
    }
    assert!(service.runtime_peer_message_sender_is_direct_parent("agent-%2", "agent-%1"));
    for (sender, id, payload, expected_label) in [
        (
            "agent-%3",
            "sibling-fallback",
            "sibling evidence",
            "sibling",
        ),
        (
            "external-agent",
            "unrelated-fallback",
            "unrelated evidence",
            "external-agent",
        ),
        (
            "agent-root",
            "grandparent-fallback",
            "grandparent evidence",
            "agent-root",
        ),
    ] {
        let sender_identity = service
            .ensure_runtime_message_identity(sender, None, "agent", &[], now_ms)
            .unwrap();
        let sender_agent_id = sender_identity.agent_id.clone();
        let envelope = Envelope {
            protocol: "mmp/1",
            id: id.to_string(),
            message_type: "send".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: sender_identity,
            recipient: mez_agent::messaging::Recipient::Agent(child.agent_id.clone()),
            correlation_id: None,
            ttl_ms: None,
            content_type: "text/plain; charset=utf-8".to_string(),
            payload: payload.to_string(),
            extension_fields: Vec::new(),
        };
        service
            .control
            .message_service_mut()
            .accept_at_with_scope(&sender_agent_id, envelope, MessageScope::Session, now_ms)
            .unwrap();
        assert_eq!(
            service
                .deliver_pending_runtime_agent_messages(now_ms)
                .unwrap(),
            1
        );
        assert!(
            peer_echo_pane_lines(&service, "%2")
                .iter()
                .any(|line| line.starts_with(&format!("│ {expected_label}>"))),
            "non-direct sender {sender} must keep its endpoint label"
        );
    }
    service.set_restored_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 2,
            display_name: "restored child".to_string(),
            terminal: false,
        },
    );
    assert!(
        service.runtime_peer_message_sender_is_direct_parent("agent-%2", "agent-%1"),
        "restored lineage remains valid for presentation even though it is not live authority"
    );
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &parent.agent_id,
            parent_message("restored-parent", "restored parent evidence"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert!(
        peer_echo_pane_lines(&service, "%2")
            .iter()
            .any(|line| line.starts_with("│ parent> restored parent")),
        "validated restored lineage must retain the parent presentation alias"
    );
    service.fence_subagent_descendants_for_parent_conversation("agent-%1", "replacement");
    assert!(
        !service.runtime_peer_message_sender_is_direct_parent("agent-%2", "agent-%1"),
        "a fenced historical edge must fall back to ordinary endpoint labeling"
    );
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &parent.agent_id,
            parent_message("fenced-parent", "fenced parent evidence"),
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert!(
        peer_echo_pane_lines(&service, "%2")
            .iter()
            .any(|line| line.starts_with("│ parent> fenced parent")),
        "fenced lineage must use the parent's trusted pretty name, not its renamed pane title"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies outbound presentation uses a sanitized spawn-owned name, keeps
/// selector expressions, and falls back to a concrete recipient's raw ID.
#[test]
fn runtime_outbound_recipient_display_label_preserves_spawn_name_and_fallback() {
    let mut service = test_runtime_service();
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "CypherSol".to_string(),
            terminal: false,
        },
    );
    let named_recipient =
        crate::runtime::Recipient::Agent(AgentId::opaque("agent-%2".to_string()).unwrap());
    assert_eq!(
        service.runtime_outbound_recipient_display_label(&named_recipient, "agent:%2"),
        "CypherSol"
    );

    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "agent-%2".to_string(),
            terminal: false,
        },
    );
    assert_eq!(
        service.runtime_outbound_recipient_display_label(&named_recipient, "agent:%2"),
        "agent-%2",
        "literal name mode must retain its assigned identity"
    );

    let unavailable_recipient =
        crate::runtime::Recipient::Agent(AgentId::opaque("agent-missing".to_string()).unwrap());
    assert_eq!(
        service.runtime_outbound_recipient_display_label(
            &unavailable_recipient,
            "agent:agent-missing",
        ),
        "agent-missing"
    );
}

/// Verifies runtime-owned subagent bridge notifications for an idle parent
/// start no turn and leave scheduler and provider-task accounting unchanged.
///
/// `task_status`/`task_result` notifications are authored by the runtime's own
/// subagent lifecycle, not by a model `send_message` action, so they must wait
/// behind the durable cursor for the parent's next turn instead of waking an
/// idle parent with a peer-message turn; a later model-originated peer message
/// still starts exactly one turn and injects both blocks.
#[test]
fn runtime_idle_agent_runtime_owned_bridge_notifications_start_no_turn() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let parent_identity = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&parent_identity.agent_id)
        .unwrap();
    let child_identity = service
        .ensure_runtime_message_identity("agent-%2", None, "agent", &["agent-harness"], now_ms)
        .unwrap();
    let status_envelope = Envelope {
        protocol: "mmp/1",
        id: "turn-child:task_status:started".to_string(),
        message_type: "task_status".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child_identity.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(parent_identity.agent_id.clone()),
        correlation_id: Some("turn-child".to_string()),
        ttl_ms: None,
        content_type: "application/json".to_string(),
        payload: mez_agent::messaging::TaskStatusPayload {
            task_id: "turn-child".to_string(),
            state: mez_agent::messaging::TaskState::Running,
            progress_percent: Some(0),
            summary: "subagent task started".to_string(),
        }
        .to_json(),
        extension_fields: Vec::new(),
    };
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &child_identity.agent_id,
            status_envelope,
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    assert!(
        service.agent_turn_ledger().turns().is_empty(),
        "runtime-owned bridge traffic must not start a turn for an idle parent"
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&parent_identity.agent_id)
            .unwrap()
            .last_sequence,
        0
    );

    let peer_envelope = Envelope {
        protocol: "mmp/1",
        id: "model-peer-message-1".to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: child_identity.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(parent_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "model-originated peer request".to_string(),
        extension_fields: Vec::new(),
    };
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &child_identity.agent_id,
            peer_envelope,
            MessageScope::Session,
            now_ms,
        )
        .unwrap();

    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        2,
        "the model-originated turn carries the pending runtime-owned notification"
    );
    let parent_turns: Vec<_> = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .filter(|turn| turn.agent_id == parent_identity.agent_id.as_str())
        .cloned()
        .collect();
    assert_eq!(
        parent_turns.len(),
        1,
        "a model-originated peer message starts exactly one turn"
    );
    let context = service
        .agent_turn_contexts()
        .get(&parent_turns[0].turn_id)
        .unwrap();
    assert!(
        context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::PeerMessage
                && block.label.contains("task_status")
                && block.content.contains("subagent task started")
        }),
        "pending runtime-owned notifications are still injected"
    );
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::PeerMessage
            && block.content.contains("model-originated peer request")
    }));
}
