use super::*;

fn report(input: u64, output: u64, input_present: bool, output_present: bool) -> Report {
    (
        ModelTokenUsage {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        },
        ModelTokenUsagePresence {
            input: input_present,
            output: output_present,
        },
    )
}

fn finish(state: &mut ChatStreamUsage, index: u64) {
    state.observe(
        &serde_json::json!({"choices":[{"index":index,"finish_reason":"stop"}]}),
        Default::default(),
        None,
    );
}

#[test]
fn terminal_report_does_not_borrow_prefix_presence_or_done() {
    let mut state = ChatStreamUsage::default();
    let prefix = report(11, 3, true, true);
    state.observe(
        &serde_json::json!({"choices":[{"index":0,"finish_reason":null}]}),
        Default::default(),
        Some(prefix),
    );
    finish(&mut state, 0);
    assert!(state.terminal_usage().is_none());
    let final_report = report(0, 3, false, true);
    state.observe(
        &serde_json::json!({"choices":[]}),
        prefix.0,
        Some(final_report),
    );
    assert_eq!(state.terminal_usage(), Some(final_report));
}

#[test]
fn regression_stays_distinct_from_conflict_and_later_repeat() {
    for decreased in [report(10, 3, true, true), report(11, 0, true, true)] {
        let mut state = ChatStreamUsage::default();
        finish(&mut state, 0);
        let original = report(11, 3, true, true);
        let frame = serde_json::json!({"choices":[]});
        assert!(!state.observe(&frame, original.0, Some(original)));
        assert!(state.observe(&frame, original.0, Some(decreased)));
        assert!(!state.observe(&frame, original.0, Some(original)));
        assert!(state.regressed());
        assert!(state.terminal_usage().is_none());
    }
}

#[test]
fn every_observed_choice_and_report_must_qualify() {
    let mut state = ChatStreamUsage::default();
    state.observe(
        &serde_json::json!({"choices":[{"index":0},{"index":1}]}),
        Default::default(),
        None,
    );
    finish(&mut state, 0);
    state.observe(
        &serde_json::json!({"choices":[]}),
        Default::default(),
        Some(report(11, 3, true, true)),
    );
    assert!(state.terminal_usage().is_none());
    finish(&mut state, 1);
    state.observe(
        &serde_json::json!({"choices":[]}),
        Default::default(),
        Some(report(11, 3, true, true)),
    );
    assert!(state.terminal_usage().is_some());
    assert!(!state.observe(
        &serde_json::json!({"choices":[]}),
        Default::default(),
        Some(report(13, 3, true, true))
    ));
    assert!(!state.regressed());
    assert!(state.terminal_usage().is_none());
}

#[test]
fn malformed_or_excess_choice_state_never_qualifies() {
    for choices in [
        serde_json::json!([{"finish_reason":"stop"}]),
        serde_json::json!(
            (0..65)
                .map(|index| serde_json::json!({"index":index,"finish_reason":"stop"}))
                .collect::<Vec<_>>()
        ),
    ] {
        let mut state = ChatStreamUsage::default();
        state.observe(
            &serde_json::json!({"choices":choices}),
            Default::default(),
            Some(report(0, 0, true, true)),
        );
        assert!(state.terminal_usage().is_none());
        assert!(state.choices.len() <= 64);
    }
}

#[test]
fn malformed_suffix_cannot_erase_a_terminal_regression() {
    let mut state = ChatStreamUsage::default();
    finish(&mut state, 0);
    let original = report(11, 3, true, true);
    state.observe(
        &serde_json::json!({"choices":[]}),
        original.0,
        Some(original),
    );
    assert!(state.observe(
        &serde_json::json!({"error":{}}),
        original.0,
        Some(report(10, 3, true, true))
    ));
    assert!(state.regressed());
    assert!(state.terminal_usage().is_none());
}
