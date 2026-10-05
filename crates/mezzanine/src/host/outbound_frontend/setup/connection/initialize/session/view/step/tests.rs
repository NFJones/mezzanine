//! Closed input request and runtime acknowledgement qualification.
//!
//! Validation occurs before remote mutation. Accepted input count is not proof
//! of process delivery; unknown response fields and credentials are not projected.

use super::*;

/// Exact handles and primary settlement are required before bounded input is
/// forwarded. Unknown envelope fields cannot select another method or session.
#[test]
fn outbound_input_request_requires_exact_primary_and_budget() {
    let handle = FrontendHandle {
        owner: "fixture".into(),
        generation: 1,
    };
    let body = serde_json::json!({"operation":"step","handle":handle,
        "columns":80,"rows":24,"idempotency_key":"exact-input","input_bytes":[97,98]});
    let request: StepRequest = serde_json::from_value(body.clone()).unwrap();
    validate_request(
        &request,
        &handle,
        &serde_json::json!({"granted_role":"primary"}),
    )
    .unwrap();
    assert!(
        validate_request(
            &request,
            &handle,
            &serde_json::json!({"granted_role":"observer"})
        )
        .is_err()
    );
    for (pointer, value) in [
        ("/operation", serde_json::json!("initialize")),
        ("/handle/generation", serde_json::json!(2)),
        ("/columns", serde_json::json!(0)),
        ("/rows", serde_json::json!(4097)),
        ("/idempotency_key", serde_json::json!("")),
        ("/input_bytes", serde_json::json!(vec![0_u8; 513])),
    ] {
        let mut changed = body.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let request: StepRequest = serde_json::from_value(changed).unwrap();
        assert!(
            validate_request(
                &request,
                &handle,
                &serde_json::json!({"granted_role":"primary"})
            )
            .is_err()
        );
    }
    let mut foreign = body;
    foreign["session_id"] = serde_json::json!("$2");
    assert!(serde_json::from_value::<StepRequest>(foreign).is_err());
}

/// Correlation and exact runtime input acceptance must precede a content-free
/// acknowledgement. Lifecycle flags are preserved rather than invented from
/// input size, and forwarded-byte or credential metadata never crosses IPC.
#[test]
fn outbound_input_acknowledgement_is_exact_and_content_free() {
    let original = serde_json::json!({"jsonrpc":"2.0","id":STEP_ID,"result":{
        "input_bytes":2,"client_detached":false,"session_terminated":false,
        "application":{"forwarded_bytes":0},"device_credential":"not-projected"}});
    assert_eq!(
        project_acknowledgement(&original.to_string(), 2).unwrap(),
        serde_json::json!({"input_bytes":2,"client_detached":false,"session_terminated":false})
    );
    for (pointer, value) in [
        ("/id", serde_json::json!("other")),
        ("/result/input_bytes", serde_json::json!(1)),
        ("/result/client_detached", serde_json::Value::Null),
        ("/result/session_terminated", serde_json::json!("false")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        let error = project_acknowledgement(&changed.to_string(), 2).unwrap_err();
        assert!(!error.message().contains("not-projected"));
    }
}
