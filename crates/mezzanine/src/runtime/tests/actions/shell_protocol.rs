//! Runtime tests for actions shell protocol behavior.

use super::*;

/// Verifies that runtime shell transaction markers are generated with fresh
/// entropy for every dispatch. Identical turn/action metadata must not produce
/// reusable marker tokens.
#[test]
fn runtime_marker_for_action_uses_fresh_entropy() {
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "turn-1".to_string(),
        conversation_id: "conversation-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 200,
        deadline_at_unix_millis: 0,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        initial_capability: None,
        state: mez_agent::AgentTurnState::Running,
    };

    let first = runtime_marker_for_action(&turn, "a1").unwrap();
    let second = runtime_marker_for_action(&turn, "a1").unwrap();

    assert_ne!(first.as_str(), second.as_str());
    assert!(first.as_str().len() >= 64);
    assert!(second.as_str().len() >= 64);
}

/// Verifies that runtime shell transaction observation remains bounded after
/// reserving Base64 expansion, line wrapping, and output-frame metadata.
///
/// Agent actions retain encoded PTY bytes rather than raw command-output bytes,
/// so truncation must begin at the expanded transport limit instead of the
/// smaller raw-output limit.
#[test]
fn runtime_shell_transaction_observation_is_bounded_and_truncated() {
    let mut service = test_runtime_service();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "printf marker\n".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    let raw_limit = 256 * 1024usize;
    let encoded_bytes = raw_limit.div_ceil(3).saturating_mul(4);
    let encoded_lines = encoded_bytes.div_ceil(76);
    let observation_limit = encoded_bytes
        .saturating_add(encoded_lines)
        .saturating_add(4 * 1024);
    let output = vec![b'x'; observation_limit + 4096];

    service.record_running_shell_transaction_output("%1", &output);

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert_eq!(transaction.observed_output_bytes, output.len() + 1);
    assert_eq!(transaction.observed_output_preview.len(), observation_limit);
    assert!(transaction.observed_output_truncated);
}

/// Verifies the sandbox transaction observation bound retains a complete
/// maximum-size encoded payload followed by trusted Bubblewrap status.
///
/// Non-stateful action output is base64-framed before the status descriptor is
/// emitted. Bounding the encoded PTY stream at the raw-output limit discards
/// the trailing status frame and falsely reports a completed sandbox action as
/// an invalid Bubblewrap transport.
#[test]
fn runtime_shell_transaction_observation_retains_trailing_bubblewrap_status() {
    let mut service = test_runtime_service();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "produce bounded output".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    service.register_sandboxed_shell_transaction_backend(
        "marker-1",
        crate::runtime::SandboxBackend::Bubblewrap,
    );
    let encoded_bytes = mez_agent::SHELL_OUTPUT_BASE64_MAX_RAW_BYTES
        .div_ceil(3)
        .saturating_mul(4);
    let encoded_payload = "e"
        .repeat(encoded_bytes)
        .as_bytes()
        .chunks(76)
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let output = format!(
        "{}\n{}\n{}\n{}\neyJjaGlsZC1waWQiOjQyfQp7ImV4aXQtY29kZSI6MH0K\n{}\n",
        mez_agent::SHELL_OUTPUT_BASE64_BEGIN_MARKER,
        encoded_payload,
        mez_agent::SHELL_OUTPUT_BASE64_END_MARKER,
        mez_agent::SHELL_STATUS_BASE64_BEGIN_MARKER,
        mez_agent::SHELL_STATUS_BASE64_END_MARKER,
    );
    assert!(output.len() > 256 * 1024);

    service.record_running_shell_transaction_output("%1", output.as_bytes());

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert!(!transaction.observed_output_truncated);
    assert_eq!(
        mez_agent::decode_shell_status_transport(&transaction.observed_output_preview).unwrap(),
        "{\"child-pid\":42}\n{\"exit-code\":0}\n"
    );
}

/// Verifies non-sandboxed agent actions retain the complete Base64-expanded
/// output frame rather than applying the raw-output byte limit to PTY bytes.
///
/// Encoded transport is used independently of Bubblewrap, so every agent
/// action must reserve expansion, wrapping, and marker bytes before deciding
/// that its observation was truncated.
#[test]
fn runtime_shell_transaction_observation_sizes_unsandboxed_encoded_output() {
    let mut service = test_runtime_service();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "produce bounded output".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    let encoded_bytes = mez_agent::SHELL_OUTPUT_BASE64_MAX_RAW_BYTES
        .div_ceil(3)
        .saturating_mul(4);
    let encoded_payload = "e"
        .repeat(encoded_bytes)
        .as_bytes()
        .chunks(76)
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let output = format!(
        "{}\n{}\n{}\n",
        mez_agent::SHELL_OUTPUT_BASE64_BEGIN_MARKER,
        encoded_payload,
        mez_agent::SHELL_OUTPUT_BASE64_END_MARKER,
    );

    service.record_running_shell_transaction_output("%1", output.as_bytes());

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert!(!transaction.observed_output_truncated);
    assert!(
        transaction
            .observed_output_preview
            .contains(mez_agent::SHELL_OUTPUT_BASE64_END_MARKER)
    );
}

/// Verifies retained transaction output reconstructs one UTF-8 scalar split
/// across arbitrary PTY reads instead of replacing each partial chunk.
#[test]
fn runtime_shell_transaction_observation_preserves_split_utf8() {
    let mut service = test_runtime_service();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "printf unicode".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    service.record_running_shell_transaction_output("%1", &[0xe2]);
    service.record_running_shell_transaction_output("%1", &[0x82, 0xac]);

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert_eq!(transaction.observed_output_preview, "€\n");
    assert_eq!(transaction.observed_output_bytes, 4);
    assert!(!transaction.observed_output_truncated);
}

/// Registers one raw-output transaction whose capture must begin at its OSC
/// start marker. Readiness probes preserve bytes exactly, which keeps boundary
/// assertions independent from agent-action output cleanup.
fn register_required_start_capture(service: &mut RuntimeSessionService) {
    service.register_running_shell_transaction(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::ReadinessProbe,
            pane_id: "%1".to_string(),
            command: "printf sentinel".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
        true,
    );
}

/// Verifies pane write-failure settlement releases the exact generation-fenced
/// input lease acquired when a shell transaction was registered.
///
/// Adapter arbitration blocks every ordinary keystroke while this lease is
/// retained, so removing only the transaction record would leave a responsive
/// parent shell appearing permanently frozen.
#[test]
fn runtime_shell_write_failure_releases_transaction_input_lease() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1";
    let mut process = service
        .take_running_pane_process_for_adapter(pane_id)
        .unwrap();
    let marker = "write-failure-lease-marker";
    service.register_running_shell_transaction(
        marker.to_string(),
        RunningShellTransactionRef {
            turn_id: "write-failure-lease-turn".to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: pane_id.to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: Some(1),
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
        true,
    );
    let acquired = service.drain_pane_io_transition().side_effects;
    assert!(acquired.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::AcquireShellInputLease { owner_id },
            ..
        } if owner_id == marker
    )));

    assert_eq!(
        service
            .fail_shell_transactions_for_pane_write_failure(pane_id, "injected write failure")
            .unwrap(),
        1
    );
    let settled = service.drain_pane_io_transition().side_effects;
    assert!(settled.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::ReleaseShellInputLease { owner_id },
            ..
        } if owner_id == marker
    )));
    assert!(service.running_shell_transactions_for_tests().is_empty());
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies timeout settlement releases the exact transaction input lease
/// before recovery writes an interrupt to the pane process.
///
/// This protects the timer-owned failure path, which historically removed the
/// transaction directly and stranded the actor's exclusive input owner.
#[test]
fn runtime_shell_timeout_releases_transaction_input_lease() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1";
    let mut process = service
        .take_running_pane_process_for_adapter(pane_id)
        .unwrap();
    let marker = "timeout-lease-marker";
    service.register_running_shell_transaction(
        marker.to_string(),
        RunningShellTransactionRef {
            turn_id: "timeout-lease-turn".to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: pane_id.to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: Some(1),
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
        true,
    );
    let _ = service.drain_pane_io_transition();

    assert_eq!(service.expire_timed_out_shell_transactions(1).unwrap(), 1);
    let settled = service.drain_pane_io_transition().side_effects;
    let release_index = settled
        .iter()
        .position(|effect| matches!(
            effect,
            RuntimeSideEffect::PaneProcessIo {
                effect: crate::runtime::PaneProcessIoEffect::ReleaseShellInputLease { owner_id },
                ..
            } if owner_id == marker
        ))
        .expect("timeout settlement must release the transaction input lease");
    let interrupt_index = settled
        .iter()
        .position(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::PaneProcessIo {
                    effect: crate::runtime::PaneProcessIoEffect::WriteInput { bytes },
                    ..
                } if bytes == b"\x03"
            )
        })
        .expect("bootstrap timeout should request a pane interrupt");
    assert!(release_index < interrupt_index, "{settled:?}");
    assert!(service.running_shell_transactions_for_tests().is_empty());
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies wrapper echo before a mandatory start marker is excluded while
/// a newline-free capability sentinel is retained exactly until its matching
/// end marker despite ordinary CRLF terminal traffic outside the transaction.
#[test]
fn runtime_shell_transaction_capture_starts_after_osc_boundary() {
    let mut service = test_runtime_service();
    register_required_start_capture(&mut service);

    service.record_running_shell_transaction_output(
        "%1",
        b"\x1b[?2004l\rMEZ_STTY_STATE=\r\n\x1b]133;C;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\mez-bubblewrap-capability-v1\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\\r\nprompt > ",
    );

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert_eq!(
        transaction.observed_output_preview,
        "mez-bubblewrap-capability-v1"
    );
    assert_eq!(
        transaction.observed_output_bytes,
        "mez-bubblewrap-capability-v1".len()
    );
}

/// Verifies a mandatory OSC start marker split across PTY reads is retained
/// only as framing state and does not leak pre-start shell bytes into output.
#[test]
fn runtime_shell_transaction_capture_preserves_split_start_boundary() {
    let mut service = test_runtime_service();
    register_required_start_capture(&mut service);

    service.record_running_shell_transaction_output(
        "%1",
        b"wrapper echo\r\n\x1b]133;C;mez_marker=marker-1;mez_turn=turn-1;",
    );
    service.record_running_shell_transaction_output(
        "%1",
        b"mez_agent=agent-%1;mez_pane=%1\x1b\\sentinel\n",
    );

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert_eq!(transaction.observed_output_preview, "sentinel\n");
    assert_eq!(transaction.observed_output_bytes, 9);
}

