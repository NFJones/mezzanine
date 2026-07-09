//! Agent-shell record-browser commands for issues and memories.
//!
//! This module wires the reusable runtime record-browser model to concrete
//! issue and memory stores. It keeps query parsing, record adaptation, and
//! save-to-file behavior close to the live slash-command runtime while leaving
//! the browser state itself backend-agnostic.

use super::*;
use crate::runtime::record_browser::{RuntimeRecordBrowser, RuntimeRecordBrowserRecord};

const DEFAULT_SHOW_RECORD_LIMIT: usize = 100;

impl RuntimeSessionService {
    /// Executes `/show-issues` by querying issue records and rendering browser Markdown.
    pub(super) fn execute_agent_shell_show_issues_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let slash = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("show-issues command must be a slash command"))?;
        if !issues::runtime_issues_enabled(self) {
            return Err(MezError::invalid_args(
                "show-issues requires issues.enabled to be true",
            ));
        }
        let Some(config_root) = self.config_root.clone() else {
            return Err(MezError::config(
                "show-issues requires a configured config root",
            ));
        };
        let args = parse_show_issues_args(&slash.args)?;
        let working_directory = self
            .pane_current_working_directory(pane_id)
            .unwrap_or_else(|| config_root.clone());
        let current_project = crate::issues::project_key_for_working_directory(working_directory);
        let project_glob = args
            .project_glob
            .clone()
            .or_else(|| (!args.all_projects).then_some(current_project.clone()));
        let store = crate::issues::IssueStore::from_database_path(
            issues::runtime_issue_database_path(self, &config_root),
        );
        let records = if let Some(id) = args.detail_id.as_ref() {
            store
                .get_issue(current_project, id.clone())?
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            let query = crate::issues::IssueBrowserQuery::new(
                project_glob,
                args.kind,
                args.state.or(Some(crate::issues::IssueState::Open)),
                args.text,
                Some(args.limit),
            )?;
            store.query_issue_browser(&query)?
        };
        let mut browser = RuntimeRecordBrowser::new(
            if args.detail_id.is_some() {
                "Issue detail"
            } else {
                "Issues"
            },
            records.into_iter().map(issue_browser_record).collect(),
        )?;
        if args.detail_id.is_some() {
            let _ = browser.apply_action(
                crate::runtime::record_browser::RuntimeRecordBrowserAction::OpenFocused,
            )?;
        }
        let page = browser.render_page();
        if let Some(path) = args.save_path {
            return self.save_record_browser_page(pane_id, "show-issues", path, page.raw_markdown);
        }
        self.pending_record_browser_overlays
            .insert((pane_id.to_string(), "show-issues".to_string()), browser);
        Ok(AgentShellCommandOutcome::Display {
            command: "show-issues".to_string(),
            body: page.raw_markdown,
        })
    }

    /// Executes `/show-memories` by querying persistent memory and rendering browser Markdown.
    pub(super) fn execute_agent_shell_show_memories_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let slash = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("show-memories command must be a slash command")
        })?;
        if !self.runtime_persistent_memory_enabled() {
            return Err(MezError::invalid_state(
                "show-memories requires persistent memory to be enabled; run /memory on first",
            ));
        }
        let Some(config_root) = self.config_root.clone() else {
            return Err(MezError::invalid_state(
                "show-memories requires a configured Mezzanine config root",
            ));
        };
        let args = parse_show_memories_args(&slash.args)?;
        let store = crate::memory::PersistentMemoryStore::under_config_root(&config_root);
        let scope = if args.all_scopes {
            None
        } else {
            Some(
                args.scope
                    .unwrap_or_else(|| self.runtime_remember_scope_for_pane(pane_id)),
            )
        };
        let records = if let Some(id) = args.detail_id.as_ref() {
            vec![store.inspect(id)?]
        } else {
            store
                .search(&crate::memory::MemorySearchRequest {
                    query: args.text,
                    scope,
                    kind: args.kind,
                    state: Some(args.state.unwrap_or(crate::memory::MemoryState::Active)),
                    source: None,
                    limit: args.limit,
                })?
                .into_iter()
                .map(|result| result.record)
                .collect()
        };
        let mut browser = RuntimeRecordBrowser::new(
            if args.detail_id.is_some() {
                "Memory detail"
            } else {
                "Memories"
            },
            records.into_iter().map(memory_browser_record).collect(),
        )?;
        if args.detail_id.is_some() {
            let _ = browser.apply_action(
                crate::runtime::record_browser::RuntimeRecordBrowserAction::OpenFocused,
            )?;
        }
        let page = browser.render_page();
        if let Some(path) = args.save_path {
            return self.save_record_browser_page(
                pane_id,
                "show-memories",
                path,
                page.raw_markdown,
            );
        }
        self.pending_record_browser_overlays
            .insert((pane_id.to_string(), "show-memories".to_string()), browser);
        Ok(AgentShellCommandOutcome::Display {
            command: "show-memories".to_string(),
            body: page.raw_markdown,
        })
    }

    fn save_record_browser_page(
        &self,
        pane_id: &str,
        command: &str,
        path: String,
        markdown: String,
    ) -> Result<AgentShellCommandOutcome> {
        let destination = record_browser_save_destination(self, pane_id, &path);
        fs::write(&destination, markdown)?;
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: command.to_string(),
            body: format!(
                "{} saved path={}",
                command,
                json_escape(&destination.to_string_lossy())
            ),
            visibility,
        })
    }
}

