//! Reusable pager state for record-oriented agent-shell browsers.
//!
//! The record browser keeps detail navigation, filter prompts, save
//! prompts, and raw-Markdown export data independent from issue and memory
//! backends. Command adapters provide records and later consume typed outcomes,
//! while the runtime pager can render the returned Markdown without knowing
//! whether the source is an issue, a memory, or another durable record type.

#![allow(dead_code)]

use crate::error::{MezError, Result};

/// Rendered pager content produced from browser state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeRecordBrowserPage {
    /// Human-readable browser title.
    pub title: String,
    /// Markdown to show in the pager, including transient prompt and error chrome.
    pub markdown: String,
    /// Raw Markdown to save when the save prompt is accepted.
    pub raw_markdown: String,
}

/// One record that can be shown by the shared browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeRecordBrowserRecord {
    /// Stable record id used for selection and metadata.
    pub id: String,
    /// Optional agent-shell command that opens this record's detail view.
    pub open_command: Option<String>,
    /// Short summary rendered in list rows and detail headings.
    pub title: String,
    /// Backend-specific metadata rendered above the Markdown body.
    pub metadata: Vec<(String, String)>,
    /// Raw Markdown body rendered in detail view and written by save.
    pub markdown: String,
}

/// Supported reusable filter prompts for record browsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeRecordBrowserFilterField {
    /// Defect/task-like kind filter when a backend supports it.
    Kind,
    /// Tag filter when a backend supports tags.
    Tags,
    /// Project glob filter.
    ProjectGlob,
    /// Full-text filter.
    Text,
}

/// Active modal prompt inside a record browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeRecordBrowserPrompt {
    /// Prompt collecting a filter value.
    Filter {
        /// Filter field being edited.
        field: RuntimeRecordBrowserFilterField,
        /// Current prompt input.
        input: String,
    },
    /// Prompt collecting a save destination path.
    Save {
        /// Current path input. Path completion is supplied by the caller's
        /// existing selector/path-completion surface.
        input: String,
    },
}

/// User intent decoded by the pager and applied to browser state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeRecordBrowserAction {
    /// Return from detail or prompt state to the preserved list view.
    BackToList,
    /// Begin editing one filter field.
    StartFilter(RuntimeRecordBrowserFilterField),
    /// Begin editing a save destination path.
    StartSave,
    /// Replace the active prompt text.
    EditPrompt(String),
    /// Accept the active prompt text.
    SubmitPrompt,
}

/// Typed result of applying one browser action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeRecordBrowserOutcome {
    /// Browser state changed without requiring an external side effect.
    Updated,
    /// A filter value was accepted and the caller should refresh records.
    FilterSubmitted {
        /// Filter field that changed.
        field: RuntimeRecordBrowserFilterField,
        /// Submitted value.
        value: String,
    },
    /// A save destination was accepted and the caller should write Markdown.
    SaveSubmitted {
        /// Destination path text supplied by the user.
        path: String,
        /// Markdown content to write, including the metadata table.
        markdown: String,
    },
    /// The action had no effect in the current state.
    Ignored,
}

/// Stateful list/detail browser shared by issue and memory pager commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeRecordBrowser {
    title: String,
    records: Vec<RuntimeRecordBrowserRecord>,
    active_index: usize,
    scroll_offset: usize,
    detail_index: Option<usize>,
    prompt: Option<RuntimeRecordBrowserPrompt>,
    error: Option<String>,
}

impl RuntimeRecordBrowser {
    /// Builds a browser from already-filtered records.
    pub(crate) fn new(
        title: impl Into<String>,
        records: Vec<RuntimeRecordBrowserRecord>,
    ) -> Result<Self> {
        let title = title.into();
        if title.trim().is_empty() {
            return Err(MezError::invalid_args(
                "record browser title must not be empty",
            ));
        }
        for record in &records {
            validate_browser_record(record)?;
        }
        Ok(Self {
            title,
            records,
            active_index: 0,
            scroll_offset: 0,
            detail_index: None,
            prompt: None,
            error: None,
        })
    }

    /// Replaces the pager error shown above list or detail content.
    pub(crate) fn set_error(&mut self, error: Option<String>) {
        self.error = error.filter(|value| !value.trim().is_empty());
    }