/// Verifies a matching OSC end marker split across PTY reads remains framing
/// state rather than contaminating the transaction body. Capability probes
/// compare a newline-free sentinel byte-for-byte, so even the first escape
/// fragment would otherwise reject a successful Bubblewrap process.
#[test]
fn runtime_shell_transaction_capture_preserves_split_end_boundary() {
    let mut service = test_runtime_service();
    register_required_start_capture(&mut service);

    service.record_running_shell_transaction_output(
        "%1",
        b"ignored\r\n\x1b]133;C;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\mez-bubblewrap-capability-v1\x1b]133;D;0;mez_",
    );
    service.record_running_shell_transaction_output(
        "%1",
        b"marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\prompt > ",
    );

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert_eq!(
        transaction.observed_output_preview,
        "mez-bubblewrap-capability-v1"
    );
    assert_eq!(
        transaction.observed_output_bytes,
        "mez-bubblewrap-capability-v1".len()
    );
}

/// Verifies a managed-Bash transaction permanently closes output capture at
/// its inner end marker even though settlement waits for receiver completion.
///
/// Bash emits receiver completion after the evaluated wrapper returns, so the
/// PTY may deliver that OSC in a later read. Those callback bytes are protocol
/// traffic, not capability-probe output, and must not contaminate the exact
/// newline-free sentinel retained from the already-closed transaction body.
#[test]
fn runtime_shell_transaction_capture_stays_closed_while_bash_completion_is_pending() {
    let mut service = test_runtime_service();
    register_required_start_capture(&mut service);
    service.register_shell_receiver_payload(
        "marker-1",
        mez_mux::process::ShellInputDelivery::receiver_acknowledged(
            b"managed Bash source\n".to_vec(),
            "marker-1",
            true,
        ),
    );

    service.record_running_shell_transaction_output(
        "%1",
        b"\x1b]133;C;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\mez-bubblewrap-capability-v1\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );
    service
        .observe_agent_shell_transaction_start("%1", "marker-1", "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", "marker-1", "turn-1", "agent-%1", "%1", 0)
        .unwrap();
    service.record_running_shell_transaction_output(
        "%1",
        b"\x1b]133;R;mez_receiver=complete;mez_token=receiver-token;mez_marker=marker-1;mez_status=0\x1b\\",
    );

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .expect("managed-Bash transaction should await receiver completion");
    assert_eq!(
        transaction.observed_output_preview,
        "mez-bubblewrap-capability-v1"
    );
    assert_eq!(
        transaction.observed_output_bytes,
        "mez-bubblewrap-capability-v1".len()
    );
}

/// Verifies private receiver acknowledgements emitted before the mandatory
/// transaction start boundary are consumed before transaction output is
/// sliced. Identity probes use the same record-separator byte in their framed
/// payload, so a stale acknowledgement count must not remove those fields.
#[test]
fn runtime_shell_transaction_capture_consumes_receiver_acks_before_start_boundary() {
    let mut service = test_runtime_service();
    register_required_start_capture(&mut service);
    service.set_shell_transaction_receiver_acknowledgements_for_tests("marker-1", 2);

    service.record_running_shell_transaction_output(
        "%1",
        b"\x1e\x1e\x1b]133;C;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\\x1emez_shell_identity_begin=marker-1\n\x1emez_shell_path=/bin/bash\n\x1emez_shell_identity_end=marker-1\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert_eq!(
        transaction.observed_output_preview,
        "\u{1e}mez_shell_identity_begin=marker-1\n\u{1e}mez_shell_path=/bin/bash\n\u{1e}mez_shell_identity_end=marker-1\n"
    );
}

/// Verifies output capture still fails closed semantically: bytes after the
/// trusted start boundary remain evidence, while prompt bytes after the
/// matching end marker are excluded.
#[test]
fn runtime_shell_transaction_capture_retains_contamination_before_end_only() {
    let mut service = test_runtime_service();
    register_required_start_capture(&mut service);

    service.record_running_shell_transaction_output(
        "%1",
        b"ignored\r\n\x1b]133;C;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\sentinel\npollution\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\prompt > ",
    );

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert_eq!(transaction.observed_output_preview, "sentinel\npollution\n");
    assert_eq!(transaction.observed_output_bytes, 19);
}

/// Registers one encoded agent-action owner for private rendering regressions.
fn register_encoded_output_render_owner(service: &mut RuntimeSessionService) {
    let marker = "encoded-render-marker";
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        marker.to_string(),
        RunningShellTransactionRef {
            turn_id: "encoded-render-turn".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "encoded-render-action".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "printf encoded-output".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    service.register_encoded_shell_output_transaction(marker);
}

/// Verifies ordinary parent-shell output resembling a private marker remains
/// literal when no encoded agent transaction owns the pane decoder.
#[test]
fn runtime_rendering_preserves_unowned_shell_output_transport_markers() {
    let mut service = test_runtime_service();
    let output = b"__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\nb2sK\n__MEZ_SHELL_OUTPUT_BASE64_END__\n";

    assert_eq!(service.renderable_pane_output_bytes("%1", output), output);
}

/// Verifies visible transaction rendering retains a Base64 frame split across
/// PTY reads and emits only its decoded payload after the matching end marker.
#[test]
fn runtime_rendering_preserves_split_shell_output_transport() {
    let mut service = test_runtime_service();
    register_encoded_output_render_owner(&mut service);
    let first = service.renderable_pane_output_bytes("%1", b"__MEZ_SHELL_OUTPUT_BASE64_BEG");
    let second = service
        .renderable_pane_output_bytes("%1", b"IN__\n4oKsCg==\n__MEZ_SHELL_OUTPUT_BASE64_END__\n");

    assert!(first.is_empty(), "split private marker leaked: {first:?}");
    assert_eq!(second, "€\n".as_bytes());
}

/// Verifies fragmented output larger than the former whole-frame retention
/// ceiling is decoded incrementally without exposing private Base64 records.
#[test]
fn runtime_rendering_streams_large_shell_output_transport_without_base64_leakage() {
    use base64::Engine as _;

    let mut service = test_runtime_service();
    register_encoded_output_render_owner(&mut service);
    let raw = vec![b'x'; 768 * 1024];
    let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);
    let payload = encoded
        .as_bytes()
        .chunks(76)
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let frame = format!(
        "{}\n{}\n{}\n",
        mez_agent::SHELL_OUTPUT_BASE64_BEGIN_MARKER,
        payload,
        mez_agent::SHELL_OUTPUT_BASE64_END_MARKER,
    );
    let mut rendered = Vec::new();
    for fragment in frame.as_bytes().chunks(4093) {
        rendered.extend(service.renderable_pane_output_bytes("%1", fragment));
    }

    assert_eq!(rendered, raw);
    assert!(!String::from_utf8_lossy(&rendered).contains("eHh4eHh4"));
}

/// Verifies one admitted malformed frame remains suppressed through its END
/// marker instead of exposing later encoded records as ordinary pane text.
#[test]
fn runtime_rendering_suppresses_malformed_shell_output_until_end_marker() {
    let mut service = test_runtime_service();
    register_encoded_output_render_owner(&mut service);
    let rendered = service.renderable_pane_output_bytes(
        "%1",
        b"__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\nb2sK\nnot-base64!\nc2VjcmV0Cg==\n__MEZ_SHELL_OUTPUT_BASE64_END__\ntail\n",
    );

    assert_eq!(rendered, b"ok\ntail\n");
}

/// Verifies async pane write completions are retained in the hidden trace log.
///
/// A shell transaction being recorded as running is not enough evidence that
/// the async pane worker actually wrote its wrapper bytes to the PTY. The trace
/// log should include write progress so file-action hangs can be diagnosed at
/// the delivery boundary instead of only at the transaction marker boundary.
#[test]
fn runtime_pane_input_written_traces_active_shell_transaction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "create-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "cat > note.txt".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    assert!(service.apply_pane_input_written_event("%1", 4096).unwrap());

    let trace = service.agent_pane_trace_log_text("%1").unwrap();
    assert!(trace.contains("pane input written bytes: 4096"), "{trace}");
    assert!(trace.contains("marker: marker-1"), "{trace}");
    assert!(trace.contains("action: create-1"), "{trace}");
}

/// Verifies positive pane-write progress refreshes a primary bootstrap's idle
/// deadline even when no foreign-shell boundary exists.
///
/// Darwin delivers generated shell source in paced records, so a primary
/// bootstrap must not expire against its registration time while its wrapper
/// is still being accepted by the pane worker.
#[test]
fn runtime_primary_bootstrap_input_progress_refreshes_timeout() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "bootstrap-marker".to_string(),
        RunningShellTransactionRef {
            turn_id: "bootstrap-turn".to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: "%1".to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: Some(10),
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    assert!(service.apply_pane_input_written_event("%1", 4096).unwrap());

    let refreshed_at_unix_ms = service
        .running_shell_transactions_for_tests()
        .get("bootstrap-marker")
        .unwrap()
        .started_at_unix_ms;
    assert!(refreshed_at_unix_ms > 0);
    let original_timer_key = crate::runtime::RuntimeTimerKey::new(
        crate::runtime::RuntimeTimerKind::Bootstrap,
        "bootstrap-marker",
        0,
    );
    assert!(
        service
            .shell_transaction_timer_transition(
                &std::collections::HashSet::from([original_timer_key]),
                10,
            )
            .side_effects
            .is_empty(),
        "an earlier bootstrap wakeup remains safe after progress moves the deadline"
    );
    assert_eq!(service.expire_timed_out_shell_transactions(10).unwrap(), 0);
    assert!(
        service
            .running_shell_transactions_for_tests()
            .contains_key("bootstrap-marker")
    );
    let rearmed = service.shell_transaction_timer_transition(&std::collections::HashSet::new(), 10);
    assert!(rearmed.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::ScheduleTimer { key, .. }
            if key.owner_id == "bootstrap-marker"
                && key.generation == refreshed_at_unix_ms
    )));
    assert_eq!(
        service
            .expire_timed_out_shell_transactions(refreshed_at_unix_ms.saturating_add(10))
            .unwrap(),
        1
    );
}

/// Verifies empty pane-write notifications cannot indefinitely extend a
/// primary bootstrap transaction that made no delivery progress.
#[test]
fn runtime_primary_bootstrap_empty_input_does_not_refresh_timeout() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "bootstrap-marker".to_string(),
        RunningShellTransactionRef {
            turn_id: "bootstrap-turn".to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: "%1".to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 7,
            timeout_ms: Some(10),
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    assert!(service.apply_pane_input_written_event("%1", 0).unwrap());

    assert_eq!(
        service
            .running_shell_transactions_for_tests()
            .get("bootstrap-marker")
            .unwrap()
            .started_at_unix_ms,
        7
    );
}

