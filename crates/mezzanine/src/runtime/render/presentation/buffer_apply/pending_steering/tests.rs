//! Structural bounds for exact steering source associations.
//!
//! Test metadata bytes before feeding large sources into terminal presentation.
//! A payload appears once, with constant-size references on other physical rows.

use super::*;

/// Actual pending overflow and settled promotion retain a single full source
/// anchor in bounded terminal history, not one payload per rendered row. Run
/// this only after the helper-level linear bound has established safe metadata.
#[test]
fn steering_copy_large_multiline_terminal_projection_retains_one_anchor() {
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.start_agent_prompt_turn("%1", "initial").unwrap();
    let display = "short source\r\n".repeat(60_000);
    service
        .inject_agent_steering_with_display("%1", "input", &display)
        .unwrap();
    let check = |screen: &TerminalScreen| {
        let rows = screen.normal_styled_content_lines();
        let bytes = rows
            .iter()
            .map(|row| row.copy_text.as_ref().map_or(0, String::len))
            .sum::<usize>();
        assert!(
            bytes <= display.len() + rows.len() * 128 + 1024,
            "retained copy bytes: {bytes}"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.copy_text.as_deref().is_some_and(|source| source
                    .starts_with(mez_mux::copy::COPY_SOURCE_LINE_PREFIX)
                    && source.contains("short source\r\n")))
                .count(),
            1
        );
    };
    check(service.agent_pane_screen("%1").unwrap());
    let receipt = service
        .steering_presentation_receipts("%1")
        .unwrap()
        .remove(0);
    // Use an independent settled-rendering surface with no live pending owner.
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut receipt = receipt;
    receipt.status = SteeringRecoveryStatus::Admitted(7);
    service
        .append_settled_steering_source(
            "%1",
            &crate::storage::transcript::steering::Source {
                version: 1,
                conversation_id: conversation,
                receipt,
            },
        )
        .unwrap();
    check(service.agent_pane_screen("%1").unwrap());
}

/// A near-limit multiline source retains exactly one full payload rather than
/// cloning it for every wrapped row. Both settled rendering and a clipped
/// pending suffix have source-plus-row-linear metadata, independent of width.
#[test]
fn steering_copy_metadata_is_linear_for_large_multiline_source() {
    let display = "short source\r\n".repeat(60_000);
    assert!(display.len() < mez_agent::transcript::STEERING_RECOVERY_BYTES);
    let settled = steering_source_rendered_lines("user> ", &display, 38, "occurrence");
    assert!(settled.len() >= 60_000);
    let metadata_bytes = settled
        .iter()
        .map(|line| line.copy_text.as_ref().map_or(0, String::len))
        .sum::<usize>();
    assert_eq!(
        settled
            .iter()
            .filter(|line| line
                .copy_text
                .as_deref()
                .is_some_and(|source| source.starts_with(mez_mux::copy::COPY_SOURCE_LINE_PREFIX)))
            .count(),
        1
    );
    assert!(
        metadata_bytes <= display.len() + settled.len() * 64 + 128,
        "metadata must remain source-plus-row-linear: {metadata_bytes}"
    );
    // Pending overflow bounds rows before attaching any full-source payload.
    let mut pending = wrapped_prefixed_agent_terminal_lines("user> [pending] ", &display, 38);
    pending.drain(..pending.len().saturating_sub(3));
    attach_steering_copy_source(&mut pending, &display, "overflow");
    assert_eq!(pending.len(), 3);
    let pending_bytes = pending
        .iter()
        .map(|line| line.copy_text.as_ref().map_or(0, String::len))
        .sum::<usize>();
    assert!(pending_bytes <= display.len() + pending.len() * 64 + 128);
    assert_eq!(
        pending
            .iter()
            .filter(|line| line
                .copy_text
                .as_deref()
                .is_some_and(|source| source.starts_with(mez_mux::copy::COPY_SOURCE_LINE_PREFIX)))
            .count(),
        1
    );
}
