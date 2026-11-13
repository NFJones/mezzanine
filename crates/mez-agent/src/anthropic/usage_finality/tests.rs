//! Exercise the real incremental decoder with distinct start/delta presence.
//! These pure grammar checks supplement, not replace, native receipt tests.

use super::*;
use crate::{AnthropicMessagesStreamDecoder, SseEvent};
use serde_json::json;

fn push(decoder: &mut AnthropicMessagesStreamDecoder, value: Value) {
    decoder
        .push_event(&SseEvent {
            name: None,
            data: value.to_string(),
        })
        .unwrap();
}

fn start(usage: Value) -> AnthropicMessagesStreamDecoder {
    let mut decoder = AnthropicMessagesStreamDecoder::default();
    push(
        &mut decoder,
        json!({"type":"message_start", "message":{
            "type":"message", "role":"assistant", "usage":usage
        }}),
    );
    decoder
}

fn delta(decoder: &mut AnthropicMessagesStreamDecoder, usage: Value) {
    push(
        decoder,
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":usage}),
    );
}

fn complete(decoder: &mut AnthropicMessagesStreamDecoder) -> Option<(u64, u64, bool, bool)> {
    push(decoder, json!({"type":"message_stop"}));
    decoder
        .terminal_usage()
        .map(|(u, p)| (u.input_tokens, u.output_tokens, p.input, p.output))
}