/// Verifies transaction retention removes a fragmented, marker-correlated Fish
/// payload receiver record without hiding child-owned OSC output.
///
/// Fish emits receiver readiness between the transaction start boundary and
/// child output. PTY reads may split that private control record arbitrarily;
/// retaining any fragment contaminates strict internal probe output, while
/// stripping unrelated OSC records would corrupt legitimate command output.
#[test]
fn runtime_shell_transaction_observation_excludes_fragmented_fish_control_osc() {
    let mut service = test_runtime_service();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::ReadinessProbe,
            pane_id: "%1".to_string(),
            command: String::new(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    service.record_running_shell_transaction_output(
        "%1",
        b"\x1b]133;R;mez_payload_receiver=ready;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_",
    );
    service.record_running_shell_transaction_output(
        "%1",
        b"pane=%1\x1b\\mez-bubblewrap-capability-v6\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
    );

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert_eq!(
        transaction.observed_output_preview,
        "mez-bubblewrap-capability-v6\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\"
    );
    assert_eq!(
        transaction.observed_output_bytes,
        transaction.observed_output_preview.len()
    );
    assert!(!transaction.observed_output_truncated);
}

/// Verifies model-visible shell transaction observation strips prompt styling
/// and Mezzanine wrapper echo while preserving command output.
///
/// Styled shell prompts can be much larger than the useful output for common
/// commands like `ls`. The agent context must contain the file names rather
/// than consuming its bounded observation budget with PS1 repaint bytes.
#[test]
fn runtime_shell_transaction_observation_strips_prompt_and_wrapper_noise() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let filtered = service.visible_pane_output_bytes(
        "%1",
        b"\x1b[38;2;214;93;14m\xee\x82\xb6\x1b[48;2;214;93;14m\xef\xb0\x95 neil \x1b[0m\r\n\x1b[38;2;214;93;14m\xee\x82\xb6\x1b[48;2;214;93;14m\xef\xb0\x95 neil \x1b[0m MEZ_MARKER_TOKEN='abc'\r\n\x1b[38;2;214;93;14m\xee\x82\xb6\x1b[48;2;214;93;14m\xef\xb0\x95 neil \x1b[0m MEZ_TURN='turn-1'\r\n\x1b[1;38;2;152;151;26m\xef\x90\xb2\x1b[0m ls\r\nCargo.toml\r\nsrc\r\n\x1b]133;D;0;mez_marker=abc;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );
    service.record_running_shell_transaction_output("%1", &filtered);

    let transaction = service
        .running_shell_transactions_for_tests()
        .get("marker-1")
        .unwrap();
    assert!(
        transaction.observed_output_preview.contains("src"),
        "{}",
        transaction.observed_output_preview
    );
    assert!(
        !transaction.observed_output_preview.contains("MEZ_"),
        "{}",
        transaction.observed_output_preview
    );
    assert!(
        !transaction.observed_output_preview.contains("neil"),
        "{}",
        transaction.observed_output_preview
    );
    assert!(transaction.observed_output_bytes > 0);
    assert!(!transaction.observed_output_truncated);
}

/// Verifies that transaction observation hides echoed Mezzanine-owned wrapper
/// lines for active shell transactions while preserving actual command output and the
/// OSC transaction markers that the runtime needs to observe completion.
#[test]
fn runtime_shell_transaction_wrapper_echo_is_hidden_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let visible = service.visible_pane_output_bytes(
        "%1",
        b"MEZ_RESTORE_ERREXIT=0; case $- in *e*) MEZ_RESTORE_ERREXIT=1; set +e;; esac; MEZ_HISTORY_RESTORE=0; case \"$(set -o 2>/dev/null | awk '$1==\"history\"{print $2; exit}')\" in on) MEZ_HISTORY_RESTORE=1; set +o history 2>/dev/null || :; history -d $((HISTCMD-1)) 2>/dev/null || :;; esac\r\nMEZ_HISTORY_HISTFILE_WAS_SET=0\r\nHISTFILE=/dev/null\r\nMEZ_MARKER_TOKEN='abc'\r\nMEZ_TURN='turn-1'\r\nls\r\nprintf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\'\r\n\"$MEZ_STATUS\" \"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"\r\nif [ \"$MEZ_HISTORY_HISTFILE_WAS_SET\" = 1 ]; then HISTFILE=$MEZ_HISTORY_HISTFILE_SAVED; else unset HISTFILE; fi\r\nMEZ_RESTORE_HISTORY_NOW=$MEZ_HISTORY_RESTORE\r\nunset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS\r\nif [ \"$MEZ_RESTORE_HISTORY_NOW\" = 1 ]; then set -o history 2>/dev/null || :; fi; if [ \"$MEZ_RESTORE_ERREXIT_NOW\" = 1 ]; then set -e; fi; unset MEZ_RESTORE_HISTORY_NOW MEZ_RESTORE_ERREXIT_NOW\r\n>\r\nfile-a\n\x1b]133;D;0;mez_marker=abc;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\",
    );
    let visible_text = String::from_utf8_lossy(&visible);

    assert!(!visible_text.contains("MEZ_MARKER_TOKEN"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_TURN"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_STATUS"), "{visible_text}");
    assert!(
        !visible_text.contains("MEZ_RESTORE_ERREXIT"),
        "{visible_text}"
    );
    assert!(!visible_text.contains("MEZ_HISTORY"), "{visible_text}");
    assert!(!visible_text.contains("HISTFILE"), "{visible_text}");
    assert!(!visible_text.contains("history -d"), "{visible_text}");
    assert!(!visible_text.contains("case $-"), "{visible_text}");
    assert!(!visible_text.contains("\nls"), "{visible_text}");
    assert!(visible_text.contains("file-a"), "{visible_text}");
    assert!(visible.contains(&0x1b));
}

/// Verifies that runtime transaction marker parsing is stateful per pane rather
/// than per PTY read chunk. Real PTY reads can split the OSC 133 transaction end
/// marker across chunks; losing that fragment leaves the agent shell action in a
/// permanent running state even though the command has already exited.
#[test]
fn runtime_shell_transaction_osc_parser_preserves_fragmented_markers() {
    let mut service = test_runtime_service();
    let size = Size::new(80, 24).unwrap();

    let (first_events, _, _) = service
        .terminal_osc_events_for_pane_bytes(
            "%1",
            size,
            b"file-a\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez",
        )
        .unwrap();
    let (second_events, _, _) = service
        .terminal_osc_events_for_pane_bytes("%1", size, b"_pane=%1\x1b\\")
        .unwrap();

    assert_eq!(first_events, Vec::<TerminalOscEvent>::new());
    assert_eq!(
        second_events,
        vec![TerminalOscEvent::ShellTransactionEnd {
            marker: "marker-1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            exit_code: 0,
        }]
    );
}

/// Verifies that terminal-wrapped fragments of Mezzanine wrapper echo are hidden
/// even when a PTY splits the original wrapper line before the filter receives a
/// newline. The visible pane must contain command output, not implementation
/// variable fragments.
#[test]
fn runtime_shell_transaction_wrapper_echo_fragments_are_hidden_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "printf 'file-a\\n'".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let visible = service.visible_pane_output_bytes(
        "%1",
        b"Z_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"\r\nEZ_PANE MEZ_STATUS\r\nfile-a\n",
    );
    let visible_text = String::from_utf8_lossy(&visible);

    assert!(!visible_text.contains("Z_TURN"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_AGENT"), "{visible_text}");
    assert!(!visible_text.contains("MEZ_STATUS"), "{visible_text}");
    assert!(visible_text.contains("file-a"), "{visible_text}");
}

/// Verifies deferred command payload records are never registered as shell
/// echo candidates after the transaction receiver has disabled terminal echo.
/// Real command output that happens to equal payload text must remain visible.
#[test]
fn runtime_shell_transaction_deferred_payload_text_remains_visible() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.register_running_shell_transaction(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "payload-record-that-is-real-output".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: Some(
                mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                    b"encoded-payload\n".to_vec(),
                    "marker-1",
                    true,
                ),
            ),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
        true,
    );

    let visible = service.visible_pane_output_bytes(
        "%1",
        b"MEZ_MARKER_TOKEN='abc'\r\npayload-record-that-is-real-output\r\nfile-a\n",
    );
    let visible_text = String::from_utf8_lossy(&visible);

    assert!(!visible_text.contains("MEZ_MARKER_TOKEN"), "{visible_text}");
    assert!(
        visible_text.contains("payload-record-that-is-real-output"),
        "{visible_text}"
    );
    assert!(visible_text.contains("file-a"), "{visible_text}");
}

/// Verifies newline-free output that resembles a wrapper prefix cannot grow
/// retained filter state without bound. Once the conservative prefix ceiling
/// is exceeded, the runtime must fail open and preserve the original bytes.
#[test]
fn runtime_shell_transaction_wrapper_prefix_retention_is_bounded() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.register_running_shell_transaction(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
        true,
    );
    let mut output = b"printf".to_vec();
    output.extend(std::iter::repeat_n(b'x', 16 * 1024));

    let visible = service.visible_pane_output_bytes("%1", &output);

    assert_eq!(visible, output);
}

/// Verifies that `/log-level trace` is the high-verbosity escape hatch for raw
/// shell-wrapper diagnosis. When enabled, the runtime leaves echoed wrapper
/// traffic untouched so developers can inspect exactly what was written to and
/// echoed by the pane PTY.
#[test]
fn runtime_shell_transaction_wrapper_echo_is_visible_with_trace_enabled() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Trace)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let visible =
        service.visible_pane_output_bytes("%1", b"MEZ_MARKER_TOKEN='abc'\r\nls\r\nfile-a\n");
    let visible_text = String::from_utf8_lossy(&visible);

    assert!(visible_text.contains("MEZ_MARKER_TOKEN"), "{visible_text}");
    assert!(visible_text.contains("ls"), "{visible_text}");
    assert!(visible_text.contains("file-a"), "{visible_text}");
}

/// Verifies that agent command output retained for transaction observation is
/// not rendered into the user pane by default. This keeps default agent turns
/// conversational while still preserving the bytes needed for command-result
/// context.
#[test]
fn runtime_agent_shell_transaction_output_is_hidden_from_pane_by_default() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let rendered = service.renderable_pane_output_bytes("%1", b"file-a\n");

    assert!(rendered.is_empty());
}

/// Verifies that `/log-level verbose` opts the pane back into agent command
/// output without enabling raw wrapper traffic. Verbose remains the shell-view
/// level for commands and their output; trace remains reserved for wrapper
/// internals and full diagnostic payloads.
#[test]
fn runtime_agent_shell_transaction_output_is_visible_with_verbose_enabled() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let rendered = service.renderable_pane_output_bytes("%1", b"file-a\n");

    assert_eq!(rendered, b"file-a\n");
}

