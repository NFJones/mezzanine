//! Control schemas tests.

use super::*;

/// Verifies baseline control methods reject unknown params outside extensions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn baseline_control_methods_reject_unknown_params_outside_extensions() {
    let (mut session, primary) = test_session();

    let unknown = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"a","surprise":true}}"#,
        &mut session,
        &primary,
    );
    assert!(unknown.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(unknown.contains("window/create params contains unknown field"));

    let bad_extensions = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"b","extensions":1}}"#,
        &mut session,
        &primary,
    );
    assert!(bad_extensions.contains("extensions must be an object"));

    let accepted = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"c","extensions":{"vendor":true}}}"#,
        &mut session,
        &primary,
    );
    assert!(accepted.contains(r#""window":"#));
}

/// Verifies that advertised primary control methods are backed by the shared
/// method registry that now owns dispatch and schema metadata. This prevents a
/// future control method from being added to capabilities without a matching
/// dispatch/schema entry.
#[test]
fn advertised_primary_control_methods_have_registry_entries() {
    for method in PRIMARY_CONTROL_METHODS {
        assert!(
            control_method_spec(method).is_some(),
            "{method} is advertised but missing from the control method registry"
        );
    }
}

/// Verifies that approval control methods use the same unknown-parameter schema
/// enforcement as the main control dispatcher. Approval handling has its own
/// specialized path because it needs access to the blocked-approval queue, so
/// this regression keeps that path from silently ignoring extra request fields.
#[test]
fn approval_control_methods_reject_unknown_params_on_specialized_dispatch() {
    let (mut session, primary) = test_session();
    let mut queue = BlockedApprovalQueue::default();

    let list = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":1,"method":"approval/list","params":{"surprise":true}}"#,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(list.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(list.contains("approval/list params contains unknown field"));

    let decide = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":2,"method":"approval/decide","params":{"approval_id":"ba1","decision":"approve","idempotency_key":"decide","surprise":true}}"#,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(decide.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(decide.contains("approval/decide params contains unknown field"));

    let invalid_scope = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":3,"method":"approval/decide","params":{"approval_id":"ba1","decision":"approve","scope":{"persistence":"session","surprise":true},"idempotency_key":"bad-scope"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(invalid_scope.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(invalid_scope.contains("approval/decide scope contains unknown field"));

    let invalid_prefix = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":4,"method":"approval/decide","params":{"approval_id":"ba1","decision":"approve","scope":{"persistence":"session","command_prefix":"git diff"},"idempotency_key":"bad-prefix"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(invalid_prefix.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(invalid_prefix.contains("command_prefix must be an array"));

    let invalid_digest = dispatch_control_request_with_approvals(
        r#"{"jsonrpc":"2.0","id":5,"method":"approval/decide","params":{"approval_id":"ba1","decision":"approve","scope":{"persistence":"session","exact_sha256":"not-a-digest"},"idempotency_key":"bad-digest"}}"#,
        &mut session,
        &primary,
        &mut queue,
    );

    assert!(invalid_digest.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(invalid_digest.contains("exact_sha256 must be a 64-character hexadecimal digest"));
}

/// Verifies that snapshot control methods validate their parameter schema before
/// repository operations. Snapshot dispatch is specialized so it can receive a
/// repository and capture context, but it still exposes JSON-RPC methods whose
/// request objects must reject unknown fields outside `extensions`.
#[test]
fn snapshot_control_methods_reject_unknown_params_on_specialized_dispatch() {
    let (mut session, primary) = test_session();
    let root = temp_root("snapshot-unknown-params");
    let snapshots = SnapshotRepository::new(root.to_path_buf());

    let response = dispatch_control_request_with_snapshots(
        &JsonRpcRequestBuilder::method("snapshot/list")
            .params_json(r#"{"surprise":true}"#)
            .build(),
        &mut session,
        &primary,
        &snapshots,
    );

    assert!(response.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(response.contains("snapshot/list params contains unknown field"));

    let null_target = dispatch_control_request_with_snapshots(
        &JsonRpcRequestBuilder::method("snapshot/list")
            .id(2)
            .params_json(r#"{"target":null}"#)
            .build(),
        &mut session,
        &primary,
        &snapshots,
    );
    assert!(null_target.contains(r#""snapshots":[]"#), "{null_target}");

    let invalid_target = dispatch_control_request_with_snapshots(
        r#"{"jsonrpc":"2.0","id":3,"method":"snapshot/list","params":{"target":"default"}}"#,
        &mut session,
        &primary,
        &snapshots,
    );
    assert!(
        invalid_target.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_target}"
    );

    let missing_target = dispatch_control_request_with_snapshots(
        r#"{"jsonrpc":"2.0","id":4,"method":"snapshot/list","params":{"target":{"session_id":"missing"}}}"#,
        &mut session,
        &primary,
        &snapshots,
    );
    assert!(
        missing_target.contains(r#""mezzanine_code":"not_found""#),
        "{missing_target}"
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies generic control dispatches empty snapshot state without repository.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn generic_control_dispatches_empty_snapshot_state_without_repository() {
    let (mut session, primary) = test_session();

    let list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"snapshot/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(list.contains(r#""snapshots":[]"#), "{list}");

    let null_target_list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":10,"method":"snapshot/list","params":{"target":null}}"#,
        &mut session,
        &primary,
    );
    assert!(
        null_target_list.contains(r#""snapshots":[]"#),
        "{null_target_list}"
    );

    let invalid_target_list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":11,"method":"snapshot/list","params":{"target":"default"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        invalid_target_list.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_target_list}"
    );

    let create = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"snapshot/create","params":{"target":{"default":true},"name":"manual","idempotency_key":"snapshot-create"}}"#,
        &mut session,
        &primary,
    );
    assert!(create.contains(r#""error""#), "{create}");
    assert!(create.contains(r#""code":-32004"#), "{create}");
    assert!(
        create.contains(r#""mezzanine_code":"invalid_state""#),
        "{create}"
    );
    assert!(
        create.contains("snapshot repository is not configured"),
        "{create}"
    );

    let resume_missing_id = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"snapshot/resume","params":{"idempotency_key":"snapshot-resume"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        resume_missing_id.contains("snapshot/resume requires snapshot_id"),
        "{resume_missing_id}"
    );
}

/// Verifies control frame round trips raw json body.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn control_frame_round_trips_raw_json_body() {
    let encoded = encode_control_body(r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize"}"#);

    let (body, consumed) = decode_control_frame(&encoded, 4096).unwrap();

    assert_eq!(
        body,
        r#"{"jsonrpc":"2.0","id":1,"method":"control/initialize"}"#
    );
    assert_eq!(consumed, encoded.len());
}

/// Verifies parses json rpc request envelope.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_json_rpc_request_envelope() {
    let request = parse_json_rpc_request(
        r#"{"jsonrpc":"2.0","id":"abc","method":"session/get","params":{}}"#,
    )
    .unwrap();

    assert_eq!(request.id, r#""abc""#);
    assert_eq!(request.method, "session/get");
    assert_eq!(request.params.as_deref(), Some("{}"));
}

/// Verifies json rpc parser uses top level fields and requires object params.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn json_rpc_parser_uses_top_level_fields_and_requires_object_params() {
    let request = parse_json_rpc_request(
        r#"{"jsonrpc":"2.0","id":7,"method":"session/get","params":{"method":"nested/ignored"}}"#,
    )
    .unwrap();
    assert_eq!(request.id, "7");
    assert_eq!(request.method, "session/get");
    assert_eq!(
        request.params.as_deref(),
        Some(r#"{"method":"nested/ignored"}"#)
    );

    let error =
        parse_json_rpc_request(r#"{"jsonrpc":"2.0","id":8,"method":"session/get","params":[]}"#)
            .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}
