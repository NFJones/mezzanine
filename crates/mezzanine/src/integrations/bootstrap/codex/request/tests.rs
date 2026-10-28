//! Source-shaped facts from actual model hooks, never a simulated durable ACK.

use super::*;

/// Builds only the source's known request/counter fields and deliberately adds
/// private data and a selected model that must not enter the normalized fact.
fn input() -> serde_json::Value {
    serde_json::json!({
        "hook_event_name":"PostModelRequest", "session_id":"session-a", "turn_id":"turn-a",
        "status":"completed", "model":"PRIVATE-selected-model", "cwd":"/PRIVATE",
        "transcript_path":"/PRIVATE", "error":"PRIVATE", "tool_input":{"text":"PRIVATE"},
        "token_usage":{"input_tokens":10,"output_tokens":3,"cached_input_tokens":2,
            "cache_write_input_tokens":4,"reasoning_output_tokens":1,"total_tokens":13}
    })
}

/// Uses the real bounded JSON decoder and counter validator, preserving unknown
/// usage separately from invalid input, rather than mirroring projection logic.
fn project(value: &serde_json::Value) -> Observation {
    normalize(&serde_json::to_vec(value).unwrap())
        .unwrap()
        .unwrap()
}

/// Per-request input/output are already inclusive; summing cache or reasoning
/// again would overcharge. Selected model and private fields provide no response
/// model, source credential, original request ID, or expense-origin authority.
#[test]
fn codex_model_hook_projects_checked_inclusive_counters_without_private_metadata() {
    let fact = project(&input());
    assert_eq!(fact.session, "session-a");
    assert_eq!(fact.turn, "turn-a");
    assert_eq!(fact.agent_id, None);
    assert_eq!(
        fact.phase,
        Phase::After {
            status: Status::Completed,
            counters: Some(ExternalCounters {
                input_tokens: 10,
                output_tokens: 3,
                cached_input_tokens: Some(2),
                cache_write_input_tokens: Some(4),
                reasoning_tokens: Some(1),
            })
        }
    );
    assert!(!format!("{fact:?}").contains("PRIVATE"));
}

/// The source can report actual upstream usage after failure or cancellation.
/// Status must not erase those counters or convert a missing outcome to zero.
#[test]
fn codex_model_hook_retains_failed_and_cancelled_incurred_usage() {
    for (name, status) in [
        ("completed", Status::Completed),
        ("failed", Status::Failed),
        ("cancelled", Status::Cancelled),
    ] {
        let mut value = input();
        value["status"] = name.into();
        let Phase::After {
            status: actual,
            counters,
        } = project(&value).phase
        else {
            panic!("post fact");
        };
        assert_eq!(actual, status);
        assert_eq!(counters.unwrap().input_tokens, 10);
        value["token_usage"] = serde_json::Value::Null;
        assert_eq!(
            project(&value).phase,
            Phase::After {
                status,
                counters: None
            }
        );
    }
}

/// A pre hook is only a source boundary hint. Payload counters on that event
/// never become a finalized report; only runtime-native qualification may admit
/// a request. Non-model hooks cannot smuggle occupancy counters through this API.
#[test]
fn codex_model_hook_pre_and_unknown_events_do_not_infer_expense() {
    let mut value = input();
    value["hook_event_name"] = "PreModelRequest".into();
    assert_eq!(project(&value).phase, Phase::Before);
    for event in [
        "SessionStart",
        "Stop",
        "UserPromptSubmit",
        "response.completed",
        "turn.complete",
    ] {
        value["hook_event_name"] = event.into();
        assert_eq!(
            normalize(&serde_json::to_vec(&value).unwrap()).unwrap(),
            None
        );
    }
}