/// Verifies exiting agent mode after interrupting a live shell transaction
/// closes the nested agent subshell with a line command.
///
/// Immediate EOF can be consumed by an interrupted transaction wrapper's read
/// loop, leaving the user inside the child shell after agent mode hides. After
/// a live transaction is interrupted, the runtime should queue Ctrl+C followed
/// by `exit` so the command is read by the shell after the wrapper unwinds.
#[test]
fn runtime_agent_shell_exit_after_shell_transaction_uses_command_exit() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    service.enter_agent_subshell(pane_id.clone());
    let exit_marker = b"\x1b]133;mez_agent_subshell_exit=test-boundary\x1b\\".to_vec();
    service.remember_agent_subshell_exit_marker(&pane_id, exit_marker.clone());
    let started = service
        .start_agent_prompt_turn(&pane_id, "search the file")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-grep".to_string(),
        RunningShellTransactionRef {
            turn_id: started.turn_id.clone(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-grep".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "grep -n needle file.txt".to_string(),
            started_at_unix_ms: 1_000,
            timeout_ms: Some(10 * 60 * 1000),
            pending_input_payload: Some(
                mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                    b"payload\n".to_vec(),
                    "marker-grep",
                    true,
                ),
            ),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit-live-shell","input":"/exit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    let exit_inputs = service.drain_pane_io_transition().side_effects;
    assert_eq!(exit_inputs.len(), 3);
    assert!(matches!(
        &exit_inputs[0],
        RuntimeSideEffect::PaneProcessIo {
            instance,
            effect: crate::runtime::PaneProcessIoEffect::CancelShellInput { delivery_id },
        } if instance.pane_id == pane_id && delivery_id == "marker-grep"
    ));
    assert_eq!(exit_inputs[1].pane_input_parts().0, pane_id);
    assert_eq!(exit_inputs[1].pane_input_parts().1, b"\x03");
    assert_eq!(exit_inputs[2].pane_input_parts().0, pane_id);
    assert_eq!(exit_inputs[2].pane_input_parts().1, b"exit\n");
    assert!(!service.agent_subshell_is_active(&pane_id));
    assert!(!service.agent_subshell_command_exit_is_pending_for_tests(&pane_id));
    let mut exit_output = b"exit\r\n".to_vec();
    exit_output.extend_from_slice(&exit_marker);
    service
        .apply_pane_output_bytes(pane_id.clone(), exit_output)
        .unwrap();
    let pane_text = service
        .pane_screen(&pane_id)
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    assert!(
        !pane_text.contains("exit"),
        "the agent-owned child-shell exit echo must not enter pane history: {pane_text}"
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies list items keep their marker and first content words on the same
/// rendered row instead of flushing a marker-only line before the paragraph
/// text arrives. CommonMark emits `Paragraph` inside list items, so the
/// renderer must not treat the freshly written list prefix as a completed
/// block.
#[test]
fn runtime_agent_markdown_lists_keep_content_on_marker_row() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(64, 20).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(64, 20).unwrap(), 120).unwrap(),
    );
    let markdown = "1. first numbered item\n2. second numbered item\n\n- bullet item";

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            markdown,
            mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let pane_text = pane_lines.join("\n");

    assert!(
        pane_text.contains("▐ mez> 1. first numbered item"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("▐      2. second numbered item"),
        "{pane_text}"
    );
    assert!(pane_text.contains("▐      • bullet item"), "{pane_text}");
    assert!(
        !pane_lines.iter().any(|line| line.trim_end() == "▐ mez> 1."
            || line.trim_end() == "▐      2."
            || line.trim_end() == "▐      •"),
        "{pane_text}"
    );
}

