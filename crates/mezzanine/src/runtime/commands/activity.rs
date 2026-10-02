//! View-only activity disclosure through the existing client-local record browser.
//!
//! Reads occur only in an explicit command (off-actor in attached operation),
//! never during rendering. Opening details changes browser navigation, not the
//! conversation screen, canonical history, execution state or logging level.
//! Legacy records without explicit activity identities remain ungrouped.

use super::show_records::RuntimeContextBrowserRead;
use crate::error::{MezError, Result};
use crate::storage::transcript::{
    AgentTranscriptStore,
    activity::{ACTIVITY_CONTENT_TYPE, ActivitySource},
};
use mez_mux::record_browser::{RecordBrowser, RecordBrowserAction, RecordBrowserRecord};

/// Builds a bounded recent-activity snapshot; sequence selection names an exact
/// durable component, never a guessed action id or adjacent timestamp.
pub(super) fn read_activity_browser(
    store: &AgentTranscriptStore,
    conversation: &str,
    _pane: &str,
    args: &str,
) -> Result<RuntimeContextBrowserRead> {
    let args = args.split_whitespace().collect::<Vec<_>>();
    let detail = match args.as_slice() {
        ["activity"] => None,
        ["activity", sequence] => Some(
            sequence
                .parse::<u64>()
                .map_err(|_| MezError::invalid_args("activity sequence must be an integer"))?,
        ),
        _ => {
            return Err(MezError::invalid_args(
                "use /show-context activity [presentation-sequence]",
            ));
        }
    };
    let entries = store.inspect_recent_presentation(conversation, 200, 8 * 1024 * 1024)?;
    let mut records: Vec<RecordBrowserRecord> = Vec::new();
    let mut groups = std::collections::BTreeMap::new();
    let mut sequences = std::collections::BTreeMap::new();
    let mut exports: std::collections::BTreeMap<String, Vec<(u64, ActivitySource)>> =
        std::collections::BTreeMap::new();
    for entry in entries {
        // Pane ids describe historical producers, not current attachments.
        // The store query and envelope bind disclosure to the conversation.
        if entry.source_content_type.as_deref() != Some(ACTIVITY_CONTENT_TYPE) {
            continue;
        }
        let activity = ActivitySource::decode(entry.source_text.as_deref().unwrap_or_default())?;
        if entry.conversation_id != conversation
            || activity.conversation_id != conversation
            || entry.turn_id.as_deref() != Some(activity.turn_id.as_str())
        {
            return Err(MezError::invalid_args(
                "activity browser source differs from owner",
            ));
        }
        let body = if activity.content_type
            == "application/vnd.mezzanine.agent-presentation.styled-lines+json; charset=utf-8"
        {
            let lines: Vec<(String, String)> = serde_json::from_str(&activity.source)
                .map_err(|_| MezError::invalid_args("activity styled source is malformed"))?;
            lines
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            activity.source.clone()
        };
        let mut detail = String::new();
        for (label, source) in [
            ("Accepted rationale", &activity.intent.rationale),
            ("Accepted summary", &activity.intent.summary),
            (
                "Command intent (not execution evidence)",
                &activity.intent.command,
            ),
            ("Action header", &activity.intent.header),
        ] {
            if let Some(source) = source.as_deref().filter(|source| !source.is_empty()) {
                detail.push_str(&format!(
                    "## {label}\n\n{}\n\n",
                    literal_activity_source(source)
                ));
            }
        }
        detail.push_str(&format!(
            "## {}\n\n{}",
            activity_component_heading(activity.kind),
            literal_activity_source(&body)
        ));
        if let Some(mutation) = activity.mutation.as_ref() {
            detail.push_str(&format!("\n\n## Confirmed section {}\n\n{}\n\nConfirmation applies only to this section, not whole-action success.",
                mutation.section_index, literal_activity_source(&mutation.path)));
        }
        let identity = (
            activity.turn_id.clone(),
            activity.response_id.clone(),
            activity.action_id.clone(),
            activity.action_ordinal,
            activity.transaction.clone(),
        );
        if let Some(&index) = groups.get(&identity) {
            let record: &mut RecordBrowserRecord = &mut records[index];
            exports
                .entry(record.id.clone())
                .or_default()
                .push((entry.sequence, activity.clone()));
            // Source order is durable sequence order, not worker arrival or a
            // status severity guess. A later component retains its own status.
            record.markdown.push_str(&format!(
                "\n\n## Component {} · {:?} · {}\n\n{}",
                entry.sequence,
                activity.kind,
                activity.status,
                literal_activity_source(&body)
            ));
            if let Some(mutation) = activity.mutation.as_ref() {
                record.markdown.push_str(&format!(
                    "\n\nConfirmed section {} (not whole-action success):\n\n{}",
                    mutation.section_index,
                    literal_activity_source(&mutation.path)
                ));
            }
            if let Some((_, status)) = record.metadata.iter_mut().find(|(key, _)| key == "status") {
                *status = activity.status.clone();
            }
            record.title = format!(
                "{} · {}",
                activity.action_id.as_deref().unwrap_or("response"),
                activity.status
            );
            sequences.insert(entry.sequence, record.id.clone());
            continue;
        }
        groups.insert(identity, records.len());
        sequences.insert(entry.sequence, entry.sequence.to_string());
        exports.insert(
            entry.sequence.to_string(),
            vec![(entry.sequence, activity.clone())],
        );
        records.push(RecordBrowserRecord {
            id: entry.sequence.to_string(),
            open_command: Some(format!("/show-context activity {}", entry.sequence)),
            title: format!(
                "{} · {:?} · {}",
                activity.action_id.as_deref().unwrap_or("response"),
                activity.kind,
                activity.status
            ),
            metadata: vec![
                ("turn".into(), activity.turn_id),
                ("response".into(), activity.response_id),
                ("action".into(), activity.action_id.unwrap_or_default()),
                (
                    "ordinal".into(),
                    activity
                        .action_ordinal
                        .map_or_else(String::new, |value| value.to_string()),
                ),
                ("status".into(), activity.status),
                (
                    "transaction".into(),
                    activity
                        .transaction
                        .unwrap_or_else(|| "not recorded".into()),
                ),
            ],
            markdown: detail,
        });
    }
    let mut browser = RecordBrowser::new("Retained activity", records, Vec::new())?;
    for (id, components) in exports {
        let source =
            serde_json::to_string(&serde_json::json!({"version": 1, "components": components}))
                .map_err(|error| {
                    MezError::invalid_args(format!("activity export encoding failed: {error}"))
                })?;
        browser.set_record_copy_source(&id, source);
    }
    browser.set_table_id_column("Activity");
    browser.set_table_columns_with_labels(vec![
        ("Action".into(), "action".into()),
        ("Status".into(), "status".into()),
        ("Turn".into(), "turn".into()),
    ]);
    browser.set_help(Some("Enter or click opens retained detail · Esc back · y exports exact activity JSON · snapshot limited to latest 200 records / 8 MiB; legacy ungrouped rows omitted".into()), Some("Esc returns to list without altering logs · y exports exact activity JSON".into()));
    browser.set_empty_message(Some("No identity-bearing activity retained in this snapshot. Legacy rows remain in the conversation/context views.".into()));
    if let Some(sequence) = detail {
        if !sequences
            .get(&sequence)
            .is_some_and(|id| browser.set_active_record_id(id))
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "activity component is outside the retained snapshot",
            ));
        }
        browser.apply_action(RecordBrowserAction::OpenActive)?;
    }
    let markdown = browser.render_page().raw_markdown;
    Ok(RuntimeContextBrowserRead {
        browser,
        source: None,
        markdown,
    })
}

/// Returns a disclosure heading from producer-owned semantics, never status
/// guesses or source text. Accepted intent is not represented as a result.
fn activity_component_heading(
    kind: crate::storage::transcript::activity::ActivityComponentKind,
) -> &'static str {
    use crate::storage::transcript::activity::ActivityComponentKind;
    match kind {
        ActivityComponentKind::Rationale => "Accepted rationale",
        ActivityComponentKind::Summary => "Accepted action summary",
        ActivityComponentKind::Command => "Accepted command intent",
        ActivityComponentKind::Header => "Accepted action header",
        ActivityComponentKind::Result => "Retained result",
        ActivityComponentKind::Outcome => "Observed outcome",
        ActivityComponentKind::ConfirmedMutation => "Confirmed mutation evidence",
    }
}

/// Renders untrusted retained bytes literally with a fence longer than any
/// source backtick run. Payload text cannot create view controls or headings.
fn literal_activity_source(source: &str) -> String {
    let fence = "`".repeat(
        source
            .split(|ch| ch != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .max(2)
            + 1,
    );
    format!("{fence}text\n{source}\n{fence}")
}