    /// Returns the currently active modal prompt, if one is open.
    pub(crate) fn prompt(&self) -> Option<&RuntimeRecordBrowserPrompt> {
        self.prompt.as_ref()
    }

    /// Returns the selected row index retained by the list view.
    pub(crate) fn active_index(&self) -> usize {
        self.active_index
    }

    /// Returns the retained list scroll offset.
    pub(crate) fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Sets the retained list scroll offset.
    pub(crate) fn set_scroll_offset(&mut self, scroll_offset: usize) {
        self.scroll_offset = scroll_offset;
    }

    /// Shows the first record as a detail page when command parsing has
    /// already resolved a concrete record id.
    pub(crate) fn show_first_record_detail(&mut self) {
        if !self.records.is_empty() {
            self.detail_index = Some(0);
        }
    }

    /// Applies one typed pager action to this browser state.
    pub(crate) fn apply_action(
        &mut self,
        action: RuntimeRecordBrowserAction,
    ) -> Result<RuntimeRecordBrowserOutcome> {
        match action {
            RuntimeRecordBrowserAction::BackToList => {
                let changed = self.detail_index.take().is_some() || self.prompt.take().is_some();
                Ok(if changed {
                    RuntimeRecordBrowserOutcome::Updated
                } else {
                    RuntimeRecordBrowserOutcome::Ignored
                })
            }
            RuntimeRecordBrowserAction::StartFilter(field) => {
                self.prompt = Some(RuntimeRecordBrowserPrompt::Filter {
                    field,
                    input: String::new(),
                });
                Ok(RuntimeRecordBrowserOutcome::Updated)
            }
            RuntimeRecordBrowserAction::StartSave => {
                self.prompt = Some(RuntimeRecordBrowserPrompt::Save {
                    input: String::new(),
                });
                Ok(RuntimeRecordBrowserOutcome::Updated)
            }
            RuntimeRecordBrowserAction::EditPrompt(input) => {
                match self.prompt.as_mut() {
                    Some(RuntimeRecordBrowserPrompt::Filter { input: current, .. })
                    | Some(RuntimeRecordBrowserPrompt::Save { input: current }) => {
                        *current = input;
                    }
                    None => return Ok(RuntimeRecordBrowserOutcome::Ignored),
                }
                Ok(RuntimeRecordBrowserOutcome::Updated)
            }
            RuntimeRecordBrowserAction::SubmitPrompt => self.submit_prompt(),
        }
    }

    /// Renders the current list, detail, or prompt state into pager content.
    pub(crate) fn render_page(&self) -> RuntimeRecordBrowserPage {
        if let Some(detail_index) = self.detail_index {
            return self.render_detail_page(detail_index);
        }
        self.render_list_page()
    }

    fn submit_prompt(&mut self) -> Result<RuntimeRecordBrowserOutcome> {
        let Some(prompt) = self.prompt.take() else {
            return Ok(RuntimeRecordBrowserOutcome::Ignored);
        };
        match prompt {
            RuntimeRecordBrowserPrompt::Filter { field, input } => {
                Ok(RuntimeRecordBrowserOutcome::FilterSubmitted {
                    field,
                    value: input.trim().to_string(),
                })
            }
            RuntimeRecordBrowserPrompt::Save { input } => {
                let path = input.trim().to_string();
                if path.is_empty() {
                    return Err(MezError::invalid_args(
                        "record browser save path must not be empty",
                    ));
                }
                Ok(RuntimeRecordBrowserOutcome::SaveSubmitted {
                    path,
                    markdown: self.render_page().raw_markdown,
                })
            }
        }
    }

    fn render_list_page(&self) -> RuntimeRecordBrowserPage {
        let raw_markdown = list_markdown(&self.title, &self.records);
        let mut markdown = String::new();
        if let Some(error) = &self.error {
            markdown.push_str(&format!("Error: {error}\n\n"));
        }
        if let Some(prompt) = &self.prompt {
            markdown.push_str(&format!("{}\n\n", prompt_line(prompt)));
        }
        markdown.push_str(&raw_markdown);
        RuntimeRecordBrowserPage {
            title: self.title.clone(),
            markdown,
            raw_markdown,
        }
    }

