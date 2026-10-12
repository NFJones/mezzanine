//! Textual terminal status sequences with acceptance-owned off-actor reports.
//!
//! Immediate commands retain ordinary ordered actor execution. Snapshot commands
//! capture shared reports at their exact position, never resolving focus or roots
//! in the worker. One existing command lease per pane fences every report from
//! replacement, and completion returns text without opening a client overlay.

use super::{
    CommandOutcome, MezError, Result, RuntimeSessionService,
    execute_runtime_planned_terminal_command, parse_command_sequence,
};
use crate::runtime::{RuntimeAgentCommandLifecyclePhase, commands::RuntimeStatusReportWork};
use mez_core::ids::ClientId;
use std::collections::BTreeMap;

/// Ordered outcomes and captured reports for a single textual invocation.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeTerminalStatusWork {
    /// Attached primary that owns every outcome in this sequence.
    client: ClientId,
    /// Already-executed immediate outcomes; report placeholders are replaced off actor.
    outcomes: Vec<CommandOutcome>,
    /// Snapshot position and immutable shared report input.
    pub(crate) reports: Vec<(usize, RuntimeStatusReportWork)>,
    /// Exact pane-command generation and conversation for every reported pane.
    leases: BTreeMap<String, (String, u64)>,
    /// Test-only worker gate, with no production scheduling effect.
    #[cfg(test)]
    pub(crate) worker_gate: (
        Option<std::sync::Arc<tokio::sync::Notify>>,
        Option<std::sync::Arc<tokio::sync::Notify>>,
    ),
}

/// Parses the existing status token grammar with a terminal-specific diagnostic.
pub(crate) fn terminal_status_options(args: &[String]) -> Result<mez_agent::slash::StatusOptions> {
    mez_agent::slash::parse_status_option_tokens(args.iter().map(String::as_str)).map_err(|_| {
        MezError::invalid_args("usage: agent-status [--extended] [--project | --all-projects]")
    })
}

impl RuntimeTerminalStatusWork {
    /// Renders captured snapshots off actor without accessing mutable runtime state.
    pub(crate) fn render(&self) -> Result<String> {
        let mut outcomes = self.outcomes.clone();
        for (index, report) in &self.reports {
            outcomes[*index] = CommandOutcome::Display {
                command: "agent-status".into(),
                body: report.render()?,
            };
        }
        Ok(crate::runtime::runtime_command_outcomes_json(&outcomes))
    }
}

impl RuntimeSessionService {
    /// Recognizes snapshot sequences, captures each position and claims each pane.
    /// Plain live sequences stay on the ordinary terminal lane. No SQLite query
    /// or project discovery occurs during this bounded actor-owned preparation.
    pub(crate) fn prepare_terminal_status_work(
        &mut self,
        client: &ClientId,
        input: &str,
    ) -> Option<Result<RuntimeTerminalStatusWork>> {
        let commands = parse_command_sequence(input).ok()?;
        if !commands.iter().any(|command| {
            command.name == "agent-status"
                && terminal_status_options(&command.args).is_ok_and(|options| {
                    options.extended || options.scope != mez_agent::slash::StatusScope::Overall
                })
        }) {
            return None;
        }
        let mut leases = BTreeMap::<String, (String, u64)>::new();
        let prepared = (|| {
            self.require_live()?;
            if !self.session.is_attached_primary(client) {
                return Err(MezError::forbidden(
                    "status requires an attached primary client",
                ));
            }
            let mut active_client = client.clone();
            let mut outcomes = Vec::new();
            let mut reports = Vec::new();
            for command in &commands {
                if command.name == "agent-status" {
                    let options = terminal_status_options(&command.args)?;
                    if options.extended || options.scope != mez_agent::slash::StatusScope::Overall {
                        let pane = self.session.active_pane_for(&active_client)?.id.to_string();
                        let visibility = self
                            .agent_shell_store()
                            .get(&pane)
                            .map(|session| session.visibility);
                        let report = self
                            .prepare_status_report(&active_client, &pane, options)?
                            .for_terminal(visibility);
                        if let std::collections::btree_map::Entry::Vacant(entry) =
                            leases.entry(pane.clone())
                        {
                            let conversation = report.command_owner();
                            let generation =
                                self.begin_agent_command_claim(&pane, &conversation)?;
                            if !self
                                .agent
                                .claim_agent_command(&pane, &conversation, generation)
                            {
                                self.agent.cancel_matching_agent_command(
                                    &pane,
                                    &conversation,
                                    generation,
                                );
                                return Err(MezError::conflict(
                                    "terminal status claim unavailable",
                                ));
                            }
                            entry.insert((conversation, generation));
                        }
                        reports.push((outcomes.len(), report));
                        outcomes.push(CommandOutcome::Noop {
                            command: "agent-status".into(),
                        });
                        continue;
                    }
                }
                outcomes.push(execute_runtime_planned_terminal_command(
                    self,
                    &mut active_client,
                    command,
                )?);
            }
            Ok(RuntimeTerminalStatusWork {
                client: client.clone(),
                outcomes,
                reports,
                leases: leases.clone(),
                #[cfg(test)]
                worker_gate: self.integration.deferred_agent_command_probe(),
            })
        })();
        if prepared.is_err() {
            for (pane, (conversation, generation)) in &leases {
                self.agent
                    .cancel_matching_agent_command(pane, conversation, *generation);
            }
        }
        Some(prepared)
    }

    /// Revalidates every captured owner and consumes all sequence leases.
    pub(crate) fn complete_terminal_status_work(
        &mut self,
        work: &RuntimeTerminalStatusWork,
        result: Result<String>,
    ) -> Result<String> {
        let current = self.session.is_attached_primary(&work.client)
            && work
                .reports
                .iter()
                .all(|(_, report)| self.status_report_is_current(report))
            && work
                .leases
                .iter()
                .all(|(pane, (conversation, generation))| {
                    self.agent
                        .agent_command_is_claimed(pane, conversation, *generation)
                });
        let result = if current {
            result
        } else {
            Err(MezError::conflict(
                "terminal status owner or active project changed",
            ))
        };
        let phase = if result.is_ok() {
            RuntimeAgentCommandLifecyclePhase::Completed
        } else {
            RuntimeAgentCommandLifecyclePhase::Failed
        };
        for (pane, (conversation, generation)) in &work.leases {
            self.agent
                .settle_agent_command(pane, conversation, *generation, phase);
        }
        result
    }
}
