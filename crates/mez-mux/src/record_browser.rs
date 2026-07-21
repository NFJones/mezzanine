//! Reusable pager state for record-oriented agent-shell browsers.
//!
//! The record browser keeps detail navigation, filter prompts, save
//! prompts, and raw-Markdown export data independent from issue and memory
//! backends. Command adapters provide records and later consume typed outcomes,
//! while the runtime pager can render the returned Markdown without knowing
//! whether the source is an issue, a memory, or another durable record type.
use crate::{MuxError, Result};

/// Rendered pager content produced from browser state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordBrowserPage {
    /// Human-readable browser title.
    pub title: String,
    /// Markdown to show in the pager, including transient prompt and error chrome.
    pub markdown: String,
    /// Raw Markdown to save when the save prompt is accepted.
    pub raw_markdown: String,
}

/// One record that can be shown by the shared browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordBrowserRecord {
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

/// One selectable choice exposed by a record-browser filter selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordBrowserFilterChoice {
    /// Human-readable label shown in the selector list.
    pub label: String,
    /// Submitted filter value applied when the choice is accepted.
    pub value: String,
}

/// Supported reusable filter prompts for record browsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordBrowserFilterField {
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
pub enum RecordBrowserPrompt {
    /// Prompt collecting a filter value.
    Filter {
        /// Filter field being edited.
        field: RecordBrowserFilterField,
        /// Current prompt input.
        input: String,
    },
    /// Selector collecting one available kind filter choice.
    KindSelector {
        /// Available choices in display and selection order.
        options: Vec<RecordBrowserFilterChoice>,
        /// Zero-based active selector row.
        active_index: usize,
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
pub enum RecordBrowserAction {
    /// Return from detail or prompt state to the preserved list view.
    BackToList,
    /// Open the active list record as an in-browser detail view.
    OpenActive,
    /// Begin editing one filter field.
    StartFilter(RecordBrowserFilterField),
    /// Begin editing a save destination path.
    StartSave,
    /// Request deletion of the active record through the owning backend.
    DeleteActive,
    /// Move the active selector row inside the open kind prompt.
    MovePromptSelection(isize),
    /// Jump to the first selector row inside the open kind prompt.
    SelectPromptFirst,
    /// Jump to the last selector row inside the open kind prompt.
    SelectPromptLast,
    /// Replace the active prompt text.
    EditPrompt(String),
    /// Accept the active prompt text.
    SubmitPrompt,
}

/// Selector row metadata for a rendered record-browser prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordBrowserPromptSelection {
    /// Zero-based rendered line where the selector rows begin.
    pub start_line: usize,
    /// Number of selector rows rendered for the prompt.
    pub option_count: usize,
    /// Zero-based active selector row inside that rendered range.
    pub active_index: usize,
}

/// Typed result of applying one browser action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordBrowserOutcome {
    /// Browser state changed without requiring an external side effect.
    Updated,
    /// A filter value was accepted and the caller should refresh records.
    FilterSubmitted {
        /// Filter field that changed.
        field: RecordBrowserFilterField,
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
    /// The caller should delete one stable record id and refresh the browser.
    DeleteSubmitted {
        /// Stable backend record id selected for deletion.
        id: String,
    },
    /// The action had no effect in the current state.
    Ignored,
}

/// Stateful list/detail browser shared by issue and memory pager commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordBrowser {
    title: String,
    scope_indicator: Option<String>,
    records: Vec<RecordBrowserRecord>,
    kind_filter_choices: Vec<RecordBrowserFilterChoice>,
    table_columns: Vec<String>,
    list_help: Option<String>,
    detail_help: Option<String>,
    empty_message: Option<String>,
    deletion_enabled: bool,
    active_index: usize,
    scroll_offset: usize,
    detail_index: Option<usize>,
    prompt: Option<RecordBrowserPrompt>,
    error: Option<String>,
}

impl RecordBrowser {
    /// Builds a browser from already-filtered records.
    pub fn new(
        title: impl Into<String>,
        records: Vec<RecordBrowserRecord>,
        kind_filter_choices: Vec<RecordBrowserFilterChoice>,
    ) -> Result<Self> {
        let title = title.into();
        if title.trim().is_empty() {
            return Err(MuxError::invalid_args(
                "record browser title must not be empty",
            ));
        }
        for record in &records {
            validate_browser_record(record)?;
        }
        for choice in &kind_filter_choices {
            if choice.label.trim().is_empty() {
                return Err(MuxError::invalid_args(
                    "record browser filter choice labels must not be empty",
                ));
            }
        }
        Ok(Self {
            title,
            scope_indicator: None,
            records,
            kind_filter_choices,
            table_columns: Vec::new(),
            list_help: None,
            detail_help: None,
            empty_message: None,
            deletion_enabled: false,
            active_index: 0,
            scroll_offset: 0,
            detail_index: None,
            prompt: None,
            error: None,
        })
    }