#[derive(Debug, Default)]
struct ShowIssuesArgs {
    project_glob: Option<String>,
    all_projects: bool,
    kind: Option<crate::issues::IssueKind>,
    state: Option<crate::issues::IssueState>,
    text: Option<String>,
    limit: usize,
    save_path: Option<String>,
    detail_id: Option<String>,
}

#[derive(Debug, Default)]
struct ShowMemoriesArgs {
    all_scopes: bool,
    scope: Option<crate::memory::MemoryScope>,
    kind: Option<crate::memory::MemoryKind>,
    state: Option<crate::memory::MemoryState>,
    text: Option<String>,
    limit: usize,
    save_path: Option<String>,
    detail_id: Option<String>,
}

fn parse_show_issues_args(args: &str) -> Result<ShowIssuesArgs> {
    let mut parsed = ShowIssuesArgs {
        limit: DEFAULT_SHOW_RECORD_LIMIT,
        ..ShowIssuesArgs::default()
    };
    let Some(tokens) = shlex::split(args) else {
        return Err(MezError::invalid_args(
            "show-issues arguments contain an unterminated quote",
        ));
    };
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--project" | "--project-glob" => {
                index = index.saturating_add(1);
                parsed.project_glob =
                    Some(required_show_value(&tokens, index, "project")?.to_string());
            }
            "--all-projects" => parsed.all_projects = true,
            "--kind" => {
                index = index.saturating_add(1);
                parsed.kind = Some(crate::issues::IssueKind::parse(required_show_value(
                    &tokens, index, "kind",
                )?)?);
            }
            "--state" => {
                index = index.saturating_add(1);
                let value = required_show_value(&tokens, index, "state")?;
                parsed.state = if value == "all" {
                    None
                } else {
                    Some(crate::issues::IssueState::parse(value)?)
                };
            }
            "--text" | "--query" => {
                index = index.saturating_add(1);
                parsed.text = Some(required_show_value(&tokens, index, "text")?.to_string());
            }
            "--limit" => {
                index = index.saturating_add(1);
                parsed.limit = parse_show_limit(required_show_value(&tokens, index, "limit")?)?;
            }
            "--save" => {
                index = index.saturating_add(1);
                parsed.save_path = Some(required_show_value(&tokens, index, "save")?.to_string());
            }
            token if token.starts_with('-') => {
                return Err(MezError::invalid_args(
                    "show-issues accepts --project, --all-projects, --kind, --state, --text, --limit, --save, and an optional issue id",
                ));
            }
            id => {
                if parsed.detail_id.replace(id.to_string()).is_some() {
                    return Err(MezError::invalid_args(
                        "show-issues accepts at most one issue id",
                    ));
                }
            }
        }
        index = index.saturating_add(1);
    }
    Ok(parsed)
}

