//! Immutable accounting report snapshots and harness-aware table projection.
//!
//! Acceptance captures diagnostics, counters, registry and UTC time. Only the
//! extended worker reads SQLite. Rendering never resolves roots or current focus;
//! historical and unattributed partitions remain explicit and zero-use inventory
//! records require no fabricated model rows.

use super::super::super::RuntimeSessionService;
use crate::error::{MezError, Result};
use crate::storage::token_usage::{
    AccountingProjectId, AccountingProjectRecord, TOKEN_USAGE_WINDOWS_DAYS, TokenHistoryKey,
    TokenHistoryScope, TokenHistorySnapshot, TokenHistoryUsage, TokenUsageStore,
};
use mez_agent::slash::{StatusOptions, StatusScope};
use std::collections::{BTreeMap, BTreeSet};

/// Acceptance-owned report and delivery evidence, independent of worker timing.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeStatusReportWork {
    /// Exact attached primary who requested the report.
    pub(crate) client: mez_core::ids::ClientId,
    /// Pane whose counters and diagnostics were captured.
    pub(crate) pane: String,
    /// Conversation owning the pane at acceptance, including visibility checks.
    conversation: String,
    /// Canonical cwd evidence retained by the actor; never reread by the worker.
    pub(crate) cwd: Option<std::path::PathBuf>,
    /// Active project at acceptance; completion must not relabel changed state.
    pub(crate) active_project: Option<AccountingProjectId>,
    /// Shared strict grammar result.
    pub(crate) options: StatusOptions,
    /// One UTC instant for every history boundary.
    now: u64,
    /// Already-rendered pane diagnostics, without scoped accounting tables.
    diagnostics: String,
    /// Qualified registry snapshot; missing is unavailable, not empty.
    inventory: Option<Vec<AccountingProjectRecord>>,
    /// Pane-view partitions, after reset baselines.
    pane_usage: Vec<(TokenHistoryKey, TokenHistoryUsage)>,
    /// Runtime-instance partitions, independent of pane reset or closure.
    instance_usage: Vec<(TokenHistoryKey, TokenHistoryUsage)>,
    /// Captured history handle; no query occurs at acceptance.
    store: Option<TokenUsageStore>,
    /// Captured accounting health; a gap must not become plausible zero history.
    health: Option<String>,
}

/// Converts native partitions without inferring missing project attribution.
fn native_rows(
    rows: Vec<mez_agent::ProjectTokenUsage>,
) -> Result<Vec<(TokenHistoryKey, TokenHistoryUsage)>> {
    rows.into_iter()
        .map(|row| {
            Ok((
                TokenHistoryKey {
                    project: row
                        .project_id
                        .map(AccountingProjectId::from_stored)
                        .transpose()?,
                    harness: "mez".to_string(),
                    model: row.model,
                },
                TokenHistoryUsage {
                    usage: row.usage,
                    reasoning_known: true,
                },
            ))
        })
        .collect()
}

/// Combines same-harness/model rows only within the caller's chosen project scope.
fn table(
    rows: impl IntoIterator<Item = (TokenHistoryKey, TokenHistoryUsage)>,
) -> Result<Vec<String>> {
    let mut totals = BTreeMap::<(String, mez_agent::ModelTokenUsageKey), TokenHistoryUsage>::new();
    for (key, value) in rows {
        let total = totals
            .entry((key.harness, key.model))
            .or_insert(TokenHistoryUsage {
                usage: Default::default(),
                reasoning_known: true,
            });
        total.usage =
            crate::storage::token_usage::checked_history_usage_sum(total.usage, value.usage)?;
        total.reasoning_known &= value.reasoning_known;
    }
    let rows = totals
        .into_iter()
        .map(|((harness, model), value)| {
            vec![
                harness,
                model.provider,
                model.model,
                value.usage.billed_input_tokens().to_string(),
                value.usage.cached_input_tokens_display(),
                value.usage.output_tokens.to_string(),
                if value.reasoning_known {
                    value.usage.reasoning_tokens.to_string()
                } else {
                    "unknown".to_string()
                },
                value.usage.cached_input_hit_ratio_display(),
            ]
        })
        .collect::<Vec<_>>();
    Ok(super::runtime_markdown_table(
        &[
            "Harness",
            "Provider",
            "Model",
            "Input",
            "Cached input",
            "Output",
            "Reasoning",
            "Cumulative Cache Hit %",
        ],
        &rows,
    ))
}