    fn render_detail_page(&self, detail_index: usize) -> RuntimeRecordBrowserPage {
        let record = &self.records[detail_index.min(self.records.len().saturating_sub(1))];
        let raw_markdown = detail_markdown(record);
        let mut markdown = String::new();
        if let Some(error) = &self.error {
            markdown.push_str(&format!("Error: {error}\n\n"));
        }
        if let Some(prompt) = &self.prompt {
            markdown.push_str(&format!("{}\n\n", prompt_line(prompt)));
        }
        markdown.push_str(&raw_markdown);
        RuntimeRecordBrowserPage {
            title: record.title.clone(),
            markdown,
            raw_markdown,
        }
    }
}

fn validate_browser_record(record: &RuntimeRecordBrowserRecord) -> Result<()> {
    if record.id.trim().is_empty() {
        return Err(MezError::invalid_args(
            "record browser id must not be empty",
        ));
    }
    if record.title.trim().is_empty() {
        return Err(MezError::invalid_args(
            "record browser title must not be empty",
        ));
    }
    Ok(())
}

fn prompt_line(prompt: &RuntimeRecordBrowserPrompt) -> String {
    match prompt {
        RuntimeRecordBrowserPrompt::Filter { field, input } => {
            format!("Filter {}: {}", filter_field_name(*field), input)
        }
        RuntimeRecordBrowserPrompt::Save { input } => format!("Save to: {input}"),
    }
}

fn filter_field_name(field: RuntimeRecordBrowserFilterField) -> &'static str {
    match field {
        RuntimeRecordBrowserFilterField::Kind => "kind",
        RuntimeRecordBrowserFilterField::Tags => "tags",
        RuntimeRecordBrowserFilterField::ProjectGlob => "project",
        RuntimeRecordBrowserFilterField::Text => "text",
    }
}

fn list_markdown(title: &str, records: &[RuntimeRecordBrowserRecord]) -> String {
    let mut lines = vec![format!("# {title}"), String::new()];
    if records.is_empty() {
        lines.push("No records found.".to_string());
    } else {
        for record in records {
            let label = list_record_label(record);
            if let Some(command) = record.open_command.as_deref() {
                lines.push(format!(
                    "- [`{}`](mez-agent:{})",
                    escape_markdown_link_label(&label),
                    encode_mez_agent_command(command)
                ));
            } else {
                lines.push(format!("- **{}** — {}", record.id, record.title));
            }
        }
    }
    lines.join("\n")
}

fn detail_markdown(record: &RuntimeRecordBrowserRecord) -> String {
    let mut lines = vec![format!("# {}", record.title), String::new()];
    lines.push(record.title.clone());
    lines.push(String::new());
    if !record.metadata.is_empty() {
        lines.push("| Field | Value |".to_string());
        lines.push("| --- | --- |".to_string());
        for (key, value) in &record.metadata {
            lines.push(format!(
                "| {} | {} |",
                escape_markdown_table(key),
                escape_markdown_table(value)
            ));
        }
        lines.push(String::new());
    }
    lines.push(record.markdown.clone());
    lines.join("\n")
}

fn list_record_label(record: &RuntimeRecordBrowserRecord) -> String {
    let mut label = format!("{} — {}", record.id, record.title);
    let metadata = list_metadata_summary(&record.metadata);
    if !metadata.is_empty() {
        label.push_str(" · ");
        label.push_str(&metadata);
    }
    label
}

