//! Real manual input admission after retirement of the flat row renderer.
//!
//! Accounting and audit are qualified by the ordinary history owner before
//! canonical selection. These regressions exercise /compact rather than keeping
//! a test-only copy of the retired auxiliary rendering algorithm.

use super::*;
use crate::storage::transcript::AgentTranscriptStore;

/// Captures the actual queued request without invoking a provider. Original
/// rows are written once; cleanup follows all dropped runtime/storage owners.
fn request(rows: &[TranscriptEntry]) -> Result<ModelRequest> {
    let root = std::env::temp_dir().join(format!(
        "mez-canonical-input-{:032x}",
        rand::random::<u128>()
    ));
    let result = (|| {
        let store = AgentTranscriptStore::new(root.clone());
        for row in rows {
            store.append(row)?;
        }
        let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
        service.replace_config_layers(vec![crate::config::ConfigLayer {
            name: "canonical-input".into(),
            path: None,
            format: crate::config::ConfigFormat::Toml,
            scope: crate::config::ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider="openai"
default_model_profile="default"
compaction_raw_retention_percent=1
[providers.openai]
kind="openai"
models=["test"]
default_model="test"
[model_profiles.default]
provider="openai"
model="test"
context_window_tokens=40000
"#
            .into(),
        }])?;
        service.set_agent_transcript_store(store);
        let primary =
            service.attach_primary("input", true, mez_mux::layout::Size::new(80, 24)?, 1)?;
        service.agent_shell_store_mut().enter_or_resume("%1")?;
        service.agent_shell_store_mut().bind_conversation(
            "%1",
            &rows[0].conversation_id,
            rows.len() as u64,
        )?;
        let response = service.execute_agent_shell_command(&primary, "/compact")?;
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .ok_or_else(|| {
                MezError::invalid_state(format!("canonical source did not queue: {response}"))
            })?;
        assert!(task.replay_source.is_some());
        assert!(service.agent_turn_ledger().turns().is_empty());
        let mut request = task.request.clone();
        let mut messages = request.messages.iter().cloned().collect::<Vec<_>>();
        messages.last_mut().unwrap().content =
            crate::test_support::compaction_source::rendered_source(task)?;
        request.messages = messages.into();
        Ok(request)
    })();
    if root.exists() {
        std::fs::remove_dir_all(root)?;
    }
    result
}

fn row(sequence: u64, role: TranscriptRole, content: String) -> TranscriptEntry {
    TranscriptEntry {
        conversation_id: "canonical-input".into(),
        sequence,
        created_at_unix_seconds: 1,
        role,
        turn_id: format!("original-{sequence}"),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content,
    }
}

/// A terminal certificate without its claimed original source must refuse;
/// inherited audit is inert. Neither case may expose audit accounting as model
/// source. User-quoted markers remain literal, separately authored input.
#[test]
fn compactor_request_excludes_terminal_and_inherited_accounting_audit() {
    use crate::storage::transcript::compaction_handoff::{
        INHERITED_MARKER, MARKER, TerminalCompactionHandoff, TerminalCompactionReplacement,
    };
    let block = ContextBlock::assistant_event("audit source", "AUDIT_ONLY_SOURCE");
    let record = mez_agent::ContextBlockRecord::allocate(&block).unwrap();
    let event = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
        block.source,
        block.label,
        block.content,
        mez_agent::ContextExecutionGroupId::new("audit-source").unwrap(),
        1,
        None,
    )
    .unwrap();
    let encoded = mez_agent::transcript::TranscriptContextRecord::new(event, record, None)
        .unwrap()
        .to_transcript_content()
        .unwrap();
    let certificate = TerminalCompactionHandoff {
        baseline: None,
        replacements: vec![TerminalCompactionReplacement {
            summary_record: None,
            sources: vec![encoded],
            summary: "AUDIT_ONLY_SUMMARY".into(),
        }],
    }
    .to_content()
    .unwrap();
    let occurrence = String::from(record.occurrence_id());
    let visible = row(1, TranscriptRole::User, "VISIBLE_COMPACTOR_SOURCE".into());
    assert!(
        request(&[
            visible.clone(),
            row(2, TranscriptRole::System, certificate.clone())
        ])
        .is_err()
    );
    let inherited = certificate.replacen(MARKER, INHERITED_MARKER, 1);
    let body = mez_agent::openai_responses_request_body(
        &request(&[
            visible.clone(),
            row(2, TranscriptRole::System, inherited.clone()),
        ])
        .unwrap(),
    )
    .unwrap();
    assert!(body.contains("VISIBLE_COMPACTOR_SOURCE"));
    for hidden in [
        occurrence.as_str(),
        "AUDIT_ONLY_SOURCE",
        "AUDIT_ONLY_SUMMARY",
        "rendered_sha256",
        "mez-transcript-context-record/",
    ] {
        assert!(!body.contains(hidden), "{hidden}");
    }
    for content in [certificate, inherited] {
        // Keep this actual quoted occurrence outside the optional raw-tail
        // allowance, so the test checks selected input rather than a later tail.
        let quoted = format!("{}\n{content}", "literal quoted audit ".repeat(1_000));
        let body = mez_agent::openai_responses_request_body(
            &request(&[visible.clone(), row(2, TranscriptRole::User, quoted)]).unwrap(),
        )
        .unwrap();
        assert!(body.contains(content.lines().next().unwrap()));
    }
}

/// Malformed/future accounting refuses before provider dispatch, while the same
/// bytes under an ordinary user producer remain authored literal source.
#[test]
fn compactor_request_rejects_invalid_accounting_and_preserves_literal_user_text() {
    for content in [
        "[mez-transcript-context-record/v1]\n{}",
        "[mez-transcript-context-record/v999]\n{}",
    ] {
        assert!(request(&[row(1, TranscriptRole::System, content.into())]).is_err());
        let request = request(&[row(1, TranscriptRole::User, content.into())]).unwrap();
        assert!(request.messages.last().unwrap().content.contains(content));
    }
}
