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
    let store = crate::issues::IssueStore::from_database_path(runtime_issue_database_path(
        service,
        &config_root,
    ));
    let args = parse_issue_args(slash.args.trim())?;
    match args {
        RuntimeIssueArgs::Add {
            kind,
            title,
            body,
            notes,
            depends_on,
        } => {
            let record = store.add_issue_with_dependencies(
                crate::issues::NewIssueRecord {
                    project,
                    kind,
                    title,
                    body,
                    notes,
                    depends_on,
                },
                current_unix_seconds(),
            )?;
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
        RuntimeIssueArgs::Show { id } => {
            let record = store.get_issue(project, id)?;
            Ok(AgentShellCommandOutcome::Display {
                command: "issue".to_string(),
                body: runtime_issue_record_detail_display(record.as_ref()),
            })
        }
        RuntimeIssueArgs::Update { id, update } => {
            let result = store.update_issue(project, id, update, current_unix_seconds())?;
            Ok(AgentShellCommandOutcome::Mutated {
                command: "issue".to_string(),
                body: format!(
                    "issue update id={} project={} updated={}",
                    result.id,
                    json_escape(&result.project),
                    result.updated
                ),
                visibility,
            })
        }
        RuntimeIssueArgs::Query {
            kind,
            state,
            text,
            limit,
        } => {
            let query = crate::issues::IssueQuery::new_with_state(
                project,
                kind,
                state.or(Some(crate::issues::IssueState::Open)),
                text,
                limit,
            )?;
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

#[derive(Debug)]
enum RuntimeIssueArgs {
    Add {
        kind: crate::issues::IssueKind,
        title: String,
        body: Option<String>,
        notes: Option<String>,
        depends_on: Vec<String>,
    },
    Show {
        id: String,
    },
    Update {
        id: String,
        update: crate::issues::IssueUpdate,
    },
    Query {
        kind: Option<crate::issues::IssueKind>,
        state: Option<crate::issues::IssueState>,
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
            "/issue expects add, show, update, query, or delete",
        ));
    };
    match command {
        "add" => parse_issue_add_args(&tokens[1..]),
        "show" => parse_issue_show_args(&tokens[1..]),
        "update" => parse_issue_update_args(&tokens[1..]),
        "query" | "list" => parse_issue_query_args(&tokens[1..]),
        "delete" | "remove" => parse_issue_delete_args(&tokens[1..]),
        _ => Err(MezError::invalid_args(
            "/issue expects add, show, update, query, or delete",
        )),
    }
}

fn parse_issue_add_args(tokens: &[String]) -> Result<RuntimeIssueArgs> {
    let mut kind = crate::issues::IssueKind::Defect;
    let mut title = None;
    let mut body = None;
    let mut notes = None;
    let mut depends_on = Vec::new();
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
            "--notes" => {
                index = index.saturating_add(1);
                notes = Some(required_issue_value(tokens, index, "notes")?.to_string());
            }
            "--depends-on" => {
                index = index.saturating_add(1);
                depends_on.push(required_issue_value(tokens, index, "depends-on")?.to_string());
            }
            _ => {
                return Err(MezError::invalid_args(
                    "issue add accepts --kind, --title, --body, --notes, and --depends-on",
                ));
            }
        }
        index = index.saturating_add(1);
    }
    Ok(RuntimeIssueArgs::Add {
        kind,
        title: title.ok_or_else(|| MezError::invalid_args("issue add requires --title"))?,
        body,
        notes,
        depends_on,
    })
}

fn parse_issue_show_args(tokens: &[String]) -> Result<RuntimeIssueArgs> {
    if tokens.len() != 1 {
        return Err(MezError::invalid_args("issue show expects one issue id"));
    }
    Ok(RuntimeIssueArgs::Show {
        id: tokens[0].clone(),
    })
}

fn parse_issue_update_args(tokens: &[String]) -> Result<RuntimeIssueArgs> {
    let Some(id) = tokens.first() else {
        return Err(MezError::invalid_args("issue update expects one issue id"));
    };
    let mut update = crate::issues::IssueUpdate::default();
    let mut index = 1usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--kind" => {
                index = index.saturating_add(1);
                update.kind = Some(crate::issues::IssueKind::parse(required_issue_value(
                    tokens, index, "kind",
                )?)?);
            }
            "--state" => {
                index = index.saturating_add(1);
                update.state = Some(crate::issues::IssueState::parse(required_issue_value(
                    tokens, index, "state",
                )?)?);
            }
            "--title" => {
                index = index.saturating_add(1);
                update.title = Some(required_issue_value(tokens, index, "title")?.to_string());
            }
            "--body" => {
                index = index.saturating_add(1);
                update.body = Some(required_issue_value(tokens, index, "body")?.to_string());
            }
            "--clear-body" => update.clear_body = true,
            "--notes" => {
                index = index.saturating_add(1);
                update.notes = Some(required_issue_value(tokens, index, "notes")?.to_string());
            }
            "--clear-notes" => update.clear_notes = true,
            "--depends-on" => {
                index = index.saturating_add(1);
                update
                    .depends_on
                    .get_or_insert_with(Vec::new)
                    .push(required_issue_value(tokens, index, "depends-on")?.to_string());
            }
            "--clear-depends-on" => update.clear_depends_on = true,
            _ => {
                return Err(MezError::invalid_args(
                    "issue update accepts --kind, --state, --title, --body, --clear-body, --notes, --clear-notes, --depends-on, and --clear-depends-on",
                ));
            }
        }
        index = index.saturating_add(1);
    }
    update.validate()?;
    Ok(RuntimeIssueArgs::Update {
        id: id.clone(),
        update,
    })
}