fn list_metadata_summary(metadata: &[(String, String)]) -> String {
    metadata
        .iter()
        .filter(|(key, _)| list_metadata_key_is_prominent(key))
        .map(|(key, value)| format!("{key}: {value}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn list_metadata_key_is_prominent(key: &str) -> bool {
    matches!(
        key,
        "kind" | "state" | "project" | "scope" | "priority" | "expires_at_unix_seconds"
    )
}

fn escape_markdown_table(value: &str) -> String {
    value.replace('\\', "\\\\").replace('|', "\\|")
}

fn escape_markdown_link_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('`', "\\`")
}

fn encode_mez_agent_command(command: &str) -> String {
    let mut encoded = String::new();
    for byte in command.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser_record(id: &str, title: &str) -> RuntimeRecordBrowserRecord {
        RuntimeRecordBrowserRecord {
            id: id.to_string(),
            open_command: Some(format!("/show-issues {id}")),
            title: title.to_string(),
            metadata: vec![("project".to_string(), "/repo".to_string())],
            markdown: format!("Body for {title}"),
        }
    }

    /// Verifies the reusable browser can render command-resolved detail state
    /// and return to the list while preserving the retained scroll position.
    #[test]
    fn record_browser_returns_from_detail_with_list_state_preserved() {
        let mut browser = RuntimeRecordBrowser::new(
            "Issues",
            vec![
                browser_record("issue-1", "First"),
                browser_record("issue-2", "Second"),
            ],
        )
        .unwrap();
        browser.set_scroll_offset(4);

        browser.show_first_record_detail();
        assert!(browser.render_page().raw_markdown.contains("# First"));

        assert_eq!(
            browser
                .apply_action(RuntimeRecordBrowserAction::BackToList)
                .unwrap(),
            RuntimeRecordBrowserOutcome::Updated
        );
        assert_eq!(browser.scroll_offset(), 4);
        let list_page = browser.render_page();
        assert!(
            list_page
                .markdown
                .contains("[`issue-1 — First · project: /repo`]")
        );
        assert!(
            list_page
                .markdown
                .contains("[`issue-2 — Second · project: /repo`]")
        );
        assert!(!list_page.markdown.contains("> issue-"));
        assert!(!list_page.markdown.contains("  issue-"));
    }

    /// Verifies filter and save prompts produce typed outcomes while empty and
    /// error states render inside the same pager content model.
    #[test]
    fn record_browser_prompts_and_empty_error_states_are_typed() {
        let mut browser = RuntimeRecordBrowser::new("Memories", Vec::new()).unwrap();
        browser.set_error(Some("database unavailable".to_string()));
        let empty_page = browser.render_page();
        assert!(empty_page.markdown.contains("No records found."));
        assert!(empty_page.markdown.contains("Error: database unavailable"));

        assert_eq!(
            browser
                .apply_action(RuntimeRecordBrowserAction::StartFilter(
                    RuntimeRecordBrowserFilterField::ProjectGlob,
                ))
                .unwrap(),
            RuntimeRecordBrowserOutcome::Updated
        );
        assert!(matches!(
            browser.prompt(),
            Some(RuntimeRecordBrowserPrompt::Filter {
                field: RuntimeRecordBrowserFilterField::ProjectGlob,
                ..
            })
        ));
        browser
            .apply_action(RuntimeRecordBrowserAction::EditPrompt(
                "/repo/*".to_string(),
            ))
            .unwrap();
        assert_eq!(
            browser
                .apply_action(RuntimeRecordBrowserAction::SubmitPrompt)
                .unwrap(),
            RuntimeRecordBrowserOutcome::FilterSubmitted {
                field: RuntimeRecordBrowserFilterField::ProjectGlob,
                value: "/repo/*".to_string(),
            }
        );

        let mut browser = RuntimeRecordBrowser::new(
            "Issues",
            vec![RuntimeRecordBrowserRecord {
                id: "issue-1".to_string(),
                open_command: Some("/show-issues issue-1".to_string()),
                title: "Pipe table".to_string(),
                metadata: vec![("state".to_string(), "open|resolved".to_string())],
                markdown: "Detail body".to_string(),
            }],
        )
        .unwrap();
        browser.show_first_record_detail();
        browser
            .apply_action(RuntimeRecordBrowserAction::StartSave)
            .unwrap();
        browser
            .apply_action(RuntimeRecordBrowserAction::EditPrompt(
                "issue.md".to_string(),
            ))
            .unwrap();
        match browser
            .apply_action(RuntimeRecordBrowserAction::SubmitPrompt)
            .unwrap()
        {
            RuntimeRecordBrowserOutcome::SaveSubmitted { path, markdown } => {
                assert_eq!(path, "issue.md");
                assert!(markdown.contains("| state | open\\|resolved |"));
                assert!(markdown.contains("Detail body"));
            }
            other => panic!("expected save outcome, got {other:?}"),
        }
    }
}