/// Verifies that a managed Bash parent preserves an unsubmitted Readline draft
/// while entering and leaving an agent child shell.
///
/// The private receiver must clear the editor only while it consumes the
/// authenticated handoff, then restore the exact draft when the child exits.
/// The draft must execute only after the user explicitly submits it to the
/// restored parent prompt.
#[test]
fn runtime_bash_dirty_prompt_survives_agent_subshell_admission() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping dirty Bash prompt regression because bash is unavailable");
        return;
    };
    let root = temp_root("bash-dirty-agent-admission");
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(bash_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        root.join("default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    configure_pane_shell_protocol_fixture(&mut service);
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .write_input_to_pane(&primary, Some("%1"), b"printf '__MEZ_DRAFT_SURVIVED__\\n'")
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    let mut child_confirmed = false;
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.agent_subshell_is_active("%1")
            && !service.pane_bootstrap_is_pending_for_tests("%1")
        {
            child_confirmed = true;
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        child_confirmed,
        "dirty Bash admission did not confirm a child shell; authority={:?}",
        service.pane_environment_authority("%1")
    );

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.pane_foreground_certified_shell_state("%1") == Some(true) {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert_eq!(
        service.pane_foreground_certified_shell_state("%1"),
        Some(true)
    );
    assert!(!service.agent_subshell_is_active("%1"));
    service
        .write_input_to_pane(&primary, Some("%1"), b"\n")
        .unwrap();

    let mut draft_executed = false;
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("__MEZ_DRAFT_SURVIVED__")
        {
            draft_executed = true;
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(draft_executed, "dirty parent draft was not preserved");
    assert!(service.poll_pane_processes().unwrap().is_empty());
    assert!(service.pane_processes().contains_pane("%1"));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a managed Fish parent discards an unsubmitted command-line
/// draft while the private receiver admits and later exits an agent child.
///
/// The receiver must consume its transport outside Fish's editable buffer,
/// leave the original draft unexecuted, and return an empty responsive editor.
#[test]
fn runtime_fish_dirty_prompt_is_discarded_during_agent_subshell_admission() {
    let Some(fish_path) = [
        "/usr/bin/fish",
        "/usr/local/bin/fish",
        "/opt/homebrew/bin/fish",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file()) else {
        eprintln!("skipping dirty Fish prompt regression because fish is unavailable");
        return;
    };
    let root = temp_root("fish-dirty-agent-admission");
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(fish_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        root.join("default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    configure_pane_shell_protocol_fixture(&mut service);
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    settle_initial_managed_fish_bootstrap(&mut service, "%1");
    service
        .write_input_to_pane(
            &primary,
            Some("%1"),
            b"fish_vi_key_bindings; printf '__MEZ_FISH_VI_READY__\\n'\n",
        )
        .unwrap();
    wait_for_managed_fish_command_prompt(&mut service, "%1", "__MEZ_FISH_VI_READY__");
    let discarded_path = root.join("discarded-fish-draft");
    let draft = format!(
        "command touch {}",
        mez_agent::fish_quote(&discarded_path.to_string_lossy())
    );
    let mut draft_input = b"\x1bi\x1b[200~".to_vec();
    draft_input.extend_from_slice(draft.as_bytes());
    draft_input.extend_from_slice(b"\x1b[201~");
    service
        .write_input_to_pane(&primary, Some("%1"), &draft_input)
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    let mut child_confirmed = false;
    let child_confirmation_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < child_confirmation_deadline {
        let _ = service.poll_pane_outputs(8192).unwrap();
        for effect in service.drain_pane_io_transition().side_effects {
            match effect {
                RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect:
                        crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                            observation_id,
                            ..
                        },
                } => {
                    let process_group_id =
                        service.pane_processes().foreground_process_group_id("%1");
                    let process_name = service.pane_processes().foreground_process_name("%1");
                    service
                        .apply_pane_foreground_process_observation_transition(
                            instance,
                            crate::runtime::PaneForegroundProcessObservation {
                                observation_id,
                                process_name,
                                process_group_id,
                                current_working_directory: None,
                                error: None,
                            },
                        )
                        .unwrap();
                }
                RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
                } => {
                    service
                        .pane_processes_mut()
                        .write_pane_shell_delivery(&instance.pane_id, &delivery)
                        .unwrap();
                }
                RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect:
                        crate::runtime::PaneProcessIoEffect::WriteInput { bytes }
                        | crate::runtime::PaneProcessIoEffect::WriteInputPriority { bytes },
                } => service
                    .pane_processes_mut()
                    .write_pane_input(&instance.pane_id, &bytes)
                    .unwrap(),
                _ => {}
            }
        }
        if service.agent_subshell_is_active("%1")
            && !service.pane_bootstrap_is_pending_for_tests("%1")
        {
            child_confirmed = true;
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        child_confirmed,
        "dirty Fish admission did not confirm a child shell; authority={:?}; readiness={:?}; transactions={:?}",
        service.pane_environment_authority("%1"),
        service.pane_readiness_state("%1"),
        service.running_shell_transactions_for_tests()
    );

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    let parent_confirmation_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < parent_confirmation_deadline {
        // Read one byte at a time so the generic child-exit marker is applied
        // before the later authenticated Fish parent-restored event. This
        // deterministically exercises foreground input arriving while the
        // private callback is still unwinding.
        let updates = service.poll_pane_outputs(1).unwrap();
        if service.agent_subshell_exit_marker_for_tests("%1").is_none() {
            break;
        }
        if updates.is_empty() {
            wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
        }
    }
    assert!(
        service.agent_subshell_exit_marker_for_tests("%1").is_none(),
        "Fish child-exit rendering boundary did not settle before fresh input"
    );
    assert!(
        service.managed_shell_parent_restoration_is_pending_for_tests("%1"),
        "Fish parent return must remain owned after the earlier child-exit marker"
    );
    assert!(!service.agent_subshell_is_active("%1"));
    assert!(
        matches!(
            service.pane_readiness_state("%1"),
            PaneReadinessState::Unknown | PaneReadinessState::PromptCandidate
        ),
        "returned Fish parent should remain user-owned while agent mode is hidden"
    );
    service
        .write_input_to_pane(
            &primary,
            Some("%1"),
            b"printf '__MEZ_FISH_PARENT_RESPONSIVE__\\n'\n",
        )
        .unwrap();
    assert!(
        service.managed_shell_parent_restoration_is_pending_for_tests("%1"),
        "foreground input must not release Fish parent-return ownership"
    );

    let mut parent_responsive = false;
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("__MEZ_FISH_PARENT_RESPONSIVE__")
        {
            parent_responsive = true;
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        parent_responsive,
        "Fish parent did not accept fresh input after return; screen={}",
        service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\\n")
    );
    assert!(
        !discarded_path.exists(),
        "discarded Fish draft executed after agent-shell return"
    );
    assert!(
        !service.managed_shell_parent_restoration_is_pending_for_tests("%1"),
        "authenticated parent restoration should release queued foreground input"
    );
    assert!(service.poll_pane_processes().unwrap().is_empty());
    assert!(service.pane_processes().contains_pane("%1"));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies hiding agent mode before Fish installs its child receiver cancels
/// admission, keeps the unsubmitted draft discarded, and returns responsively.
///
/// Runtime must retain the synchronous Fish callback even though no agent
/// child is active yet, send the authenticated cancellation record, and wait
/// for parent readiness before allowing fresh input to be submitted.
#[test]
fn runtime_fish_dirty_prompt_exit_before_receiver_installation_discards_draft() {
    let Some(fish_path) = [
        "/usr/bin/fish",
        "/usr/local/bin/fish",
        "/opt/homebrew/bin/fish",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file()) else {
        eprintln!("skipping early-exit Fish regression because fish is unavailable");
        return;
    };
    let root = temp_root("fish-dirty-agent-early-exit");
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(fish_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        root.join("default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    configure_pane_shell_protocol_fixture(&mut service);
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    settle_initial_managed_fish_bootstrap(&mut service, "%1");
    service
        .write_input_to_pane(
            &primary,
            Some("%1"),
            b"fish_vi_key_bindings; printf '__MEZ_FISH_EARLY_EXIT_READY__\\n'\n",
        )
        .unwrap();
    wait_for_managed_fish_command_prompt(&mut service, "%1", "__MEZ_FISH_EARLY_EXIT_READY__");

    let discarded_path = root.join("discarded-fish-early-exit-draft");
    let draft = format!(
        "command touch {}",
        mez_agent::fish_quote(&discarded_path.to_string_lossy())
    );
    let mut draft_input = b"\x1bi\x1b[200~".to_vec();
    draft_input.extend_from_slice(draft.as_bytes());
    draft_input.extend_from_slice(b"\x1b[201~");
    service
        .write_input_to_pane(&primary, Some("%1"), &draft_input)
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    assert!(
        service.managed_shell_parent_restoration_is_pending_for_tests("%1"),
        "Fish must own parent return as soon as admission is triggered"
    );
    assert!(
        !service.agent_subshell_is_active("%1"),
        "the regression must exit before receiver installation"
    );

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    assert!(service.managed_shell_parent_restoration_is_pending_for_tests("%1"));
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if !service.managed_shell_parent_restoration_is_pending_for_tests("%1") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        !service.managed_shell_parent_restoration_is_pending_for_tests("%1"),
        "authenticated cancellation did not return the Fish parent"
    );
    assert!(!service.agent_subshell_is_active("%1"));
    assert!(!service.pane_bootstrap_is_pending_for_tests("%1"));

    service
        .write_input_to_pane(
            &primary,
            Some("%1"),
            b"printf '__MEZ_FISH_EARLY_PARENT_RESPONSIVE__\\n'\n",
        )
        .unwrap();
    let mut parent_responsive = false;
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("__MEZ_FISH_EARLY_PARENT_RESPONSIVE__")
        {
            parent_responsive = true;
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        parent_responsive,
        "early-exit cancellation did not return a responsive Fish editor; screen={}",
        service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\\n")
    );
    assert!(
        !discarded_path.exists(),
        "discarded Fish draft executed after early-exit cancellation"
    );
    assert!(service.poll_pane_processes().unwrap().is_empty());
    assert!(service.pane_processes().contains_pane("%1"));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a lost Fish parent-restored event requires fresh parent foreground
/// proof before runtime releases queued input after the restoration deadline.
///
/// A timer alone cannot distinguish the parent editor from a blocked receiver
/// or child. Recovery must retain the exact bytes until the owning pane-process
/// generation proves the original parent process group is foreground again.
#[test]
fn runtime_fish_parent_restoration_timeout_requires_foreground_proof() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1";
    let mut process = service
        .take_running_pane_process_for_adapter(pane_id)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "fish-restoration-marker".to_string(),
        RunningShellTransactionRef {
            turn_id: "bootstrap-fish-restoration".to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: pane_id.to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: Some(mez_mux::process::ShellInputDelivery::generated_source(
                Vec::new(),
            )),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );
    service.prepend_fish_shell_receiver_payloads(
        "fish-restoration-marker",
        mez_mux::process::ShellInputDelivery::generated_source(Vec::new()),
        mez_mux::process::ShellInputDelivery::generated_source(Vec::new()),
        mez_mux::process::ShellInputDelivery::generated_source(Vec::new()),
        mez_mux::process::ShellInputDelivery::generated_source(Vec::new()),
    );
    service.bind_agent_subshell_bootstrap_marker(pane_id, "fish-restoration-marker");
    assert!(service.mark_managed_shell_payload_released(pane_id, "fish-restoration-marker"));
    assert_eq!(
        service.mark_managed_shell_child_installed(pane_id, "fish-restoration-marker"),
        Some(false)
    );
    assert_eq!(
        service.mark_managed_fish_child_prompt_ready(pane_id, "fish-restoration-marker"),
        Some(false)
    );
    service.remove_running_shell_transaction("fish-restoration-marker");
    service.clear_shell_transaction_protocol_state("fish-restoration-marker");
    assert!(service.request_managed_shell_handoff_exit(pane_id).unwrap());
    let exit_effects = service.drain_pane_io_transition().side_effects;
    assert_eq!(pane_input_effects(&exit_effects).len(), 1);
    assert_eq!(
        pane_input_effects(&exit_effects)[0].pane_input_parts().1,
        mez_agent::fish_agent_subshell_exit_input()
    );
    service
        .write_input_to_pane(&primary, Some(pane_id), b"queued-after-fish-exit\n")
        .unwrap();
    assert!(service.managed_shell_parent_restoration_is_pending_for_tests(pane_id));
    assert!(pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty());

    assert_eq!(
        service
            .recover_expired_managed_shell_parent_restorations_for_tests(u64::MAX)
            .unwrap(),
        1
    );
    assert!(service.managed_shell_parent_restoration_is_pending_for_tests(pane_id));
    assert_eq!(
        service.pane_readiness_state(pane_id),
        PaneReadinessState::Degraded
    );
    let effects = service.drain_pane_io_transition().side_effects;
    assert!(pane_input_effects(&effects).is_empty());
    let (instance, observation_id, parent_process_group) = effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect:
                    crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id,
                        expected_process_group_id: Some(parent_process_group),
                    },
            } => Some((instance, observation_id, parent_process_group)),
            _ => None,
        })
        .expect("restoration timeout should request exact parent foreground proof");
    service
        .apply_pane_foreground_process_observation_transition(
            instance,
            crate::runtime::PaneForegroundProcessObservation {
                observation_id,
                process_name: Some("fish".to_string()),
                process_group_id: Some(parent_process_group),
                current_working_directory: None,
                error: None,
            },
        )
        .unwrap();
    assert!(!service.managed_shell_parent_restoration_is_pending_for_tests(pane_id));
    let released = service.drain_pane_io_transition().side_effects;
    let inputs = pane_input_effects(&released);
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].pane_input_parts().1, b"queued-after-fish-exit\n");
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies managed zsh startup admission fails closed after its bounded
/// deadline without creating a bootstrap transaction or writing pane input.
///
/// A missing startup availability event must not leave agent mode waiting
/// indefinitely, and timeout recovery must leave the ordinary parent process
/// available rather than attempting an unauthenticated private handoff.
#[test]
fn runtime_managed_zsh_admission_timeout_creates_no_shell_work() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1";
    let mut process = service
        .take_running_pane_process_for_adapter(pane_id)
        .unwrap();
    service.set_expired_managed_zsh_admission_for_tests(pane_id);
    let _ = service.drain_pane_io_transition();

    assert_eq!(
        service
            .recover_expired_managed_zsh_admissions_for_tests(u64::MAX)
            .unwrap(),
        1
    );
    assert!(
        service.managed_zsh_admission_unavailable_for_tests(pane_id, "startup-admission-timeout")
    );
    assert!(service.running_shell_transactions_for_tests().is_empty());
    assert!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
        "admission timeout must not write unauthenticated shell input"
    );
    assert!(service.primary_pid_for_live_pane_process(pane_id).is_some());
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies managed Bash protocol admission fails closed after its bounded
/// deadline without acquiring editor ownership or writing a handoff.
///
/// A missing or incompatible availability announcement must leave the parent
/// process responsive and create no bootstrap transaction or pane input.
#[test]
fn runtime_managed_bash_admission_timeout_creates_no_shell_work() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1";
    let mut process = service
        .take_running_pane_process_for_adapter(pane_id)
        .unwrap();
    service.set_expired_managed_bash_admission_for_tests(pane_id);
    let _ = service.drain_pane_io_transition();

    assert_eq!(
        service
            .recover_expired_managed_bash_admissions_for_tests(u64::MAX)
            .unwrap(),
        1
    );
    assert!(
        service.managed_bash_admission_unavailable_for_tests(pane_id, "startup-admission-timeout")
    );
    assert!(service.running_shell_transactions_for_tests().is_empty());
    assert!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
        "admission timeout must not write unauthenticated shell input"
    );
    assert!(service.primary_pid_for_live_pane_process(pane_id).is_some());
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies that a live POSIX shell discards an unsubmitted process draft
/// before agent-shell admission instead of concatenating generated transport
/// with the user's command.
///
/// POSIX shells do not provide a portable editor-state API, so the runtime
/// sends an interrupt and waits for a fresh prompt boundary before entering the
/// child shell. The interrupted draft must never execute, while the parent
/// process remains available after agent mode exits.
#[test]
fn runtime_posix_dirty_prompt_is_interrupted_before_agent_admission() {
    let shell_path = PathBuf::from("/bin/sh");
    if !shell_path.is_file() {
        eprintln!("skipping dirty POSIX prompt regression because /bin/sh is unavailable");
        return;
    }
    let root = temp_root("posix-dirty-agent-admission");
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(shell_path, ShellSource::FallbackBinSh),
            Size::new(80, 24).unwrap(),
        ),
        root.join("default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    configure_pane_shell_protocol_fixture(&mut service);
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service.set_pane_readiness("%1", PaneReadinessState::PromptCandidate);
    assert_eq!(service.maybe_bootstrap_ready_panes().unwrap(), 1);
    let initial_bootstrap_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < initial_bootstrap_deadline {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if !service.pane_bootstrap_is_pending_for_tests("%1") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        !service.pane_bootstrap_is_pending_for_tests("%1"),
        "initial POSIX bootstrap did not settle before dirty admission"
    );
    let draft_side_effect = root.join("interrupted-draft-ran");
    let draft = format!("printf ran > '{}'", draft_side_effect.display());
    service
        .write_input_to_pane(&primary, Some("%1"), draft.as_bytes())
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    assert!(
        !service.agent_subshell_is_active("%1"),
        "child ownership must wait for the parent prompt after the interrupt"
    );

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    assert!(!service.agent_subshell_is_active("%1"));

    let responsive_side_effect = root.join("post-interrupt-parent-responsive");
    service
        .write_input_to_pane(
            &primary,
            Some("%1"),
            format!("printf ready > '{}'\n", responsive_side_effect.display()).as_bytes(),
        )
        .unwrap();
    let responsive_deadline = Instant::now() + Duration::from_secs(15);
    while !responsive_side_effect.is_file() && Instant::now() < responsive_deadline {
        let _ = service.poll_pane_outputs(8192).unwrap();
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        responsive_side_effect.is_file(),
        "the interrupted POSIX parent did not accept fresh input"
    );
    assert!(
        !draft_side_effect.exists(),
        "the dirty POSIX draft executed after cancelled agent-shell admission"
    );
    assert!(service.poll_pane_processes().unwrap().is_empty());
    assert!(service.pane_processes().contains_pane("%1"));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a bash-backed pane shell survives the first agent shell
/// transaction after the command is displayed. The user-visible failure mode
/// was the primary pane exiting immediately after an agent command preview, so
/// this test waits through transaction settlement and repeated process polls.
#[test]
fn runtime_bash_agent_shell_transaction_keeps_parent_shell_alive() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping bash parent-shell regression because bash is unavailable");
        return;
    };
    let root = temp_root("bash-parent-shell-survival");
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(bash_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        root.join("default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    configure_pane_shell_protocol_fixture(&mut service);
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-bash-survival","input":"run a bash command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "exercise bash shell survival".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run a failing bash command and keep the parent shell available"
                            .to_string(),
                        command: "printf 'agent-bash-command-ran\\n'; false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);

    for _ in 0..900 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions_for_tests().is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        service.running_shell_transactions_for_tests().is_empty(),
        "agent transaction should have completed before checking parent shell liveness: transactions={:?} pane={}",
        service.running_shell_transactions_for_tests(),
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    );
    let pane_exits = service.poll_pane_processes().unwrap();
    assert!(pane_exits.is_empty(), "{pane_exits:?}");
    assert!(service.pane_processes().contains_pane("%1"));
    for _ in 0..10 {
        let exits = service.poll_pane_processes().unwrap();
        assert!(exits.is_empty(), "{exits:?}");
        assert!(service.pane_processes().contains_pane("%1"));
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_HISTORY_"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the bash-backed pane shell also survives an agent shell
/// transaction when strict interactive options are already enabled. Some users
/// set `errexit` and `nounset` in shell startup files, so the transaction
/// prologue must temporarily disable and later restore both without letting a
/// failed agent command close the pane or the enclosing Mez session.
#[test]
fn runtime_bash_agent_shell_transaction_preserves_strict_parent_shell_options() {
    let Some(bash_path) = bash_path_for_tests() else {
        eprintln!("skipping bash strict-option regression because bash is unavailable");
        return;
    };
    let root = temp_root("bash-strict-parent-shell-survival");
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(bash_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        root.join("default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    configure_pane_shell_protocol_fixture(&mut service);
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .write_input_to_pane(
            &primary,
            Some("%1"),
            b"set -eu; printf '__MEZ_BASH_STRICT_READY__\\n'\n",
        )
        .unwrap();
    let mut strict_parent_ready = false;
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(4096).unwrap();
        if service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("__MEZ_BASH_STRICT_READY__")
        {
            strict_parent_ready = true;
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        strict_parent_ready,
        "managed Bash parent did not confirm strict-option readiness"
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-bash-strict-survival","input":"run a bash command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "exercise bash strict shell survival".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run a failing bash command and keep strict shell options intact"
                            .to_string(),
                        command: "printf 'agent-bash-strict-command-ran\\n'; false".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);

    for _ in 0..900 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions_for_tests().is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    let protocol_diagnostics = service
        .running_shell_transactions_for_tests()
        .keys()
        .map(|marker| {
            (
                marker.clone(),
                service.shell_transaction_protocol_diagnostic_for_tests(marker),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        service.running_shell_transactions_for_tests().is_empty(),
        "managed Bash strict-option transaction did not settle: transactions={:?} protocol={protocol_diagnostics:?} readiness={:?} pane={}",
        service.running_shell_transactions_for_tests(),
        service.pane_readiness_state("%1"),
        service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\\n"),
    );
    let pane_exits = service.poll_pane_processes().unwrap();
    assert!(pane_exits.is_empty(), "{pane_exits:?}");
    assert!(service.pane_processes().contains_pane("%1"));
    if !service.pending_agent_provider_tasks().is_empty() {
        let completion_provider = RuntimeBatchProvider {
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "done".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(runtime_complete_batch("turn-1")),
                provider_transcript_events: Vec::new(),
            },
        };
        let completions = service
            .poll_agent_provider_tasks_with_provider(&completion_provider, 1)
            .unwrap();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].terminal_state, AgentTurnState::Completed);
    }

    service.agent_shell_store_mut().request_exit("%1").unwrap();
    service
        .write_input_to_pane(&primary, Some("%1"), b"case $- in *e*u*|*u*e*) printf 'STRICT_OPTIONS_STILL_SET\\n';; *) printf 'STRICT_OPTIONS_LOST:%s\\n' \"$-\";; esac\n")
        .unwrap();
    let mut pane_text = String::new();
    for _ in 0..150 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        pane_text = service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        if pane_text.contains("STRICT_OPTIONS_STILL_SET") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        pane_text.contains("STRICT_OPTIONS_STILL_SET"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies mismatched shell-transaction markers fail the live action promptly.
///
/// A terminal OSC marker can be malformed, delayed, or spoofed. The runtime must
/// validate marker metadata against the retained transaction state and fail the
/// action instead of leaving the turn to wait for a later timeout.
#[test]
fn runtime_shell_transaction_metadata_mismatch_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-marker-mismatch","input":"run a command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "run a shell command".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run a command".to_string(),
                        command: "true".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-1" => {
                Some(marker.clone())
            }
            _ => None,
        })
        .unwrap();

    let failed_owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-1".to_string(),
        action_id: "shell-1".to_string(),
        marker: marker.clone(),
    };
    let unrelated_owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-unrelated".to_string(),
        action_id: "shell-unrelated".to_string(),
        marker: "marker-unrelated".to_string(),
    };
    service
        .update_agent_shell_output_preview(
            "%1",
            failed_owner,
            1,
            &["failed owner output".to_string()],
        )
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            unrelated_owner.clone(),
            1,
            &["unrelated owner output".to_string()],
        )
        .unwrap();

    let observed = service
        .observe_agent_shell_transaction_end("%2", &marker, "turn-1", "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&marker)
    );
    assert!(!service.shell_transaction_requires_start_marker_for_tests(&marker));
    assert!(!service.shell_transaction_started_for_tests(&marker));
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text
            .contains("shell transaction marker metadata does not match runtime dispatch state"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("failed owner output"), "{pane_text}");
    assert!(pane_text.contains("unrelated owner output"), "{pane_text}");
    let previews = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(previews.len(), 1, "{previews:?}");
    assert_eq!(previews[0].0, unrelated_owner);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a duplicate start marker fails the live shell action.
