//! Date presentation through the production record/browser projections.
//!
//! UTC labels are display evidence only. Numeric storage, sequence ordering and
//! lifecycle authority remain unchanged by list/detail/copy/save projection.

use super::*;

/// Epoch zero is a real UTC instant; missing approval evidence remains unknown,
/// and values beyond the four-digit RFC3339 year range are explicitly unavailable.
#[test]
fn show_record_timestamp_range_and_optional_approval_are_truthful() {
    assert_eq!(record_timestamp_display(0), "1970-01-01T00:00:00Z");
    assert_eq!(
        record_timestamp_display(253_402_300_799),
        "9999-12-31T23:59:59Z"
    );
    for value in [253_402_300_800, u64::MAX] {
        assert_eq!(
            record_timestamp_display(value),
            "unavailable (outside RFC3339 range)"
        );
    }
    let mut approval = mez_agent::permissions::BlockedApprovalRequest {
        id: "approval".into(),
        requesting_agent_id: "agent".into(),
        pane_id: "%1".into(),
        parent_agent_chain: Vec::new(),
        action_kind: "shell_command".into(),
        action_summary: "bounded action".into(),
        declared_effects: Vec::new(),
        matched_rules: Vec::new(),
        read_scopes: Vec::new(),
        write_scopes: Vec::new(),
        cooperation_mode: None,
        created_at_unix_seconds: None,
        decided_at_unix_seconds: None,
        decided_by_client_id: None,
        state: mez_agent::permissions::BlockedApprovalState::Pending,
        decision: None,
        redirect_instruction: None,
    };
    for (instant, expected) in [
        (None, "unknown"),
        (Some(0), "1970-01-01T00:00:00Z"),
        (Some(u64::MAX), "unavailable (outside RFC3339 range)"),
    ] {
        approval.created_at_unix_seconds = instant;
        let record = approval_browser_record(&approval);
        assert!(
            record
                .metadata
                .iter()
                .any(|(key, value)| key == "Created" && value == expected)
        );
    }
}

/// Context, issues and memories expose readable Created/Updated values in their
/// existing list columns and detail/export metadata. Distinct source instants
/// remain distinct, and the original records keep numeric timestamps.
#[test]
fn show_record_browsers_render_created_and_updated_as_utc() {
    let context = context_browser_record(mez_agent::transcript::TranscriptEntry {
        conversation_id: "conversation".into(),
        sequence: 1,
        created_at_unix_seconds: 1_704_067_200,
        role: mez_agent::transcript::TranscriptRole::User,
        turn_id: "turn".into(),
        agent_id: "agent".into(),
        pane_id: "%1".into(),
        content: "context source".into(),
    });
    let issue = mez_agent::issues::IssueRecord {
        id: "issue".into(),
        project: "/repo".into(),
        kind: mez_agent::issues::IssueKind::Defect,
        state: mez_agent::issues::IssueState::Open,
        priority: 50,
        title: "issue source".into(),
        body: None,
        notes: None,
        depends_on: Vec::new(),
        created_at_unix_seconds: 1_704_067_200,
        updated_at_unix_seconds: 1_704_153_600,
    };
    let memory = mez_agent::memory::MemoryRecord::new_with_defaults(
        "memory",
        mez_agent::memory::MemoryScope::Global,
        1_704_067_200,
        1_704_153_600,
        mez_agent::memory::MemorySource::User,
        50,
        "memory source",
    );
    let mut context_browser = RecordBrowser::new("Context", vec![context], Vec::new()).unwrap();
    configure_context_record_browser(&mut context_browser);
    let mut issues = RecordBrowser::new(
        "Issues",
        vec![issue_browser_record(issue.clone())],
        Vec::new(),
    )
    .unwrap();
    configure_issue_record_browser(&mut issues, false);
    let mut memories = RecordBrowser::new(
        "Memories",
        vec![memory_browser_record(memory.clone())],
        Vec::new(),
    )
    .unwrap();
    configure_memory_record_browser(&mut memories);
    for (mut browser, list_date) in [
        (context_browser, "2024-01-01T00:00:00Z"),
        (issues, "2024-01-02T00:00:00Z"),
        (memories, "2024-01-02T00:00:00Z"),
    ] {
        let list = browser.render_page().raw_markdown;
        assert!(list.contains(list_date), "{list}");
        assert!(
            !list.contains("1704067200") && !list.contains("1704153600"),
            "{list}"
        );
        browser.show_first_record_detail();
        let detail = browser.render_page().raw_markdown;
        assert!(detail.contains("2024-01-01T00:00:00Z"), "{detail}");
        assert!(!detail.contains("1704067200"), "{detail}");
    }
    assert_eq!(issue.created_at_unix_seconds, 1_704_067_200);
    assert_eq!(memory.updated_at_unix_seconds, 1_704_153_600);
}
