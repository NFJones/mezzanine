//! Agent slash commands for local project issue tracking.
//!
//! This module owns the `/issue` runtime command surface. It resolves the
//! active pane project, opens the configured local issue store, and formats
//! compact command responses for add, query, and delete operations.

use super::*;

/// Executes one `/issue` slash command for the active pane project.
pub(super) fn execute_agent_shell_issue_command(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    input: &str,
) -> Result<AgentShellCommandOutcome> {
    let visibility = service.agent_shell_visibility_for_pane(pane_id)?;
    let slash = parse_slash_command(input)?
        .ok_or_else(|| MezError::invalid_args("issue command must be a slash command"))?;
    if !runtime_issues_enabled(service) {
        return Err(MezError::invalid_args(
            "issue commands require issues.enabled to be true",
        ));
    }
    let Some(config_root) = service.config_root.clone() else {
        return Err(MezError::config(
            "issue slash command requires a configured config root",
        ));
    };
    let working_directory = service
        .pane_current_working_directory(pane_id)
        .unwrap_or_else(|| config_root.clone());
    let project = crate::issues::project_key_for_working_directory(working_directory);
    let store = crate::issues::IssueStore::new(runtime_issue_database_path(service, &config_root));
    let args = parse_issue_args(slash.args.trim())?;
    match args {
        RuntimeIssueArgs::Add { kind, title, body } => {
            let record = store.add_issue(project, kind, title, body, current_unix_seconds())?;
            Ok(AgentShellCommandOutcome::Mutated {
                command: "issue".to_string(),
                body: format!(
                    "issue added id={} project={} kind={} title={}",
                    record.id,
                    json_escape(&record.project),
                    record.kind.as_str(),
                    json_escape(&record.title)
                ),
                visibility,
            })
        }
        RuntimeIssueArgs::Query { kind, text, limit } => {
            let query = crate::issues::IssueQuery::new(project, kind, text, limit)?;
            let records = store.query_issues(&query)?;
            Ok(AgentShellCommandOutcome::Display {
                command: "issue".to_string(),
                body: runtime_issue_records_display(&records),
            })
        }
        RuntimeIssueArgs::Delete { id } => {
            let result = store.delete_issue(project, id)?;
            Ok(AgentShellCommandOutcome::Mutated {
                command: "issue".to_string(),
                body: format!(
                    "issue delete id={} project={} deleted={}",
                    result.id,
                    json_escape(&result.project),
                    result.deleted
                ),
                visibility,
            })
        }
    }
}

enum RuntimeIssueArgs {
    Add {
        kind: crate::issues::IssueKind,
        title: String,
        body: Option<String>,
    },
    Query {
        kind: Option<crate::issues::IssueKind>,
        text: Option<String>,
        limit: Option<usize>,
    },
    Delete {
        id: String,
    },
}

fn parse_issue_args(args: &str) -> Result<RuntimeIssueArgs> {
    let Some(tokens) = shlex::split(args) else {
        return Err(MezError::invalid_args(
            "issue arguments contain an unterminated quote",
        ));
    };
    let Some(command) = tokens.first().map(String::as_str) else {
        return Err(MezError::invalid_args(
            "/issue expects add, query, or delete",
        ));
    };
    match command {
        "add" => parse_issue_add_args(&tokens[1..]),
        "query" | "list" => parse_issue_query_args(&tokens[1..]),
        "delete" | "remove" => parse_issue_delete_args(&tokens[1..]),
        _ => Err(MezError::invalid_args(
            "/issue expects add, query, or delete",
        )),
    }
}

fn parse_issue_add_args(tokens: &[String]) -> Result<RuntimeIssueArgs> {
    let mut kind = crate::issues::IssueKind::Defect;
    let mut title = None;
    let mut body = None;
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--kind" => {
                index = index.saturating_add(1);
                kind =
                    crate::issues::IssueKind::parse(required_issue_value(tokens, index, "kind")?)?;
            }
            "--title" => {
                index = index.saturating_add(1);
                title = Some(required_issue_value(tokens, index, "title")?.to_string());
            }
            "--body" => {
                index = index.saturating_add(1);
                body = Some(required_issue_value(tokens, index, "body")?.to_string());
            }
            _ => {
                return Err(MezError::invalid_args(
                    "issue add accepts --kind, --title, and --body",
                ));
            }
        }
        index = index.saturating_add(1);
    }
    Ok(RuntimeIssueArgs::Add {
        kind,
        title: title.ok_or_else(|| MezError::invalid_args("issue add requires --title"))?,
        body,
    })
}

fn parse_issue_query_args(tokens: &[String]) -> Result<RuntimeIssueArgs> {
    let mut kind = None;
    let mut text = None;
    let mut limit = None;
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--kind" => {
                index = index.saturating_add(1);
                kind = Some(crate::issues::IssueKind::parse(required_issue_value(
                    tokens, index, "kind",
                )?)?);
            }
            "--text" => {
                index = index.saturating_add(1);
                text = Some(required_issue_value(tokens, index, "text")?.to_string());
            }
            "--limit" => {
                index = index.saturating_add(1);
                limit = Some(
                    required_issue_value(tokens, index, "limit")?
                        .parse::<usize>()
                        .map_err(|_| {
                            MezError::invalid_args("issue query --limit must be an integer")
                        })?,
                );
            }
            _ => {
                return Err(MezError::invalid_args(
                    "issue query accepts --kind, --text, and --limit",
                ));
            }
        }
        index = index.saturating_add(1);
    }
    Ok(RuntimeIssueArgs::Query { kind, text, limit })
}

fn parse_issue_delete_args(tokens: &[String]) -> Result<RuntimeIssueArgs> {
    if tokens.len() != 1 {
        return Err(MezError::invalid_args("issue delete expects one issue id"));
    }
    Ok(RuntimeIssueArgs::Delete {
        id: tokens[0].clone(),
    })
}

fn required_issue_value<'a>(tokens: &'a [String], index: usize, name: &str) -> Result<&'a str> {
    tokens
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| MezError::invalid_args(format!("issue option --{name} requires a value")))
}

fn runtime_issue_records_display(records: &[crate::issues::IssueRecord]) -> String {
    if records.is_empty() {
        return "issues count=0".to_string();
    }
    let mut lines = vec![format!("issues count={}", records.len())];
    for record in records {
        lines.push(format!(
            "id={} project={} kind={} title={}",
            record.id,
            json_escape(&record.project),
            record.kind.as_str(),
            json_escape(&record.title)
        ));
    }
    lines.join("\n")
}

fn runtime_issues_enabled(service: &RuntimeSessionService) -> bool {
    runtime_effective_config_value(&service.config_layers)
        .ok()
        .and_then(|root| {
            root.get("issues")
                .and_then(|issues| issues.get("enabled"))
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(true)
}

fn runtime_issue_database_path(service: &RuntimeSessionService, config_root: &PathBuf) -> PathBuf {
    let configured = runtime_effective_config_value(&service.config_layers)
        .ok()
        .and_then(|root| {
            root.get("issues")
                .and_then(|issues| issues.get("database_path"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    crate::issues::issue_database_path(config_root, configured.as_deref())
}