fn parse_issue_query_args(tokens: &[String]) -> Result<RuntimeIssueArgs> {
    let mut kind = None;
    let mut state = None;
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
            "--state" => {
                index = index.saturating_add(1);
                state = Some(crate::issues::IssueState::parse(required_issue_value(
                    tokens, index, "state",
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
                    "issue query accepts --kind, --state, --text, and --limit",
                ));
            }
        }
        index = index.saturating_add(1);
    }
    Ok(RuntimeIssueArgs::Query {
        kind,
        state,
        text,
        limit,
    })
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
    let value = tokens
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| MezError::invalid_args(format!("issue option --{name} requires a value")))?;
    if value.starts_with("--") {
        return Err(MezError::invalid_args(format!(
            "issue option --{name} requires a value"
        )));
    }
    Ok(value)
}

fn runtime_issue_record_detail_display(record: Option<&crate::issues::IssueRecord>) -> String {
    let Some(record) = record else {
        return "issue found=false".to_string();
    };
    format!(
        "issue found=true\nid={}\nproject={}\nkind={}\nstate={}\ntitle={}\nbody={}\nnotes={}\ndepends_on={}\ncreated_at_unix_seconds={}\nupdated_at_unix_seconds={}",
        record.id,
        json_escape(&record.project),
        record.kind.as_str(),
        record.state.as_str(),
        json_escape(&record.title),
        record
            .body
            .as_deref()
            .map(json_escape)
            .unwrap_or_else(|| "null".to_string()),
        record
            .notes
            .as_deref()
            .map(json_escape)
            .unwrap_or_else(|| "null".to_string()),
        runtime_issue_depends_on_display(&record.depends_on),
        record.created_at_unix_seconds,
        record.updated_at_unix_seconds
    )
}

fn runtime_issue_records_display(records: &[crate::issues::IssueRecord]) -> String {
    if records.is_empty() {
        return "issues count=0".to_string();
    }
    let mut lines = vec![format!("issues count={}", records.len())];
    for record in records {
        lines.push(format!(
            "id={} project={} kind={} state={} title={} depends_on={}",
            record.id,
            json_escape(&record.project),
            record.kind.as_str(),
            record.state.as_str(),
            json_escape(&record.title),
            runtime_issue_depends_on_display(&record.depends_on)
        ));
    }
    lines.join("\n")
}

fn runtime_issue_depends_on_display(depends_on: &[String]) -> String {
    serde_json::json!(depends_on).to_string()
}

pub(super) fn runtime_issues_enabled(service: &RuntimeSessionService) -> bool {
    runtime_effective_config_value(&service.config_layers)
        .ok()
        .and_then(|root| {
            root.get("issues")
                .and_then(|issues| issues.get("enabled"))
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(true)
}

pub(super) fn runtime_issue_database_path(
    service: &RuntimeSessionService,
    config_root: &PathBuf,
) -> crate::issues::IssueDatabasePath {
    let configured = runtime_effective_config_value(&service.config_layers)
        .ok()
        .and_then(|root| {
            root.get("issues")
                .and_then(|issues| issues.get("database_path"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    crate::issues::issue_database_location(config_root, configured.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies `/issue` parsing accepts notes on add, update, and show commands
    /// so runtime users can store progress separately from issue descriptions.
    #[test]
    fn issue_parser_accepts_notes_update_and_show() {
        match parse_issue_args("add --title Work --notes progress").unwrap() {
            RuntimeIssueArgs::Add { title, notes, .. } => {
                assert_eq!(title, "Work");
                assert_eq!(notes.as_deref(), Some("progress"));
            }
            other => panic!("expected add args, got {other:?}"),
        }
        match parse_issue_args("update issue-1 --notes progressed").unwrap() {
            RuntimeIssueArgs::Update { id, update } => {
                assert_eq!(id, "issue-1");
                assert_eq!(update.notes.as_deref(), Some("progressed"));
                assert!(!update.clear_notes);
            }
            other => panic!("expected update args, got {other:?}"),
        }
        match parse_issue_args("show issue-1").unwrap() {
            RuntimeIssueArgs::Show { id } => assert_eq!(id, "issue-1"),
            other => panic!("expected show args, got {other:?}"),
        }
    }

    /// Verifies malformed `/issue` values fail before execution, including the
    /// option-as-value edge case and conflicting note update directives.
    #[test]
    fn issue_parser_rejects_missing_values_and_conflicting_notes() {
        let missing = parse_issue_args("add --title --body details").unwrap_err();
        assert!(
            missing
                .message()
                .contains("issue option --title requires a value"),
            "{}",
            missing.message()
        );
        let conflict =
            parse_issue_args("update issue-1 --notes progress --clear-notes").unwrap_err();
        assert!(
            conflict.message().contains("set and clear notes"),
            "{}",
            conflict.message()
        );
    }
}