impl RuntimeSessionService {
    /// Retains one acceptance snapshot, dropping superseded snapshots for its pane.
    pub(crate) fn retain_pending_status_report(
        &mut self,
        pane: &str,
        generation: u64,
        report: RuntimeStatusReportWork,
    ) {
        self.agent.retain_status_report(pane, generation, report);
    }

    /// Consumes only the exact accepted status snapshot at worker claim.
    pub(crate) fn take_pending_status_report(
        &mut self,
        pane: &str,
        generation: u64,
    ) -> Option<RuntimeStatusReportWork> {
        self.agent.take_status_report(pane, generation)
    }

    /// Captures a report entirely from retained runtime evidence and counters.
    pub(crate) fn prepare_status_report(
        &self,
        client: &mez_core::ids::ClientId,
        pane: &str,
        options: StatusOptions,
    ) -> Result<RuntimeStatusReportWork> {
        if !self.session.is_attached_primary(client) {
            return Err(MezError::forbidden(
                "status requires an attached primary client",
            ));
        }
        let conversation = self
            .agent_shell_store()
            .get(pane)
            .ok_or_else(|| MezError::invalid_state("status pane conversation unavailable"))?
            .session_id
            .clone();
        let active_project = self.cached_accounting_project_for_pane(pane);
        if options.scope == StatusScope::Project && active_project.is_none() {
            return Err(MezError::invalid_state(
                "active-project-unavailable: no eligible qualified accounting project",
            ));
        }
        let diagnostics = self.runtime_agent_status_display_with_accounting(
            pane,
            false,
            options.scope == StatusScope::Overall,
        )?;
        let mut pane_usage = native_rows(self.project_usage_for_pane(pane))?;
        let mut instance_usage = native_rows(self.project_usage_for_instance())?;
        pane_usage.extend(self.external_usage_partitions(Some(pane)));
        instance_usage.extend(self.external_usage_partitions(None));
        Ok(RuntimeStatusReportWork {
            client: client.clone(),
            pane: pane.to_string(),
            conversation,
            cwd: self.pane_current_working_directory(pane),
            active_project,
            options,
            now: self.persistence.token_usage_time(),
            diagnostics,
            inventory: self.persistence.accounting_projects().map(<[_]>::to_vec),
            pane_usage,
            instance_usage,
            store: self.persistence.cloned_token_usage_store(),
            health: self.persistence.token_usage_health_error(),
        })
    }

    /// Revalidates retained report ownership without filesystem or database I/O.
    pub(crate) fn status_report_is_current(&self, work: &RuntimeStatusReportWork) -> bool {
        self.session.is_attached_primary(&work.client)
            && self
                .agent_shell_store()
                .get(&work.pane)
                .is_some_and(|session| {
                    session.session_id == work.conversation
                        && session.visibility == mez_agent::AgentShellVisibility::Visible
                })
            && self.pane_current_working_directory(&work.pane) == work.cwd
            && self.cached_accounting_project_for_pane(&work.pane) == work.active_project
    }
}