fn parse_show_memories_args(args: &str) -> Result<ShowMemoriesArgs> {
    let mut parsed = ShowMemoriesArgs {
        limit: DEFAULT_SHOW_RECORD_LIMIT,
        ..ShowMemoriesArgs::default()
    };
    let Some(tokens) = shlex::split(args) else {
        return Err(MezError::invalid_args(
            "show-memories arguments contain an unterminated quote",
        ));
    };
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--scope" => {
                index = index.saturating_add(1);
                let value = required_show_value(&tokens, index, "scope")?;
                match value {
                    "all" => parsed.all_scopes = true,
                    "global" => parsed.scope = Some(crate::memory::MemoryScope::Global),
                    "project" => parsed.scope = None,
                    _ => {
                        return Err(MezError::invalid_args(
                            "show-memories --scope accepts project, global, or all",
                        ));
                    }
                }
            }
            "--all-scopes" => parsed.all_scopes = true,
            "--kind" => {
                index = index.saturating_add(1);
                parsed.kind = Some(parse_memory_kind_for_show(required_show_value(
                    &tokens, index, "kind",
                )?)?);
            }
            "--state" => {
                index = index.saturating_add(1);
                let value = required_show_value(&tokens, index, "state")?;
                parsed.state = if value == "all" {
                    None
                } else {
                    Some(parse_memory_state_for_show(value)?)
                };
            }
            "--text" | "--query" => {
                index = index.saturating_add(1);
                parsed.text = Some(required_show_value(&tokens, index, "text")?.to_string());
            }
            "--limit" => {
                index = index.saturating_add(1);
                parsed.limit = parse_show_limit(required_show_value(&tokens, index, "limit")?)?;
            }
            "--save" => {
                index = index.saturating_add(1);
                parsed.save_path = Some(required_show_value(&tokens, index, "save")?.to_string());
            }
            token if token.starts_with('-') => {
                return Err(MezError::invalid_args(
                    "show-memories accepts --scope, --all-scopes, --kind, --state, --text, --limit, --save, and an optional memory id",
                ));
            }
            id => {
                if parsed.detail_id.replace(id.to_string()).is_some() {
                    return Err(MezError::invalid_args(
                        "show-memories accepts at most one memory id",
                    ));
                }
            }
        }
        index = index.saturating_add(1);
    }
    Ok(parsed)
}

fn required_show_value<'a>(tokens: &'a [String], index: usize, name: &str) -> Result<&'a str> {
    let value = tokens
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| MezError::invalid_args(format!("show option --{name} requires a value")))?;
    if value.starts_with("--") {
        return Err(MezError::invalid_args(format!(
            "show option --{name} requires a value"
        )));
    }
    Ok(value)
}

fn parse_show_limit(value: &str) -> Result<usize> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| MezError::invalid_args("show command --limit must be an integer"))?;
    if limit == 0 {
        return Err(MezError::invalid_args(
            "show command --limit must be positive",
        ));
    }
    Ok(limit.min(DEFAULT_SHOW_RECORD_LIMIT))
}

fn issue_browser_record(record: crate::issues::IssueRecord) -> RuntimeRecordBrowserRecord {
    let markdown = issue_record_markdown(&record);
    RuntimeRecordBrowserRecord {
        id: record.id.clone(),
        open_command: Some(format!("/show-issues {}", record.id)),
        title: record.title.clone(),
        metadata: vec![
            ("id".to_string(), record.id),
            ("project".to_string(), record.project),
            ("kind".to_string(), record.kind.as_str().to_string()),
            ("state".to_string(), record.state.as_str().to_string()),
            (
                "depends_on".to_string(),
                serde_json::json!(record.depends_on).to_string(),
            ),
            (
                "created_at_unix_seconds".to_string(),
                record.created_at_unix_seconds.to_string(),
            ),
            (
                "updated_at_unix_seconds".to_string(),
                record.updated_at_unix_seconds.to_string(),
            ),
        ],
        markdown,
    }
}

fn issue_record_markdown(record: &crate::issues::IssueRecord) -> String {
    let mut lines = Vec::new();
    if let Some(body) = record.body.as_deref() {
        lines.push(body.to_string());
    } else {
        lines.push("_No issue body._".to_string());
    }
    if let Some(notes) = record.notes.as_deref() {
        lines.push(String::new());
        lines.push("## Notes".to_string());
        lines.push(notes.to_string());
    }
    lines.join("\n")
}

fn memory_browser_record(record: crate::memory::MemoryRecord) -> RuntimeRecordBrowserRecord {
    RuntimeRecordBrowserRecord {
        id: record.id.clone(),
        open_command: Some(format!("/show-memories {}", record.id)),
        title: memory_record_title(&record),
        metadata: vec![
            ("id".to_string(), record.id),
            (
                "scope".to_string(),
                runtime_remember_scope_display(&record.scope),
            ),
            (
                "kind".to_string(),
                memory_kind_name_for_show(record.kind).to_string(),
            ),
            (
                "state".to_string(),
                memory_state_name_for_show(record.state).to_string(),
            ),
            (
                "source".to_string(),
                memory_source_name_for_show(record.source).to_string(),
            ),
            ("priority".to_string(), record.priority.to_string()),
            (
                "created_at_unix_seconds".to_string(),
                record.created_at_unix_seconds.to_string(),
            ),
            (
                "updated_at_unix_seconds".to_string(),
                record.updated_at_unix_seconds.to_string(),
            ),
        ],
        markdown: record.content,
    }
}

fn memory_record_title(record: &crate::memory::MemoryRecord) -> String {
    record
        .content
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.chars().take(80).collect())
        .unwrap_or_else(|| record.id.clone())
}