    /// Replaces the pager error shown above list or detail content.
    pub fn set_error(&mut self, error: Option<String>) {
        self.error = error.filter(|value| !value.trim().is_empty());
    }

    /// Replaces the scope label rendered near the browser title.
    pub fn set_scope_indicator(&mut self, scope_indicator: Option<String>) {
        self.scope_indicator = scope_indicator.filter(|value| !value.trim().is_empty());
    }

    /// Selects metadata fields rendered as columns in the list view.
    pub fn set_table_columns(&mut self, columns: Vec<String>) {
        self.table_columns = columns
            .into_iter()
            .filter(|column| !column.trim().is_empty())
            .collect();
    }

    /// Replaces the default list and detail key guidance.
    pub fn set_help(&mut self, list_help: Option<String>, detail_help: Option<String>) {
        self.list_help = list_help.filter(|value| !value.trim().is_empty());
        self.detail_help = detail_help.filter(|value| !value.trim().is_empty());
    }

    /// Replaces the default empty-list message.
    pub fn set_empty_message(&mut self, empty_message: Option<String>) {
        self.empty_message = empty_message.filter(|value| !value.trim().is_empty());
    }

    /// Returns the currently active modal prompt, if one is open.
    pub fn prompt(&self) -> Option<&RecordBrowserPrompt> {
        self.prompt.as_ref()
    }

    /// Returns rendered selector-row metadata for the active kind prompt.
    pub fn prompt_selection(&self) -> Option<RecordBrowserPromptSelection> {
        let RecordBrowserPrompt::KindSelector {
            options,
            active_index,
        } = self.prompt.as_ref()?
        else {
            return None;
        };
        Some(RecordBrowserPromptSelection {
            start_line: usize::from(self.error.is_some()) * 2 + 1,
            option_count: options.len(),
            active_index: (*active_index).min(options.len().saturating_sub(1)),
        })
    }

    /// Returns the selected row index retained by the list view.
    pub fn active_index(&self) -> usize {
        self.active_index
    }

    /// Returns the stable id of the active list record.
    pub fn active_record_id(&self) -> Option<&str> {
        self.records
            .get(self.active_index)
            .map(|record| record.id.as_str())
    }

    /// Selects one bounded list record by index.
    pub fn set_active_index(&mut self, active_index: usize) {
        self.active_index = active_index.min(self.records.len().saturating_sub(1));
    }

    /// Selects the record with one stable backend id when it is present.
    pub fn set_active_record_id(&mut self, record_id: &str) -> bool {
        let Some(index) = self
            .records
            .iter()
            .position(|record| record.id == record_id)
        else {
            return false;
        };
        self.active_index = index;
        true
    }

    /// Enables destructive deletion intent for browsers whose backend supports it.
    pub fn enable_deletion(&mut self) {
        self.deletion_enabled = true;
    }

    /// Reports whether the current browser exposes destructive deletion.
    pub fn deletion_enabled(&self) -> bool {
        self.deletion_enabled
    }

    /// Returns the retained list scroll offset.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Sets the retained list scroll offset.
    pub fn set_scroll_offset(&mut self, scroll_offset: usize) {
        self.scroll_offset = scroll_offset;
    }

    /// Shows the first record as a detail page when command parsing has
    /// already resolved a concrete record id.
    pub fn show_first_record_detail(&mut self) {
        if !self.records.is_empty() {
            self.detail_index = Some(0);
        }
    }

