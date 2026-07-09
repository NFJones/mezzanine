//! Agent-shell read-only list command helpers.
//!
//! This module keeps small read-only agent command displays out of the main
//! runtime command facade. It owns the effective skill and macro catalog
//! displays plus the pane-local modified-file summary used by `/list-skills`,
//! `/list-macros`, and `/list-modified-files`.

use super::*;

impl RuntimeSessionService {
    /// Executes `/list-skills` and returns the effective skill catalog.
    ///
    /// The command is read-only and intentionally uses the same effective
    /// catalog as `$skill` prompt expansion so users see only skills that can
    /// be selected explicitly in the current pane.
    pub(super) fn execute_agent_shell_list_skills_command(
        &mut self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        self.refresh_project_config_layers_for_pane(pane_id)?;
        Ok(AgentShellCommandOutcome::Display {
            command: "list-skills".to_string(),
            body: self.runtime_agent_skill_catalog_display(pane_id),
        })
    }

    /// Builds the user-facing skill catalog display for `/list-skills`.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose config root and trusted project root determine
    ///   the effective skill set.
    fn runtime_agent_skill_catalog_display(&self, pane_id: &str) -> String {
        let catalog = self.effective_skill_catalog_for_pane(pane_id);
        let mut lines = vec![
            "## Skills".to_string(),
            String::new(),
            "Start a prompt with `$` and press Tab to select a skill by name.".to_string(),
            "Submit `$<skill-name> [additional context]` to invoke a skill explicitly.".to_string(),
            String::new(),
        ];
        if catalog.skills.is_empty() {
            lines.push("No skills are currently available.".to_string());
        } else {
            lines.push(format!("{} skills available:", catalog.skills.len()));
            lines.push(String::new());
            let rows = catalog
                .skills
                .iter()
                .map(|skill| {
                    vec![
                        format!("`${}`", skill.name),
                        skill.source.as_str().to_string(),
                        skill.description.clone(),
                    ]
                })
                .collect::<Vec<_>>();
            lines.extend(runtime_markdown_table(
                &["Skill", "Scope", "Description"],
                &rows,
            ));
        }
        if !catalog.diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("Skipped skill diagnostics:".to_string());
            lines.extend(catalog.diagnostics.iter().map(|diagnostic| {
                format!("- `{}` - {}", diagnostic.path.display(), diagnostic.message)
            }));
        }
        lines.join("\n")
    }

    /// Executes `/sync-builtin-skills` and reports managed built-in skill updates.
    pub(super) fn execute_agent_shell_sync_builtin_skills_command(
        &mut self,
    ) -> Result<AgentShellCommandOutcome> {
        let Some(config_root) = self.config_root.as_ref() else {
            return Err(MezError::invalid_state(
                "sync-builtin-skills requires a configured config root",
            ));
        };
        let report = crate::skills::sync_managed_builtin_skills(config_root)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "sync-builtin-skills".to_string(),
            body: Self::runtime_agent_builtin_skill_sync_display(&report),
            visibility: AgentShellVisibility::Visible,
        })
    }

    /// Builds the user-facing sync report for managed built-in skill copies.
    fn runtime_agent_builtin_skill_sync_display(
        report: &crate::skills::ManagedBuiltinSkillSyncReport,
    ) -> String {
        use crate::skills::ManagedBuiltinSkillSyncStatus;

        let changed = report.count(ManagedBuiltinSkillSyncStatus::Created)
            + report.count(ManagedBuiltinSkillSyncStatus::ReplacedStale)
            + report.count(ManagedBuiltinSkillSyncStatus::ReplacedMalformed);
        let mut lines = vec![
            "## Built-in skill sync".to_string(),
            String::new(),
            format!(
                "{} built-in skills checked; {} changed.",
                report.entries.len(),
                changed
            ),
            String::new(),
        ];
        let rows = report
            .entries
            .iter()
            .map(|entry| {
                vec![
                    format!("`${}`", entry.name),
                    entry.status.as_str().to_string(),
                    format!("`{}`", entry.path.display()),
                ]
            })
            .collect::<Vec<_>>();
        lines.extend(runtime_markdown_table(&["Skill", "Status", "Path"], &rows));
        lines.join("\n")
    }

    /// Executes `/list-macros` and returns the effective macro catalog.
    ///
    /// The command is read-only and intentionally uses the same effective
    /// catalog as `#macro` prompt recognition so users see only macros that can
    /// be selected explicitly in the current pane.
    pub(super) fn execute_agent_shell_list_macros_command(
        &mut self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        self.refresh_project_config_layers_for_pane(pane_id)?;
        Ok(AgentShellCommandOutcome::Display {
            command: "list-macros".to_string(),
            body: self.runtime_agent_macro_catalog_display(pane_id),
        })
    }

    /// Builds the user-facing macro catalog display for `/list-macros`.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose config root and trusted project root determine
    ///   the effective macro set.
    fn runtime_agent_macro_catalog_display(&self, pane_id: &str) -> String {
        let catalog = self.effective_macro_catalog_for_pane(pane_id);
        let mut lines = vec![
            "## Macros".to_string(),
            String::new(),
            "Start a prompt with `#` and press Tab to select a macro by name.".to_string(),
            "Submit `#<macro-name> [additional context]` to invoke a macro explicitly.".to_string(),
            String::new(),
        ];
        if catalog.macros.is_empty() {
            lines.push("No macros are currently available.".to_string());
        } else {
            lines.push(format!("{} macros available:", catalog.macros.len()));
            lines.push(String::new());
            let rows = catalog
                .macros
                .iter()
                .map(|macro_summary| {
                    vec![
                        format!("`#{}`", macro_summary.name),
                        macro_summary.source.as_str().to_string(),
                        macro_summary.step_count.to_string(),
                        macro_summary.description.clone(),
                    ]
                })
                .collect::<Vec<_>>();
            lines.extend(runtime_markdown_table(
                &["Macro", "Scope", "Steps", "Description"],
                &rows,
            ));
        }
        if !catalog.diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("Skipped macro diagnostics:".to_string());
            lines.extend(catalog.diagnostics.iter().map(|diagnostic| {
                format!("- `{}` - {}", diagnostic.path.display(), diagnostic.message)
            }));
        }
        lines.join("\n")
    }

    /// Executes `/list-modified-files` and returns a compact markdown list.
    pub(super) fn execute_agent_shell_list_modified_files_command(
        &self,
        pane_id: &str,
    ) -> AgentShellCommandOutcome {
        AgentShellCommandOutcome::Display {
            command: "list-modified-files".to_string(),
            body: self.runtime_agent_modified_files_display(pane_id),
        }
    }

    /// Builds the pane-local modified-file summary used by the agent shell.
    fn runtime_agent_modified_files_display(&self, pane_id: &str) -> String {
        let Some(files) = self.agent_modified_files.get(pane_id) else {
            return "## modified files\n\nno modified files tracked for this agent conversation."
                .to_string();
        };
        if files.is_empty() {
            return "## modified files\n\nno modified files tracked for this agent conversation."
                .to_string();
        }
        let total_added = files.values().map(|summary| summary.added).sum::<usize>();
        let total_removed = files.values().map(|summary| summary.removed).sum::<usize>();
        let mut lines = vec![
            "## modified files".to_string(),
            String::new(),
            format!(
                "{} ({} {}, {} files)",
                "summary",
                Self::markdown_modified_file_count_span("mez-diff-addition", '+', total_added),
                Self::markdown_modified_file_count_span("mez-diff-deletion", '-', total_removed),
                files.len()
            ),
            String::new(),
        ];
        for summary in files.values() {
            lines.push(format!(
                "- edited `{}` ({} {})",
                summary.path,
                Self::markdown_modified_file_count_span("mez-diff-addition", '+', summary.added),
                Self::markdown_modified_file_count_span("mez-diff-deletion", '-', summary.removed)
            ));
        }
        lines.join("\n")
    }

    /// Wraps one modified-file line count in a markdown span consumed by the
    /// terminal markdown renderer.
    ///
    /// # Parameters
    /// - `class_name`: The renderer-recognized presentation class.
    /// - `sign`: The leading `+` or `-` count sign.
    /// - `count`: The count to render.
    fn markdown_modified_file_count_span(class_name: &str, sign: char, count: usize) -> String {
        format!(r#"<span class="{class_name}">{sign}{count}</span>"#)
    }
}