///
/// The wrapper start marker is the handoff boundary for deferred command
/// payloads. Seeing it twice for one marker means the in-band control stream is
/// no longer well framed, so the action should fail instead of waiting for a
/// later timeout.
#[test]
fn runtime_shell_transaction_duplicate_start_marker_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (pane_id, marker) =
        dispatch_protocol_test_shell_action(&mut service, &primary, "shell-duplicate-start");

    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();
    assert!(service.shell_transaction_started_for_tests(&marker));
    let observed = service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&marker)
    );
    assert!(!service.shell_transaction_requires_start_marker_for_tests(&marker));
    assert!(!service.shell_transaction_started_for_tests(&marker));
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("shell transaction emitted a duplicate start marker"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a pane write failure after receiver entry cancels delivery before
/// interrupting the shell transaction.
///
/// Once the start marker has been observed, Fish may be blocked reading a
/// deferred payload. Recovery must discard the queued tail, send priority
/// Ctrl-C, and only then settle the transaction so later pane input cannot be
/// consumed as stale receiver data.
#[test]
fn runtime_shell_transaction_write_failure_interrupts_started_receiver() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (pane_id, marker) =
        dispatch_protocol_test_shell_action(&mut service, &primary, "shell-started-write-fail");
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();

    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();
    let _ = service.drain_pane_io_transition();
    assert!(service.shell_transaction_started_for_tests(&marker));

    service
        .apply_pane_write_failure_event(&pane_id, "synthetic deferred payload failure")
        .unwrap();

    let recovery = service.drain_pane_io_transition().side_effects;
    assert_eq!(recovery.len(), 2, "{recovery:?}");
    assert!(matches!(
        &recovery[0],
        RuntimeSideEffect::PaneProcessIo {
            instance,
            effect: crate::runtime::PaneProcessIoEffect::CancelShellInput { delivery_id },
        } if instance.pane_id == pane_id && delivery_id == &marker
    ));
    assert_eq!(recovery[1].pane_input_parts().0, pane_id);
    assert_eq!(recovery[1].pane_input_parts().1, b"\x03");
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&marker)
    );

    process.terminate(Duration::from_millis(10)).unwrap();
}

/// Verifies a write failure before receiver entry does not interrupt an idle
/// pane shell.
///
/// A transaction that has not emitted its start marker cannot be blocked in
/// the deferred receiver. Its delivery is still cancelled and settled, but an
/// unsolicited Ctrl-C would damage unrelated user input and must not be sent.
#[test]
fn runtime_shell_transaction_write_failure_does_not_interrupt_before_start() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (pane_id, marker) =
        dispatch_protocol_test_shell_action(&mut service, &primary, "shell-prestart-write-fail");
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    assert!(!service.shell_transaction_started_for_tests(&marker));

    service
        .apply_pane_write_failure_event(&pane_id, "synthetic wrapper failure")
        .unwrap();

    let recovery = service.drain_pane_io_transition().side_effects;
    assert_eq!(recovery.len(), 1, "{recovery:?}");
    assert!(matches!(
        &recovery[0],
        RuntimeSideEffect::PaneProcessIo {
            instance,
            effect: crate::runtime::PaneProcessIoEffect::CancelShellInput { delivery_id },
        } if instance.pane_id == pane_id && delivery_id == &marker
    ));
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&marker)
    );

    process.terminate(Duration::from_millis(10)).unwrap();
}