    /// Applies one typed pager action to this browser state.
    pub fn apply_action(&mut self, action: RecordBrowserAction) -> Result<RecordBrowserOutcome> {
        match action {
            RecordBrowserAction::OpenActive => {
                if self.records.is_empty() {
                    return Ok(RecordBrowserOutcome::Ignored);
                }
                self.detail_index =
                    Some(self.active_index.min(self.records.len().saturating_sub(1)));
                Ok(RecordBrowserOutcome::Updated)
            }
            RecordBrowserAction::BackToList => {
                let changed = self.detail_index.take().is_some() || self.prompt.take().is_some();
                Ok(if changed {
                    RecordBrowserOutcome::Updated
                } else {
                    RecordBrowserOutcome::Ignored
                })
            }
            RecordBrowserAction::StartFilter(field) => {
                self.prompt = Some(
                    if field == RecordBrowserFilterField::Kind
                        && !self.kind_filter_choices.is_empty()
                    {
                        RecordBrowserPrompt::KindSelector {
                            options: self.kind_filter_choices.clone(),
                            active_index: 0,
                        }
                    } else {
                        RecordBrowserPrompt::Filter {
                            field,
                            input: String::new(),
                        }
                    },
                );
                Ok(RecordBrowserOutcome::Updated)
            }
            RecordBrowserAction::StartSave => {
                self.prompt = Some(RecordBrowserPrompt::Save {
                    input: String::new(),
                });
                Ok(RecordBrowserOutcome::Updated)
            }
            RecordBrowserAction::DeleteActive => {
                if !self.deletion_enabled {
                    return Ok(RecordBrowserOutcome::Ignored);
                }
                let Some(record) = self.records.get(self.active_index) else {
                    return Ok(RecordBrowserOutcome::Ignored);
                };
                Ok(RecordBrowserOutcome::DeleteSubmitted {
                    id: record.id.clone(),
                })
            }
            RecordBrowserAction::MovePromptSelection(delta) => {
                Ok(self.move_prompt_selection(delta))
            }
            RecordBrowserAction::SelectPromptFirst => Ok(self.select_prompt_first()),
            RecordBrowserAction::SelectPromptLast => Ok(self.select_prompt_last()),
            RecordBrowserAction::EditPrompt(input) => {
                match self.prompt.as_mut() {
                    Some(RecordBrowserPrompt::Filter { input: current, .. })
                    | Some(RecordBrowserPrompt::Save { input: current }) => {
                        *current = input;
                    }
                    Some(RecordBrowserPrompt::KindSelector { .. }) => {
                        return Ok(RecordBrowserOutcome::Ignored);
                    }
                    None => return Ok(RecordBrowserOutcome::Ignored),
                }
                Ok(RecordBrowserOutcome::Updated)
            }
            RecordBrowserAction::SubmitPrompt => self.submit_prompt(),
        }
    }

    /// Renders the current list, detail, or prompt state into pager content.
    pub fn render_page(&self) -> RecordBrowserPage {
        if let Some(detail_index) = self.detail_index {
            return self.render_detail_page(detail_index);
        }
        self.render_list_page()
    }

    fn submit_prompt(&mut self) -> Result<RecordBrowserOutcome> {
        let Some(prompt) = self.prompt.take() else {
            return Ok(RecordBrowserOutcome::Ignored);
        };
        match prompt {
            RecordBrowserPrompt::Filter { field, input } => {
                Ok(RecordBrowserOutcome::FilterSubmitted {
                    field,
                    value: input.trim().to_string(),
                })
            }
            RecordBrowserPrompt::KindSelector {
                mut options,
                active_index,
            } => {
                let selected = options.drain(..).nth(active_index).ok_or_else(|| {
                    MuxError::invalid_state("record browser kind selector is empty")
                })?;
                Ok(RecordBrowserOutcome::FilterSubmitted {
                    field: RecordBrowserFilterField::Kind,
                    value: selected.value,
                })
            }
            RecordBrowserPrompt::Save { input } => {
                let path = input.trim().to_string();
                if path.is_empty() {
                    return Err(MuxError::invalid_args(
                        "record browser save path must not be empty",
                    ));
                }
                Ok(RecordBrowserOutcome::SaveSubmitted {
                    path,
                    markdown: self.render_page().raw_markdown,
                })
            }
        }
    }

    fn render_list_page(&self) -> RecordBrowserPage {
        let raw_markdown = list_markdown(self);
        let mut markdown = String::new();
        if let Some(error) = &self.error {
            markdown.push_str(&format!("Error: {error}\n\n"));
        }
        if let Some(prompt) = &self.prompt {
            markdown.push_str(&format!("{}\n\n", prompt_block(prompt)));
        }
        markdown.push_str(&raw_markdown);
        RecordBrowserPage {
            title: self.title.clone(),
            markdown,
            raw_markdown,
        }
    }