impl RuntimeStatusReportWork {
    /// Builds the static report; only an extended request may query captured storage.
    pub(crate) fn render(&self) -> Result<String> {
        let mut lines = vec![
            self.diagnostics.clone(),
            String::new(),
            format!("## Accounting Snapshot (STATIC; UTC {})", self.now),
        ];
        let history = if self.options.extended {
            if let Some(health) = &self.health {
                lines.extend([
                    String::new(),
                    "### Rolling Token Usage Unavailable".into(),
                    health.clone(),
                ]);
                None
            } else if let Some(store) = &self.store {
                match store.history_snapshot(
                    self.now,
                    &TOKEN_USAGE_WINDOWS_DAYS,
                    &TokenHistoryScope {
                        project: (self.options.scope == StatusScope::Project)
                            .then(|| self.active_project.clone())
                            .flatten(),
                        native_only: self.options.scope == StatusScope::Overall,
                        ..Default::default()
                    },
                ) {
                    Ok(history) => Some(history),
                    Err(_) => {
                        lines.extend([String::new(), "### Rolling Token Usage Unavailable".into(), "persistent token accounting is unavailable after a storage query failure".into()]);
                        None
                    }
                }
            } else {
                lines.extend([
                    String::new(),
                    "### Rolling Token Usage Unavailable".into(),
                    "durable accounting store unavailable".into(),
                ]);
                None
            }
        } else {
            None
        };
        if self.options.scope == StatusScope::Overall {
            if let Some(history) = history {
                self.append_history(&mut lines, &history, None, true)?;
            }
            return Ok(lines.join("\n"));
        }
        if self.options.scope == StatusScope::Project {
            self.append_project(
                &mut lines,
                self.active_project.as_ref(),
                "Active Project",
                history.as_ref(),
            )?;
        } else {
            let mut registered = BTreeSet::new();
            if let Some(inventory) = &self.inventory {
                for record in inventory {
                    if let Some(id) = &record.id {
                        registered.insert(id.clone());
                    }
                    lines.extend([
                        String::new(),
                        format!(
                            "### Registered Project: {}",
                            crate::runtime::json_escape(&record.root.to_string_lossy())
                        ),
                        format!(
                            "Trust: {:?}; trust version {}; configuration version {}",
                            record.trust,
                            record.trust_policy_version,
                            record.configuration_schema_version
                        ),
                    ]);
                    if let Some(id) = &record.id {
                        self.append_project(
                            &mut lines,
                            Some(id),
                            "Registered Project Accounting",
                            history.as_ref(),
                        )?;
                    } else {
                        lines.push(
                            "Accounting identity unavailable; no model rows fabricated.".into(),
                        );
                    }
                }
            } else {
                lines.push("Registered project inventory unavailable (not verified empty).".into());
            }
            let mut historical = self
                .pane_usage
                .iter()
                .chain(&self.instance_usage)
                .filter_map(|(key, _)| key.project.clone())
                .collect::<BTreeSet<_>>();
            if let Some(history) = &history {
                historical.extend(
                    history
                        .windows
                        .values()
                        .flat_map(|rows| rows.keys().filter_map(|key| key.project.clone())),
                );
            }
            for id in historical.difference(&registered) {
                self.append_project(
                    &mut lines,
                    Some(id),
                    "Historical Unregistered Project",
                    history.as_ref(),
                )?;
            }
            self.append_project(&mut lines, None, "Unattributed Remainder", history.as_ref())?;
        }
        Ok(lines.join("\n"))
    }

    /// Appends same-scope pane/session partitions, retaining explicit zero-use headings.
    fn append_project(
        &self,
        lines: &mut Vec<String>,
        project: Option<&AccountingProjectId>,
        label: &str,
        history: Option<&TokenHistorySnapshot>,
    ) -> Result<()> {
        lines.extend([
            String::new(),
            format!(
                "### {label}: {}",
                project.map_or("unattributed", AccountingProjectId::as_str)
            ),
            "Pane view (pane lifetime; resettable)".into(),
        ]);
        lines.extend(table(
            self.pane_usage
                .iter()
                .filter(|(key, _)| key.project.as_ref() == project)
                .cloned(),
        )?);
        lines.extend([
            String::new(),
            "Mez session (runtime-instance lifetime)".into(),
        ]);
        lines.extend(table(
            self.instance_usage
                .iter()
                .filter(|(key, _)| key.project.as_ref() == project)
                .cloned(),
        )?);
        if let Some(history) = history {
            self.append_history(lines, history, project, false)?;
        }
        Ok(())
    }

    /// Uses one history age boundary and window set for every project comparison.
    fn append_history(
        &self,
        lines: &mut Vec<String>,
        history: &TokenHistorySnapshot,
        project: Option<&AccountingProjectId>,
        overall: bool,
    ) -> Result<()> {
        let Some(oldest) = history.oldest_observed_at else {
            return Ok(());
        };
        let end = TOKEN_USAGE_WINDOWS_DAYS
            .iter()
            .position(|days| oldest >= history.now.saturating_sub(u64::from(*days) * 86_400))
            .map_or(TOKEN_USAGE_WINDOWS_DAYS.len(), |index| index + 1);
        for days in &TOKEN_USAGE_WINDOWS_DAYS[..end] {
            lines.extend([
                String::new(),
                format!("### {days}-Day Token Usage (rolling; UTC {})", history.now),
            ]);
            lines.extend(table(
                history
                    .windows
                    .get(days)
                    .into_iter()
                    .flat_map(|rows| rows.iter())
                    .filter(|(key, _)| overall || key.project.as_ref() == project)
                    .map(|(key, value)| (key.clone(), value.clone())),
            )?);
        }
        Ok(())
    }
}