fn parse_memory_kind_for_show(value: &str) -> Result<crate::memory::MemoryKind> {
    match value {
        "preference" => Ok(crate::memory::MemoryKind::Preference),
        "fact" => Ok(crate::memory::MemoryKind::Fact),
        "procedure" => Ok(crate::memory::MemoryKind::Procedure),
        "documentation" => Ok(crate::memory::MemoryKind::Documentation),
        "research" => Ok(crate::memory::MemoryKind::Research),
        "episode" => Ok(crate::memory::MemoryKind::Episode),
        "warning" => Ok(crate::memory::MemoryKind::Warning),
        "scratch" => Ok(crate::memory::MemoryKind::Scratch),
        _ => Err(MezError::invalid_args("unknown memory kind")),
    }
}

fn parse_memory_state_for_show(value: &str) -> Result<crate::memory::MemoryState> {
    match value {
        "active" => Ok(crate::memory::MemoryState::Active),
        "stale" => Ok(crate::memory::MemoryState::Stale),
        "superseded" => Ok(crate::memory::MemoryState::Superseded),
        "archived" => Ok(crate::memory::MemoryState::Archived),
        "expired" => Ok(crate::memory::MemoryState::Expired),
        _ => Err(MezError::invalid_args("unknown memory state")),
    }
}

fn memory_kind_name_for_show(kind: crate::memory::MemoryKind) -> &'static str {
    match kind {
        crate::memory::MemoryKind::Preference => "preference",
        crate::memory::MemoryKind::Fact => "fact",
        crate::memory::MemoryKind::Procedure => "procedure",
        crate::memory::MemoryKind::Documentation => "documentation",
        crate::memory::MemoryKind::Research => "research",
        crate::memory::MemoryKind::Episode => "episode",
        crate::memory::MemoryKind::Warning => "warning",
        crate::memory::MemoryKind::Scratch => "scratch",
    }
}

fn memory_state_name_for_show(state: crate::memory::MemoryState) -> &'static str {
    match state {
        crate::memory::MemoryState::Active => "active",
        crate::memory::MemoryState::Stale => "stale",
        crate::memory::MemoryState::Superseded => "superseded",
        crate::memory::MemoryState::Archived => "archived",
        crate::memory::MemoryState::Expired => "expired",
    }
}

fn memory_source_name_for_show(source: crate::memory::MemorySource) -> &'static str {
    match source {
        crate::memory::MemorySource::User => "user",
        crate::memory::MemorySource::Agent => "agent",
        crate::memory::MemorySource::Imported => "imported",
        crate::memory::MemorySource::Configuration => "configuration",
    }
}

fn record_browser_save_destination(
    service: &RuntimeSessionService,
    pane_id: &str,
    input: &str,
) -> PathBuf {
    let path = PathBuf::from(input);
    if path.is_absolute() {
        return path;
    }
    service
        .pane_current_working_directory(pane_id)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies `/show-issues` accepts the browser filters and save path used
    /// by the interactive issue browser command surface.
    #[test]
    fn show_issues_parser_accepts_filters_detail_and_save_path() {
        let parsed = parse_show_issues_args(
            "--project /repo/* --kind task --state resolved --text panic --limit 20 --save out.md issue-1",
        )
        .unwrap();

        assert_eq!(parsed.project_glob.as_deref(), Some("/repo/*"));
        assert_eq!(parsed.kind, Some(crate::issues::IssueKind::Task));
        assert_eq!(parsed.state, Some(crate::issues::IssueState::Resolved));
        assert_eq!(parsed.text.as_deref(), Some("panic"));
        assert_eq!(parsed.limit, 20);
        assert_eq!(parsed.save_path.as_deref(), Some("out.md"));
        assert_eq!(parsed.detail_id.as_deref(), Some("issue-1"));
    }

    /// Verifies `/show-memories` defaults remain bounded and parse the shared
    /// memory filters used by the record-browser adapter.
    #[test]
    fn show_memories_parser_accepts_scope_kind_state_text_and_limit() {
        let parsed = parse_show_memories_args(
            "--scope all --kind documentation --state stale --text maap --limit 250 memory-1",
        )
        .unwrap();

        assert!(parsed.all_scopes);
        assert_eq!(parsed.kind, Some(crate::memory::MemoryKind::Documentation));
        assert_eq!(parsed.state, Some(crate::memory::MemoryState::Stale));
        assert_eq!(parsed.text.as_deref(), Some("maap"));
        assert_eq!(parsed.limit, DEFAULT_SHOW_RECORD_LIMIT);
        assert_eq!(parsed.detail_id.as_deref(), Some("memory-1"));
    }
}