    fn render_detail_page(&self, detail_index: usize) -> RecordBrowserPage {
        let record = &self.records[detail_index.min(self.records.len().saturating_sub(1))];
        let raw_markdown = detail_markdown(
            record,
            self.scope_indicator.as_deref(),
            self.detail_help.as_deref(),
            self.deletion_enabled,
            !self.kind_filter_choices.is_empty(),
        );
        let mut markdown = String::new();
        if let Some(error) = &self.error {
            markdown.push_str(&format!("Error: {error}\n\n"));
        }
        if let Some(prompt) = &self.prompt {
            markdown.push_str(&format!("{}\n\n", prompt_block(prompt)));
        }
        markdown.push_str(&raw_markdown);
        RecordBrowserPage {
            title: record.title.clone(),
            markdown,
            raw_markdown,
        }
    }

    /// Moves the active kind-selector row by one bounded step.
    fn move_prompt_selection(&mut self, delta: isize) -> RecordBrowserOutcome {
        let Some(RecordBrowserPrompt::KindSelector {
            options,
            active_index,
        }) = self.prompt.as_mut()
        else {
            return RecordBrowserOutcome::Ignored;
        };
        if options.is_empty() {
            return RecordBrowserOutcome::Ignored;
        }
        *active_index = step_selector_index(*active_index, options.len(), delta);
        RecordBrowserOutcome::Updated
    }

    /// Jumps to the first available kind-selector row.
    fn select_prompt_first(&mut self) -> RecordBrowserOutcome {
        let Some(RecordBrowserPrompt::KindSelector {
            options,
            active_index,
        }) = self.prompt.as_mut()
        else {
            return RecordBrowserOutcome::Ignored;
        };
        if options.is_empty() {
            return RecordBrowserOutcome::Ignored;
        }
        *active_index = 0;
        RecordBrowserOutcome::Updated
    }

    /// Jumps to the last available kind-selector row.
    fn select_prompt_last(&mut self) -> RecordBrowserOutcome {
        let Some(RecordBrowserPrompt::KindSelector {
            options,
            active_index,
        }) = self.prompt.as_mut()
        else {
            return RecordBrowserOutcome::Ignored;
        };
        if options.is_empty() {
            return RecordBrowserOutcome::Ignored;
        }
        *active_index = options.len().saturating_sub(1);
        RecordBrowserOutcome::Updated
    }
}

fn validate_browser_record(record: &RecordBrowserRecord) -> Result<()> {
    if record.id.trim().is_empty() {
        return Err(MuxError::invalid_args(
            "record browser id must not be empty",
        ));
    }
    if record.title.trim().is_empty() {
        return Err(MuxError::invalid_args(
            "record browser title must not be empty",
        ));
    }
    Ok(())
}

fn prompt_block(prompt: &RecordBrowserPrompt) -> String {
    match prompt {
        RecordBrowserPrompt::Filter { field, input } => {
            format!("Filter {}: {}", filter_field_name(*field), input)
        }
        RecordBrowserPrompt::KindSelector { options, .. } => {
            std::iter::once("Filter kind:".to_string())
                .chain(options.iter().map(|option| format!("  {}", option.label)))
                .collect::<Vec<_>>()
                .join("\n")
        }
        RecordBrowserPrompt::Save { input } => format!("Save to: {input}"),
    }
}

fn step_selector_index(active: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let max = len.saturating_sub(1);
    active.saturating_add_signed(delta).min(max)
}

fn filter_field_name(field: RecordBrowserFilterField) -> &'static str {
    match field {
        RecordBrowserFilterField::Kind => "kind",
        RecordBrowserFilterField::Tags => "tags",
        RecordBrowserFilterField::ProjectGlob => "project",
        RecordBrowserFilterField::Text => "text",
    }
}