/// Verifies an end marker before the start marker fails the live shell action.
///
/// Runtime-dispatched wrappers must emit a start marker before any end marker.
/// An end marker first means the parser missed a control boundary or command
/// output spoofed the frame, either of which should fail fast with diagnostics.
#[test]
fn runtime_shell_transaction_end_before_start_marker_fails_live_action() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (pane_id, marker) =
        dispatch_protocol_test_shell_action(&mut service, &primary, "shell-end-before-start");

    let observed = service
        .observe_agent_shell_transaction_end(&pane_id, &marker, "turn-1", "agent-%1", &pane_id, 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&marker)
    );
    assert!(!service.shell_transaction_requires_start_marker_for_tests(&marker));
    assert!(!service.shell_transaction_started_for_tests(&marker));
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("shell transaction end marker arrived before the start marker"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an adapter-owned agent bootstrap defers payload until fresh start proof.
///
/// The initial handoff must contain only the persistent-shell launch and wrapper.
/// After the start marker, the runtime must request metadata from the exact PTY
/// worker generation and keep the payload registered until the matching result
/// arrives. A stale observation must neither record evidence nor release the
/// isolated transaction child that would replace the foreground process group.
#[test]
fn runtime_agent_subshell_bootstrap_waits_for_start_before_releasing_payload() {
    let mut service = test_runtime_service();
    service.enable_legacy_managed_startup_for_tests();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = "%1".to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();

    assert!(service.enter_agent_subshell_if_needed(&pane_id).unwrap());
    let handoff = service.drain_pane_io_transition().side_effects;
    assert_eq!(handoff.len(), 2);
    assert!(matches!(
        handoff.first(),
        Some(RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::AcquireShellInputLease { .. },
            ..
        })
    ));
    let (marker, transaction) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find(|(_, transaction)| transaction.kind == RunningShellTransactionKind::Bootstrap)
        .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
        .unwrap();
    assert!(
        transaction.pending_input_payload.is_some(),
        "bootstrap payload must remain deferred until the start marker is observed"
    );

    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &marker,
            &transaction.turn_id,
            "agent-%1",
            &pane_id,
        )
        .unwrap();

    assert!(
        service
            .running_shell_transactions_for_tests()
            .get(&marker)
            .unwrap()
            .pending_input_payload
            .is_some(),
        "payload must remain deferred while fresh start observation is pending"
    );
    let observation_effect = service
        .drain_pane_io_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect:
                    crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id,
                        expected_process_group_id,
                    },
            } => Some((instance, observation_id, expected_process_group_id)),
            _ => None,
        })
        .expect("start boundary should request fresh worker metadata");
    assert_eq!(observation_effect.2, None);

    let stale = service
        .apply_pane_foreground_process_observation_transition(
            observation_effect.0.clone(),
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: "stale-start-observation".to_string(),
                process_name: Some("bash".to_string()),
                process_group_id: Some(41),
                current_working_directory: Some("/tmp".to_string()),
                error: None,
            },
        )
        .unwrap();
    assert!(!stale.applied);
    assert!(
        service
            .running_shell_transactions_for_tests()
            .get(&marker)
            .unwrap()
            .pending_input_payload
            .is_some(),
        "stale observation must not release the payload"
    );
    assert!(service.drain_pane_io_transition().side_effects.is_empty());

    let observed = service
        .apply_pane_foreground_process_observation_transition(
            observation_effect.0,
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: observation_effect.1,
                process_name: Some("bash".to_string()),
                process_group_id: Some(41),
                current_working_directory: Some("/tmp".to_string()),
                error: None,
            },
        )
        .unwrap();
    assert!(observed.applied);
    assert!(
        service
            .running_shell_transactions_for_tests()
            .get(&marker)
            .unwrap()
            .pending_input_payload
            .is_none(),
        "matching fresh observation should release the payload"
    );
    let payload = service.drain_pane_io_transition().side_effects;
    assert_eq!(payload.len(), 1);
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies shell transaction payload bytes are deferred until the wrapper
/// receiver emits its start marker.
///
/// Large generated file-action scripts must not be sent as part of the initial
/// shell wrapper. Waiting for the start marker proves the shell has reached the
/// read loop that treats following bytes as payload data instead of shell
/// source.
#[test]
fn runtime_shell_transaction_start_streams_deferred_payload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-stream-payload","input":"run command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-stream".to_string(),
                    rationale: "run payload command".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run payload command".to_string(),
                        command: "printf '%s\\n' payload-marker".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let deferred_wrapper = service.drain_pane_io_transition().side_effects;
    assert_eq!(deferred_wrapper.len(), 2);
    let RuntimeSideEffect::PaneProcessIo {
        effect: crate::runtime::PaneProcessIoEffect::AcquireShellInputLease { owner_id },
        ..
    } = &deferred_wrapper[0]
    else {
        panic!("expected shell input lease acquisition: {deferred_wrapper:?}");
    };
    let RuntimeSideEffect::PaneProcessIo {
        effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
        ..
    } = &deferred_wrapper[1]
    else {
        panic!("expected typed generated wrapper delivery: {deferred_wrapper:?}");
    };
    assert_eq!(
        delivery.pacing,
        mez_mux::process::ShellInputPacing::GeneratedSource
    );
    assert!(!delivery.priority);
    assert_eq!(delivery.delivery_id.as_deref(), Some(owner_id.as_str()));
    let wrapper_text = String::from_utf8_lossy(deferred_wrapper[1].pane_input_parts().1);
    let wrapper_source = decoded_posix_shell_wrapper_sources(&wrapper_text);
    assert!(wrapper_source.contains("__mez_tx_"), "{wrapper_source}");
    assert!(!wrapper_text.contains("payload-marker"), "{wrapper_text}");
    let (marker, transaction) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find(|(_, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "shell-stream"
            )
        })
        .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
        .unwrap();
    assert!(transaction.pending_input_payload.is_some());

    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, "turn-1", "agent-%1", &pane_id)
        .unwrap();

    let deferred_payload = service.drain_pane_io_transition().side_effects;
    assert_eq!(deferred_payload.len(), 1);
    let RuntimeSideEffect::PaneProcessIo {
        effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
        ..
    } = &deferred_payload[0]
    else {
        panic!("expected typed deferred payload delivery: {deferred_payload:?}");
    };
    assert_eq!(
        delivery.pacing,
        mez_mux::process::ShellInputPacing::ReceiverAcknowledged
    );
    assert!(delivery.priority);
    assert_eq!(delivery.delivery_id.as_deref(), Some(marker.as_str()));
    assert_eq!(
        delivery.receiver_acknowledgements,
        cfg!(target_os = "macos")
    );
    let payload_text = String::from_utf8_lossy(deferred_payload[0].pane_input_parts().1);
    let encoded = payload_text
        .lines()
        .take_while(|line| !line.starts_with("__MEZ_COMMAND_PAYLOAD_END_"))
        .map(|line| {
            line.strip_prefix("C ")
                .expect("ordinary shell commands should use typed command records")
        })
        .collect::<String>();
    let decoded = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert!(decoded.contains("payload-marker"), "{decoded}");
    assert!(
        service
            .running_shell_transactions_for_tests()
            .get(&marker)
            .unwrap()
            .pending_input_payload
            .is_none()
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies a Fish-classified deferred payload remains withheld after the
/// transaction boundary and is sent exactly once after its correlated
/// receiver-ready event. This prevents payload records from reaching Fish's
/// interactive reader while its wrapper is still entering `read`.
#[test]
fn runtime_fish_transaction_waits_for_payload_receiver_ready() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = "%1".to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service.register_running_shell_transaction(
        "fish-marker".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "fish-action".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "printf fish".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: Some(60_000),
            pending_input_payload: Some(
                mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                    b"C ZmlzaA==\n__MEZ_COMMAND_PAYLOAD_END_fish-marker__\n".to_vec(),
                    "fish-marker",
                    true,
                ),
            ),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
        true,
    );
    service.require_shell_transaction_payload_receiver_ready("fish-marker");
    let _ = service.drain_pane_io_transition();

    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            "fish-marker",
            "turn-1",
            "agent-%1",
            &pane_id,
        )
        .unwrap();
    assert!(service.drain_pane_io_transition().side_effects.is_empty());
    assert!(
        service
            .running_shell_transactions_for_tests()
            .get("fish-marker")
            .unwrap()
            .pending_input_payload
            .is_some()
    );

    service
        .observe_shell_transaction_payload_receiver_ready(
            &pane_id,
            "fish-marker",
            "turn-1",
            "agent-%1",
            &pane_id,
        )
        .unwrap();
    let payload = service.drain_pane_io_transition().side_effects;
    assert!(matches!(
        payload.as_slice(),
        [RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
            ..
        }] if delivery.delivery_id.as_deref() == Some("fish-marker")
    ));
    assert!(
        service
            .running_shell_transactions_for_tests()
            .get("fish-marker")
            .unwrap()
            .pending_input_payload
            .is_none()
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies Fish bootstrap registration requires receiver readiness before
/// releasing its deferred payload. Bootstrap uses the same Fish transport as
/// ordinary actions, so omitting this gate turns the valid readiness event
/// into a protocol violation and prevents environment certification.
#[test]
fn runtime_fish_bootstrap_waits_for_payload_receiver_ready() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = "%1".to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    let environment = mez_agent::EnvironmentSignature::new(
        "linux",
        "x86_64",
        None,
        "test-host",
        "test-user",
        None,
        "/usr/bin/fish",
        mez_agent::ShellClassification::Fish,
        None,
        Some("/usr/bin:/bin".to_string()),
        "/tmp",
        None,
        false,
        None,
        Vec::new(),
    )
    .unwrap();
    service.set_pane_environment_signature_for_tests(&pane_id, environment);

    let (marker, _wrapper) = service
        .prepare_bootstrap_to_pane(&pane_id)
        .unwrap()
        .expect("Fish bootstrap should register");
    let turn_id = service
        .running_shell_transactions_for_tests()
        .get(&marker)
        .unwrap()
        .turn_id
        .clone();
    assert!(
        service
            .running_shell_transactions_for_tests()
            .get(&marker)
            .unwrap()
            .pending_input_payload
            .is_some()
    );
    let _ = service.drain_pane_io_transition();

    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, &turn_id, "agent-%1", &pane_id)
        .unwrap();
    assert!(service.drain_pane_io_transition().side_effects.is_empty());
    assert!(
        service
            .running_shell_transactions_for_tests()
            .get(&marker)
            .unwrap()
            .pending_input_payload
            .is_some()
    );

    assert_eq!(
        service
            .observe_shell_transaction_payload_receiver_ready(
                &pane_id, &marker, &turn_id, "agent-%1", &pane_id,
            )
            .unwrap(),
        1
    );
    let payload = service.drain_pane_io_transition().side_effects;
    assert!(matches!(
        payload.as_slice(),
        [RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
            ..
        }] if delivery.delivery_id.as_deref() == Some(marker.as_str())
    ));
    assert!(
        service
            .running_shell_transactions_for_tests()
            .get(&marker)
            .unwrap()
            .pending_input_payload
            .is_none()
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies prompt readiness cannot consume deferred bootstrap wrappers owned
/// by managed Fish and Zsh receiver installation.
///
/// Both child shells can publish prompt-like output before the authenticated
/// receiver-installed event is processed. Generic prompt bootstrap dispatch
/// must leave the wrapper untouched for that event; otherwise Fish reports a
/// protocol violation and Zsh can remain indefinitely in bootstrapping.
///
/// Resolves one test shell from `PATH` before checking supported absolute
/// installation locations. Ordinary validation can therefore exercise every
/// available shell without requiring one platform-specific package layout.
fn find_test_shell(shell_name: &str, known_locations: &[&str]) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(shell_name))
        .chain(known_locations.iter().map(|path| PathBuf::from(*path)))
        .find(|candidate| candidate.is_file())
}