#[test]
fn anthropic_inclusive_final_presence_requires_checked_explicit_components() {
    for (value, input, output) in [
        (json!({"input_tokens":3,"output_tokens":2}), false, true),
        (json!({"cache_read_input_tokens":3}), false, false),
        (
            json!({"input_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}),
            true,
            true,
        ),
        (
            json!({"input_tokens":3,"cache_read_input_tokens":4,"cache_creation_input_tokens":5}),
            true,
            false,
        ),
        (
            json!({"input_tokens":u64::MAX,"cache_read_input_tokens":1,"cache_creation_input_tokens":0}),
            false,
            false,
        ),
        (
            json!({"input_tokens":3,"cache_read_input_tokens":null,"cache_creation_input_tokens":0}),
            false,
            false,
        ),
    ] {
        let p = anthropic_final_usage_presence(Some(&value));
        assert_eq!((p.input, p.output), (input, output), "{value}");
    }
}

#[test]
fn anthropic_final_output_never_borrows_start_or_prefix_presence() {
    let usage = json!({"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":3,"output_tokens":7});
    let mut decoder = start(usage.clone());
    delta(&mut decoder, json!({}));
    assert!(
        decoder.terminal_usage().is_none(),
        "stop delta is not message_stop"
    );
    assert_eq!(complete(&mut decoder), Some((11, 0, true, false)));
    assert_eq!(decoder.reported_usage().output_tokens, 7);
    let mut decoder = start(usage);
    push(
        &mut decoder,
        json!({"type":"message_delta","delta":{},"usage":{"output_tokens":7}}),
    );
    assert_eq!(complete(&mut decoder), None, "no terminal delta");
}

#[test]
fn anthropic_changed_partial_cache_degrades_input_not_output() {
    for (report, input, complete_input) in [
        (
            json!({"cache_read_input_tokens":7,"output_tokens":3}),
            13,
            false,
        ),
        (
            json!({"cache_read_input_tokens":5,"output_tokens":3}),
            11,
            true,
        ),
        (
            json!({"input_tokens":3,"cache_read_input_tokens":7,"cache_creation_input_tokens":3,"output_tokens":3}),
            13,
            true,
        ),
    ] {
        let mut decoder = start(
            json!({"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":3}),
        );
        delta(&mut decoder, report);
        let result = complete(&mut decoder).unwrap();
        assert_eq!((result.2, result.3), (complete_input, true));
        if complete_input {
            assert_eq!(result.0, input);
        }
        assert_eq!(decoder.reported_usage().input_tokens, input);
    }
}

#[test]
fn anthropic_terminal_component_regression_is_sticky_even_at_equal_inclusive_total() {
    let mut decoder = start(
        json!({"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":3,"output_tokens":3}),
    );
    delta(
        &mut decoder,
        json!({"input_tokens":2,"cache_read_input_tokens":6,"cache_creation_input_tokens":3,"output_tokens":3}),
    );
    assert!(decoder.terminal_usage_regressed());
    assert_eq!(decoder.reported_usage().input_tokens, 11);
    assert_eq!(decoder.reported_usage().cached_input_tokens, Some(5));
    delta(&mut decoder, json!({"output_tokens":3}));
    assert!(decoder.terminal_usage_regressed());
    assert!(complete(&mut decoder).is_none());
}

#[test]
fn anthropic_conflicting_delta_and_late_partial_report_never_requalify() {
    for changed in [
        json!({"output_tokens":5}),
        json!({"cache_read_input_tokens":7}),
    ] {
        let mut decoder = start(
            json!({"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":3}),
        );
        delta(&mut decoder, json!({"output_tokens":3}));
        assert!(complete(&mut decoder).is_some());
        push(
            &mut decoder,
            json!({"type":"message_delta","delta":{},"usage":changed}),
        );
        assert!(decoder.terminal_usage().is_none());
        assert!(decoder.final_usage.conflicting_report());
        delta(&mut decoder, json!({"output_tokens":3}));
        assert!(complete(&mut decoder).is_none());
    }
}

#[test]
fn anthropic_error_usage_does_not_become_terminal_evidence() {
    let mut decoder = start(
        json!({"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":3}),
    );
    delta(&mut decoder, json!({"output_tokens":3}));
    let event = SseEvent { name:None, data:json!({"type":"error","usage":{"output_tokens":4},"error":{"type":"overloaded_error","message":"fixture"}}).to_string() };
    assert!(decoder.push_event(&event).is_err());
    assert_eq!(decoder.reported_usage().output_tokens, 4);
    assert!(complete(&mut decoder).is_none());
}

#[test]
fn anthropic_mixed_terminal_regression_retains_each_known_primary_maximum() {
    for (raw, input, output) in [
        (
            json!({"input_tokens":2,"cache_read_input_tokens":5,"cache_creation_input_tokens":3,"output_tokens":5}),
            11,
            5,
        ),
        (
            json!({"input_tokens":4,"cache_read_input_tokens":5,"cache_creation_input_tokens":3,"output_tokens":2}),
            12,
            3,
        ),
    ] {
        let mut decoder = start(
            json!({"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":3,"output_tokens":3}),
        );
        delta(&mut decoder, raw);
        let usage = decoder.reported_usage();
        assert_eq!((usage.input_tokens, usage.output_tokens), (input, output));
        assert!(decoder.terminal_usage_regressed());
        delta(&mut decoder, json!({"output_tokens":3}));
        let usage = decoder.reported_usage();
        assert_eq!((usage.input_tokens, usage.output_tokens), (input, output));
        assert!(complete(&mut decoder).is_none());
    }
}

#[test]
fn anthropic_stop_before_delta_cannot_be_reopened_into_complete_coverage() {
    let mut decoder = start(
        json!({"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":3}),
    );
    assert!(complete(&mut decoder).is_none());
    delta(&mut decoder, json!({"output_tokens":3}));
    assert!(complete(&mut decoder).is_none());
    assert!(
        !decoder.final_usage.conflicting_report(),
        "malformed pre-final grammar is coverage-only"
    );
}

#[test]
fn anthropic_named_error_overrides_conflicting_json_type_for_finality_and_expense() {
    let mut decoder = start(
        json!({"input_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":3}),
    );
    delta(&mut decoder, json!({"output_tokens":3}));
    let event = SseEvent { name:Some("error".into()), data:json!({"type":"message_stop","usage":{"output_tokens":5},"error":{"type":"overloaded_error","message":"fixture"}}).to_string() };
    assert!(decoder.push_event(&event).is_err());
    assert_eq!(decoder.reported_usage().output_tokens, 5);
    assert!(complete(&mut decoder).is_none());
}
