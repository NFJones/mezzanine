//! Control trust tests.

use super::*;

/// Verifies project trust control methods decide list inspect and revoke.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn project_trust_control_methods_decide_list_inspect_and_revoke() {
    let mut store = ProjectTrustStore::default();
    let root = std::env::temp_dir()
        .join(format!("mez-control-trust-{}", std::process::id()))
        .to_string_lossy()
        .to_string();
    let decide = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"project/trust/decide","params":{{"project_root":"{}","decision":"trust","idempotency_key":"trust"}}}}"#,
        json_escape(&root)
    );

    let decide_response = dispatch_project_trust_request(&decide, &mut store);
    assert!(decide_response.contains(r#""state":"trusted""#));
    assert!(decide_response.contains(&format!(r#""id":"{}""#, json_escape(&root))));
    assert!(decide_response.contains(r#""version":1"#));
    assert!(decide_response.contains(r#""trusted_at":""#));
    assert!(!decide_response.contains(r#""trusted_at":"unix:"#));
    assert!(decide_response.contains(r#""rejected_at":null"#));
    assert!(decide_response.contains(r#""revoked_at":null"#));
    assert!(decide_response.contains(r#""decided_by_client_id":null"#));
    assert!(decide_response.contains(r#""overlay_files":[]"#));
    assert!(decide_response.contains(r#""capability_expansion_summary":[]"#));
    assert!(decide_response.contains(r#""diagnostics":[]"#));

    let list_response = dispatch_project_trust_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"project/trust/list","params":{}}"#,
        &mut store,
    );
    assert!(list_response.contains(&json_escape(&root)));

    let trusted_list_response = dispatch_project_trust_request(
        r#"{"jsonrpc":"2.0","id":20,"method":"project/trust/list","params":{"state":"trusted"}}"#,
        &mut store,
    );
    assert!(
        trusted_list_response.contains(&json_escape(&root)),
        "{trusted_list_response}"
    );

    let pending_list_response = dispatch_project_trust_request(
        r#"{"jsonrpc":"2.0","id":21,"method":"project/trust/list","params":{"state":"pending"}}"#,
        &mut store,
    );
    assert!(
        !pending_list_response.contains(&json_escape(&root)),
        "{pending_list_response}"
    );

    let invalid_state_response = dispatch_project_trust_request(
        r#"{"jsonrpc":"2.0","id":22,"method":"project/trust/list","params":{"state":"unknown"}}"#,
        &mut store,
    );
    assert!(
        invalid_state_response.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_state_response}"
    );

    let inspect = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"project/trust/inspect","params":{{"project_root":"{}"}}}}"#,
        json_escape(&root)
    );
    let inspect_response = dispatch_project_trust_request(&inspect, &mut store);
    assert!(inspect_response.contains(r#""state":"trusted""#));

    let revoke = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"project/trust/revoke","params":{{"project_root":"{}","idempotency_key":"revoke"}}}}"#,
        json_escape(&root)
    );
    let revoke_response = dispatch_project_trust_request(&revoke, &mut store);
    assert!(revoke_response.contains(r#""state":"revoked""#));

    let revoked_list_response = dispatch_project_trust_request(
        r#"{"jsonrpc":"2.0","id":23,"method":"project/trust/list","params":{"state":"revoked"}}"#,
        &mut store,
    );
    assert!(
        revoked_list_response.contains(&json_escape(&root)),
        "{revoked_list_response}"
    );
}

/// Verifies generic control dispatches empty project trust state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn generic_control_dispatches_empty_project_trust_state() {
    let (mut session, primary) = test_session();

    let list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"project/trust/list","params":{"state":null}}"#,
        &mut session,
        &primary,
    );
    assert!(list.contains(r#""projects":[]"#), "{list}");

    let invalid_state = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":10,"method":"project/trust/list","params":{"state":"unknown"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        invalid_state.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_state}"
    );

    let inspect = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"project/trust/inspect","params":{"project_root":"/tmp/missing"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        inspect.contains(r#""mezzanine_code":"not_found""#),
        "{inspect}"
    );
    assert!(inspect.contains("project not found"), "{inspect}");

    let decide = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"project/trust/decide","params":{"project_root":"/tmp/project","decision":"trust","idempotency_key":"trust-project"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        decide.contains(r#""mezzanine_code":"invalid_state""#),
        "{decide}"
    );
    assert!(
        decide.contains("project trust store is not configured"),
        "{decide}"
    );

    let unknown = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"project/trust/list","params":{"unexpected":true}}"#,
        &mut session,
        &primary,
    );
    assert!(unknown.contains("project/trust/list params contains unknown field"));
}

/// Verifies observer cannot inspect other observer request.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn observer_cannot_inspect_other_observer_request() {
    let (mut session, _primary) = test_session();
    let (first_client, _first_request) = session.request_observer("first");
    let (_second_client, second_request) = session.request_observer("second");
    let inspect_request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"observer/inspect","params":{{"observer_request_id":"{}"}}}}"#,
        second_request
    );

    let response =
        dispatch_control_request_for_client(&inspect_request, &mut session, &first_client, None);

    assert!(response.contains(r#""mezzanine_code":"forbidden""#));
}