/// Waits for authenticated managed-Fish startup to release and settle the
/// initial prompt-ready bootstrap transaction.
///
/// Prompt readiness can precede receiver installation, especially on Darwin.
/// The readiness pass must therefore leave bootstrap pending until Fish's
/// authenticated availability event is observed from pane output.
fn settle_initial_managed_fish_bootstrap(service: &mut RuntimeSessionService, pane_id: &str) {
    service.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);
    assert_eq!(
        service.maybe_bootstrap_ready_panes().unwrap(),
        0,
        "managed Fish bootstrap must wait for authenticated adapter availability"
    );
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if !service.pane_bootstrap_is_pending_for_tests(pane_id) {
            return;
        }
        wait_for_pane_process_activity(service, pane_id, Duration::from_millis(10));
    }
    panic!(
        "initial managed Fish bootstrap did not settle after adapter availability; screen={}",
        service
            .process_pane_screen(pane_id)
            .map(|screen| screen.normal_content_lines().join("\\n"))
            .unwrap_or_else(|| "<missing>".to_string())
    );
}

/// Waits for one managed-Fish setup command to finish at an authenticated,
/// editable prompt before a test installs an unsubmitted command-line draft.
///
/// Visible command output can arrive before Fish publishes its prompt-end
/// lifecycle event. Requiring settled readiness, environment authority, and
/// shell transactions prevents the following draft from racing that event or
/// being consumed by an identity probe intended for the private receiver.
fn wait_for_managed_fish_command_prompt(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    output_marker: &str,
) {
    let mut observed_command_start = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let read_limit = if observed_command_start { 8192 } else { 1 };
        let _ = service.poll_pane_outputs(read_limit).unwrap();
        observed_command_start |= service.pane_readiness_state(pane_id) == PaneReadinessState::Busy;
        let screen = service
            .process_pane_screen(pane_id)
            .map(|screen| screen.normal_content_lines().join("\n"))
            .unwrap_or_default();
        let prompt_ready = matches!(
            service.pane_readiness_state(pane_id),
            PaneReadinessState::PromptCandidate | PaneReadinessState::Ready
        );
        let authority_settled = !matches!(
            service.pane_environment_authority(pane_id),
            crate::runtime::processes::RuntimePaneEnvironmentAuthority::Pending
        );
        if observed_command_start
            && screen.contains(output_marker)
            && prompt_ready
            && authority_settled
            && service.running_shell_transactions_for_tests().is_empty()
        {
            return;
        }
        wait_for_pane_process_activity(service, pane_id, Duration::from_millis(10));
    }
    panic!(
        "managed Fish setup command did not settle at an editable prompt; marker={output_marker:?}; observed_command_start={observed_command_start}; authority={:?}; readiness={:?}; transactions={:?}; screen={}",
        service.pane_environment_authority(pane_id),
        service.pane_readiness_state(pane_id),
        service.running_shell_transactions_for_tests(),
        service
            .process_pane_screen(pane_id)
            .map(|screen| screen.normal_content_lines().join("\\n"))
            .unwrap_or_else(|| "<missing>".to_string())
    );
}

/// Verifies managed Fish bootstrap delivery requires authenticated receiver
/// availability from the current parent process and remains idempotent.
///
/// Prompt readiness and direct dispatch attempts must create no transaction
/// before availability. A stale token cannot release bootstrap, one valid
/// event releases exactly one owner, duplicate availability cannot dispatch a
/// second owner, and pane teardown clears the process-scoped readiness state.
#[test]
fn runtime_managed_fish_bootstrap_requires_authenticated_adapter_availability() {
    let Some(fish_path) = find_test_shell(
        "fish",
        &[
            "/usr/bin/fish",
            "/usr/local/bin/fish",
            "/opt/homebrew/bin/fish",
        ],
    ) else {
        eprintln!("skipping managed Fish availability regression because fish is unavailable");
        return;
    };
    let mut service = test_runtime_service();
    service.enable_legacy_managed_startup_for_tests();
    service.session.shell = ResolvedShell::new(fish_path, ShellSource::ShellEnv).into();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let pane_id = "%1";
    let token = service
        .fish_receiver_token_for_pane(pane_id)
        .cloned()
        .expect("managed Fish startup should install an authentication token");
    service.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);

    service.dispatch_bootstrap_to_pane(pane_id).unwrap();
    assert_eq!(service.maybe_bootstrap_ready_panes().unwrap(), 0);
    assert!(service.running_shell_transactions_for_tests().is_empty());
    assert!(service.pane_bootstrap_is_pending_for_tests(pane_id));
    assert!(!service.managed_fish_adapter_is_ready_for_tests(pane_id));

    let availability = mez_terminal::ManagedShellProtocolEvent::AdapterAvailable { trigger: None };
    assert_eq!(
        service
            .observe_managed_shell_protocol_event(
                pane_id,
                mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                mez_terminal::ManagedShellAdapter::Fish,
                "00000000000000000000000000000000",
                &availability,
            )
            .unwrap(),
        0
    );
    assert!(service.running_shell_transactions_for_tests().is_empty());
    assert!(!service.managed_fish_adapter_is_ready_for_tests(pane_id));

    assert_eq!(
        service
            .observe_managed_shell_protocol_event(
                pane_id,
                mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                mez_terminal::ManagedShellAdapter::Fish,
                token.as_str(),
                &availability,
            )
            .unwrap(),
        1
    );
    assert!(!service.managed_fish_adapter_is_ready_for_tests(pane_id));
    assert!(service.running_shell_transactions_for_tests().is_empty());
    settle_initial_managed_fish_bootstrap(&mut service, pane_id);
    assert!(service.managed_fish_adapter_is_ready_for_tests(pane_id));
    assert!(!service.pane_bootstrap_is_pending_for_tests(pane_id));
    assert!(service.running_shell_transactions_for_tests().is_empty());

    assert_eq!(
        service
            .observe_managed_shell_protocol_event(
                pane_id,
                mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                mez_terminal::ManagedShellAdapter::Fish,
                token.as_str(),
                &availability,
            )
            .unwrap(),
        1
    );
    assert!(service.running_shell_transactions_for_tests().is_empty());
    assert!(!service.pane_bootstrap_is_pending_for_tests(pane_id));

    service.terminate_all_pane_processes().unwrap();
    service.cleanup_removed_pane_runtime_state(pane_id).unwrap();
    assert!(!service.managed_fish_adapter_is_ready_for_tests(pane_id));
}

/// Verifies managed Fish and Zsh retain ownership of deferred bootstrap work
/// until their private receivers report installation.
///
/// Shells are resolved independently because ordinary cross-platform test
/// environments may provide only one of them. The dedicated managed-shell
/// reliability suite remains responsible for requiring both interpreters.
#[test]
fn runtime_managed_fish_and_zsh_bootstrap_wait_for_receiver_installation() {
    for (shell_name, executable, known_locations) in [
        (
            "Fish",
            "fish",
            &[
                "/usr/bin/fish",
                "/usr/local/bin/fish",
                "/opt/homebrew/bin/fish",
            ][..],
        ),
        (
            "Zsh",
            "zsh",
            &["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh"][..],
        ),
    ] {
        let Some(shell_path) = find_test_shell(executable, known_locations) else {
            eprintln!(
                "skipping managed {shell_name} bootstrap regression because it is unavailable"
            );
            continue;
        };
        let shell_path_display = shell_path.display().to_string();
        let mut service = test_runtime_service();
        service.session.shell = ResolvedShell::new(shell_path, ShellSource::ShellEnv).into();
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        let pane_id = "%1";
        service.begin_agent_subshell_shell_handoff(pane_id).unwrap();
        let (marker, wrapper) = service
            .prepare_bootstrap_to_pane(pane_id)
            .unwrap()
            .expect("managed shell bootstrap should register");
        service.bind_agent_subshell_bootstrap_marker(pane_id, &marker);
        service.defer_agent_subshell_bootstrap_wrapper(pane_id, &marker, wrapper);
        service.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);
        let _ = service.drain_pane_io_transition();

        assert_eq!(
            service.maybe_bootstrap_ready_panes().unwrap(),
            0,
            "{shell_path_display} prompt readiness must not consume receiver-owned bootstrap"
        );
        assert!(
            service.drain_pane_io_transition().side_effects.is_empty(),
            "{shell_path_display} prompt readiness unexpectedly dispatched deferred bootstrap"
        );
        assert!(service.pane_bootstrap_is_pending_for_tests(pane_id));
        assert!(
            service
                .running_shell_transactions_for_tests()
                .contains_key(&marker),
            "{shell_path_display} bootstrap transaction should remain receiver-owned"
        );
        service.terminate_all_pane_processes().unwrap();
    }
}

/// Verifies pending payload handoff uses a short start-marker deadline.
///
/// Non-stateful shell actions wait for an OSC start marker before sending the
/// encoded command body. If that marker is lost or the wrapper never reaches
/// the receiver loop, the transaction should time out quickly instead of
/// occupying the pane until the full command timeout expires.
#[test]
fn runtime_shell_transaction_pending_payload_uses_short_start_timer() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1".to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-start".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-1".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "grep -n needle file.txt".to_string(),
            started_at_unix_ms: 1_000,
            timeout_ms: Some(10 * 60 * 1000),
            pending_input_payload: Some(
                mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                    b"payload\n".to_vec(),
                    "marker-start",
                    true,
                ),
            ),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let timer = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == "marker-start")
        .unwrap();

    assert_eq!(timer.timeout_ms, 30_000);

    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            "marker-start",
            "turn-1",
            "agent-%1",
            &pane_id,
        )
        .unwrap();
    let timer = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == "marker-start")
        .unwrap();
    assert_eq!(timer.timeout_ms, 10 * 60 * 1000);
    let _ = process.terminate(Duration::from_millis(10));
}