fn list_markdown(browser: &RecordBrowser) -> String {
    let mut lines = vec![format!("# {}", browser.title), String::new()];
    if let Some(scope_indicator) = browser.scope_indicator.as_deref() {
        lines.push(format!("**Scope:** {scope_indicator}"));
        lines.push(String::new());
    }
    lines.push(browser.list_help.as_deref().map(str::to_string).unwrap_or_else(|| if browser.deletion_enabled && browser.kind_filter_choices.is_empty() {
        "**Keys:** `Enter` open · `d` delete · `/` search · `s` save".to_string()
    } else if browser.deletion_enabled {
        "**Keys:** `a` all/default scope · `k` kind · `p` project · `x` text · `d` delete · `s` save"
            .to_string()
    } else {
        "**Keys:** `a` all/default scope · `k` kind · `p` project · `x` text · `s` save".to_string()
    }));
    lines.push(String::new());
    if browser.records.is_empty() {
        lines.push(
            browser
                .empty_message
                .as_deref()
                .unwrap_or("No records found.")
                .to_string(),
        );
    } else if !browser.table_columns.is_empty() {
        lines.push(format!(
            "| Approval | {} |",
            browser.table_columns.join(" | ")
        ));
        lines.push(format!(
            "| --- | {} |",
            browser
                .table_columns
                .iter()
                .map(|_| "---")
                .collect::<Vec<_>>()
                .join(" | ")
        ));
        for record in &browser.records {
            let id = if let Some(command) = record.open_command.as_deref() {
                format!(
                    "[`{}`](mez-agent:{})",
                    escape_markdown_link_label(&record.id),
                    encode_mez_agent_command(command)
                )
            } else {
                format!("**{}**", escape_markdown_table(&record.id))
            };
            let values = browser
                .table_columns
                .iter()
                .map(|column| {
                    record
                        .metadata
                        .iter()
                        .find(|(key, _)| key == column)
                        .map(|(_, value)| escape_markdown_table(value))
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(" | ");
            lines.push(format!("| {id} | {values} |"));
        }
    } else {
        for record in &browser.records {
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

fn detail_markdown(
    record: &RecordBrowserRecord,
    scope_indicator: Option<&str>,
    custom_help: Option<&str>,
    deletion_enabled: bool,
    filter_controls_enabled: bool,
) -> String {
    let mut lines = vec![format!("# {}", record.title), String::new()];
    if let Some(scope_indicator) = scope_indicator {
        lines.push(format!("**Scope:** {scope_indicator}"));
        lines.push(String::new());
    }
    lines.push(custom_help.map(str::to_string).unwrap_or_else(|| {
        if deletion_enabled && !filter_controls_enabled {
            "**Keys:** `Esc` back · `d` delete · `s` save".to_string()
        } else if deletion_enabled {
            "**Keys:** `a` all/default scope · `Esc` back · `d` delete · `s` save".to_string()
        } else {
            "**Keys:** `a` all/default scope · `Esc` back · `s` save".to_string()
        }
    }));
    lines.push(String::new());
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

fn list_record_label(record: &RecordBrowserRecord) -> String {
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
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('(', "\\(")
        .replace(')', "\\)")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
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

    fn browser_record(id: &str, title: &str) -> RecordBrowserRecord {
        RecordBrowserRecord {
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
        let mut browser = RecordBrowser::new(
            "Issues",
            vec![
                browser_record("issue-1", "First"),
                browser_record("issue-2", "Second"),
            ],
            Vec::new(),
        )
        .unwrap();
        browser.set_scroll_offset(4);

        browser.show_first_record_detail();
        assert!(browser.render_page().raw_markdown.contains("# First"));

        assert_eq!(
            browser
                .apply_action(RecordBrowserAction::BackToList)
                .unwrap(),
            RecordBrowserOutcome::Updated
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
        let mut browser = RecordBrowser::new("Memories", Vec::new(), Vec::new()).unwrap();
        browser.set_error(Some("database unavailable".to_string()));
        let empty_page = browser.render_page();
        assert!(empty_page.markdown.contains("No records found."));
        assert!(empty_page.markdown.contains("Error: database unavailable"));

        assert_eq!(
            browser
                .apply_action(RecordBrowserAction::StartFilter(
                    RecordBrowserFilterField::ProjectGlob,
                ))
                .unwrap(),
            RecordBrowserOutcome::Updated
        );
        assert!(matches!(
            browser.prompt(),
            Some(RecordBrowserPrompt::Filter {
                field: RecordBrowserFilterField::ProjectGlob,
                ..
            })
        ));
        browser
            .apply_action(RecordBrowserAction::EditPrompt("/repo/*".to_string()))
            .unwrap();
        assert_eq!(
            browser
                .apply_action(RecordBrowserAction::SubmitPrompt)
                .unwrap(),
            RecordBrowserOutcome::FilterSubmitted {
                field: RecordBrowserFilterField::ProjectGlob,
                value: "/repo/*".to_string(),
            }
        );

        let mut browser = RecordBrowser::new(
            "Issues",
            Vec::new(),
            vec![
                RecordBrowserFilterChoice {
                    label: "all kinds".to_string(),
                    value: String::new(),
                },
                RecordBrowserFilterChoice {
                    label: "defect".to_string(),
                    value: "defect".to_string(),
                },
                RecordBrowserFilterChoice {
                    label: "task".to_string(),
                    value: "task".to_string(),
                },
            ],
        )
        .unwrap();
        assert_eq!(
            browser
                .apply_action(RecordBrowserAction::StartFilter(
                    RecordBrowserFilterField::Kind,
                ))
                .unwrap(),
            RecordBrowserOutcome::Updated
        );
        assert!(matches!(
            browser.prompt(),
            Some(RecordBrowserPrompt::KindSelector { .. })
        ));
        assert_eq!(
            browser.prompt_selection(),
            Some(RecordBrowserPromptSelection {
                start_line: 1,
                option_count: 3,
                active_index: 0,
            })
        );
        assert_eq!(
            browser
                .apply_action(RecordBrowserAction::MovePromptSelection(1))
                .unwrap(),
            RecordBrowserOutcome::Updated
        );
        assert_eq!(
            browser
                .apply_action(RecordBrowserAction::SelectPromptFirst)
                .unwrap(),
            RecordBrowserOutcome::Updated
        );
        assert_eq!(
            browser
                .apply_action(RecordBrowserAction::SelectPromptLast)
                .unwrap(),
            RecordBrowserOutcome::Updated
        );
        assert_eq!(
            browser
                .apply_action(RecordBrowserAction::SubmitPrompt)
                .unwrap(),
            RecordBrowserOutcome::FilterSubmitted {
                field: RecordBrowserFilterField::Kind,
                value: "task".to_string(),
            }
        );

        let mut browser = RecordBrowser::new(
            "Issues",
            vec![RecordBrowserRecord {
                id: "issue-1".to_string(),
                open_command: Some("/show-issues issue-1".to_string()),
                title: "Pipe table".to_string(),
                metadata: vec![("state".to_string(), "open|resolved".to_string())],
                markdown: "Detail body".to_string(),
            }],
            Vec::new(),
        )
        .unwrap();
        browser.show_first_record_detail();
        browser
            .apply_action(RecordBrowserAction::StartSave)
            .unwrap();
        browser
            .apply_action(RecordBrowserAction::EditPrompt("issue.md".to_string()))
            .unwrap();
        match browser
            .apply_action(RecordBrowserAction::SubmitPrompt)
            .unwrap()
        {
            RecordBrowserOutcome::SaveSubmitted { path, markdown } => {
                assert_eq!(path, "issue.md");
                assert!(markdown.contains("| state | open\\|resolved |"));
                assert!(markdown.contains("Detail body"));
            }
            other => panic!("expected save outcome, got {other:?}"),
        }
    }

    /// Verifies table rendering preserves exactly one selectable record link
    /// when untrusted metadata contains Markdown link syntax and line breaks.
    #[test]
    fn record_browser_table_escapes_untrusted_metadata_links() {
        let mut browser = RecordBrowser::new(
            "Approvals",
            vec![RecordBrowserRecord {
                id: "ba1".to_string(),
                open_command: Some("/show-approvals ba1".to_string()),
                title: "Shell command".to_string(),
                metadata: vec![(
                    "Summary".to_string(),
                    "run [neighbor](mez-agent:%2Fapprove) | now\nthen exit".to_string(),
                )],
                markdown: "Detail body".to_string(),
            }],
            Vec::new(),
        )
        .unwrap();
        browser.set_table_columns(vec!["Summary".to_string()]);

        let page = browser.render_page();

        assert_eq!(page.raw_markdown.matches("](mez-agent:").count(), 1);
        assert!(page.raw_markdown.contains("\\|"), "{}", page.raw_markdown);
        assert!(
            !page.raw_markdown.contains("now\nthen"),
            "{}",
            page.raw_markdown
        );
    }

    /// Verifies destructive intent uses the highlighted stable record id and
    /// safely ignores deletion when the browser has no records.
    #[test]
    fn record_browser_delete_intent_tracks_the_active_record() {
        let mut browser = RecordBrowser::new(
            "Context",
            vec![
                browser_record("1", "User"),
                browser_record("2", "Assistant"),
            ],
            Vec::new(),
        )
        .unwrap();
        browser.enable_deletion();
        browser.set_active_index(1);

        assert_eq!(
            browser
                .apply_action(RecordBrowserAction::DeleteActive)
                .unwrap(),
            RecordBrowserOutcome::DeleteSubmitted {
                id: "2".to_string(),
            }
        );

        let mut empty = RecordBrowser::new("Context", Vec::new(), Vec::new()).unwrap();
        assert_eq!(
            empty
                .apply_action(RecordBrowserAction::DeleteActive)
                .unwrap(),
            RecordBrowserOutcome::Ignored
        );
    }
}