/// Explicit zeros are evidence; omitted/null category fields remain unknown.
/// Missing/null finalized usage or mandatory input/output is never a zero bill.
#[test]
fn codex_model_hook_distinguishes_zero_unknown_and_missing_usage() {
    let mut value = input();
    value["token_usage"] = serde_json::json!({"input_tokens":0,"output_tokens":0,
        "cached_input_tokens":0,"cache_write_input_tokens":0,"reasoning_output_tokens":0,"total_tokens":0});
    let Phase::After {
        counters: Some(zero),
        ..
    } = project(&value).phase
    else {
        panic!("known zero");
    };
    assert_eq!(zero.reasoning_tokens, Some(0));
    assert_eq!(zero.cached_input_tokens, Some(0));
    value["token_usage"] = serde_json::json!({"input_tokens":0,"output_tokens":0});
    let Phase::After {
        counters: Some(unknown),
        ..
    } = project(&value).phase
    else {
        panic!("known inclusive totals");
    };
    assert_eq!(unknown.reasoning_tokens, None);
    assert_eq!(unknown.cached_input_tokens, None);
    assert_eq!(unknown.cache_write_input_tokens, None);
    value["token_usage"]["reasoning_output_tokens"] = serde_json::Value::Null;
    assert_eq!(
        project(&value).phase,
        Phase::After {
            status: Status::Completed,
            counters: Some(unknown)
        }
    );
    for usage in [
        serde_json::Value::Null,
        serde_json::json!({"input_tokens":0}),
        serde_json::json!([]),
    ] {
        value["token_usage"] = usage;
        assert_eq!(
            project(&value).phase,
            Phase::After {
                status: Status::Completed,
                counters: None
            }
        );
    }
    value.as_object_mut().unwrap().remove("token_usage");
    assert_eq!(
        project(&value).phase,
        Phase::After {
            status: Status::Completed,
            counters: None
        }
    );
}

/// Every known counter rejects negative, fractional, string and overflowing
/// values. Invalid subsets and redundant totals cannot fabricate incurred usage.
#[test]
fn codex_model_hook_refuses_invalid_counter_shapes_and_inconsistent_subsets() {
    for field in [
        "input_tokens",
        "output_tokens",
        "cached_input_tokens",
        "cache_write_input_tokens",
        "reasoning_output_tokens",
        "total_tokens",
    ] {
        for bad in [
            serde_json::json!(-1),
            serde_json::json!(0.5),
            serde_json::json!("1"),
            serde_json::json!(u64::MAX),
        ] {
            let mut value = input();
            value["token_usage"][field] = bad;
            assert_eq!(
                project(&value).phase,
                Phase::After {
                    status: Status::Completed,
                    counters: None
                }
            );
        }
    }
    for (field, bad) in [
        ("cached_input_tokens", 9),
        ("cache_write_input_tokens", 9),
        ("reasoning_output_tokens", 4),
        ("total_tokens", 14),
    ] {
        let mut value = input();
        value["token_usage"][field] = bad.into();
        assert_eq!(
            project(&value).phase,
            Phase::After {
                status: Status::Completed,
                counters: None
            }
        );
    }
}

/// Child IDs remain explicit metadata, not parent authority. A child-shaped
/// event without its exact selector cannot silently turn into a main request;
/// matching session/turn labels alone never merge the two facts.
#[test]
fn codex_model_hook_preserves_explicit_child_scope_without_parent_adoption() {
    let main = project(&input());
    let mut value = input();
    value["agent_id"] = "child-a".into();
    value["agent_type"] = "PRIVATE-profile".into();
    let child = project(&value);
    assert_eq!(child.agent_id.as_deref(), Some("child-a"));
    assert_ne!(child, main);
    assert!(!format!("{child:?}").contains("PRIVATE"));
    value.as_object_mut().unwrap().remove("agent_id");
    assert!(normalize(&serde_json::to_vec(&value).unwrap()).is_err());
    for id in [
        serde_json::Value::Null,
        serde_json::json!("../PRIVATE"),
        serde_json::json!(false),
    ] {
        value["agent_id"] = id;
        assert!(normalize(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

/// Unique bounded input and inert selectors are prerequisites, independent of
/// source version. Duplicate nested counters cannot collapse to a chosen value;
/// errors do not echo arbitrary status/path data into logs or helper responses.
#[test]
fn codex_model_hook_rejects_ambiguous_unbounded_or_content_shaped_input() {
    for bytes in [
        br#"{"hook_event_name":"PostModelRequest","hook_event_name":"PreModelRequest"}"#.as_slice(),
        br#"{"hook_event_name":"PostModelRequest","session_id":"a","turn_id":"b","status":"completed","token_usage":{"input_tokens":1,"input_tokens":2}}"#,
        b"[]", b"null", b"not json",
    ] { assert!(normalize(bytes).is_err()); }
    assert!(normalize(&vec![b' '; 65_537]).is_err());
    let mut value = input();
    value["status"] = "PRIVATE-invalid".into();
    assert!(
        !normalize(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .message()
            .contains("PRIVATE")
    );
    for field in ["session_id", "turn_id"] {
        let mut value = input();
        value[field] = "../PRIVATE".into();
        assert!(normalize(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut value = input();
    for index in 0..256 {
        value[format!("unknown-{index}")] = true.into();
    }
    assert!(normalize(&serde_json::to_vec(&value).unwrap()).is_err());
}
