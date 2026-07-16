//! Agent artifact, export, and project scaffold command helpers.
//!
//! This module owns slash commands that read or materialize agent-adjacent
//! artifacts: context exports, retained traces and patches, git diffs, project
//! instruction scaffolds, latest say-output copies, and auth logout side
//! effects. Keeping them separate from the command facade leaves live dispatch
//! and policy commands easier to navigate.

use super::{
    AgentShellCommandOutcome, MezError, PathBuf, Result, RuntimeSessionService, RuntimeSideEffect,
    json_escape, parse_slash_command, runtime_agent_init_scaffold,
    runtime_append_auth_logout_audit, runtime_git_repository_root, runtime_git_text,
    runtime_git_untracked_diff, runtime_git_untracked_files, runtime_write_agent_context_for_pane,
    runtime_write_agent_copy_output_for_pane, runtime_write_agent_patches_for_pane,
    runtime_write_agent_trace_log_for_pane,
};

impl RuntimeSessionService {
    /// Executes `/copy-context` against the active pane's model request context.
    pub(super) fn execute_agent_shell_copy_context_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("copy-context command must be a slash command")
        })?;
        let (body, mutated) =
            runtime_write_agent_context_for_pane(self, pane_id, invocation.args.trim())?;
        if mutated {
            Ok(AgentShellCommandOutcome::Mutated {
                command: "copy-context".to_string(),
                body,
                visibility: self.agent_shell_visibility_for_pane(pane_id)?,
            })
        } else {
            Ok(AgentShellCommandOutcome::Display {
                command: "copy-context".to_string(),
                body,
            })
        }
    }

    /// Executes `/copy-trace-log` against the retained bounded pane trace log.
    pub(super) fn execute_agent_shell_copy_trace_log_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("copy-trace-log command must be a slash command")
        })?;
        let (body, mutated) =
            runtime_write_agent_trace_log_for_pane(self, pane_id, invocation.args.trim())?;
        if mutated {
            Ok(AgentShellCommandOutcome::Mutated {
                command: "copy-trace-log".to_string(),
                body,
                visibility: self.agent_shell_visibility_for_pane(pane_id)?,
            })
        } else {
            Ok(AgentShellCommandOutcome::Display {
                command: "copy-trace-log".to_string(),
                body,
            })
        }
    }

    /// Executes `/copy-patches` against retained patch payloads for this session.
    pub(super) fn execute_agent_shell_copy_patches_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("copy-patches command must be a slash command")
        })?;
        let (body, mutated) =
            runtime_write_agent_patches_for_pane(self, pane_id, invocation.args.trim())?;
        if mutated {
            Ok(AgentShellCommandOutcome::Mutated {
                command: "copy-patches".to_string(),
                body,
                visibility: self.agent_shell_visibility_for_pane(pane_id)?,
            })
        } else {
            Ok(AgentShellCommandOutcome::Display {
                command: "copy-patches".to_string(),
                body,
            })
        }
    }

    /// Executes `/diff` against the pane's current version-control context.
    pub(super) fn execute_agent_shell_diff_command(
        &self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        Ok(AgentShellCommandOutcome::Display {
            command: "diff".to_string(),
            body: self.runtime_agent_diff_display(pane_id)?,
        })
    }

    /// Builds the live `/diff` display from the pane's current Git repository.
    pub(super) fn runtime_agent_diff_display(&self, pane_id: &str) -> Result<String> {
        let working_directory = self
            .pane_current_working_directory(pane_id)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let Some(repository_root) = runtime_git_repository_root(&working_directory)? else {
            return Ok(format!(
                "vcs=git status=unavailable cwd={} reason=not-a-git-repository source=runtime-vcs-diff",
                json_escape(&working_directory.to_string_lossy())
            ));
        };
        let staged_diff = runtime_git_text(
            &repository_root,
            &["diff", "--cached", "--no-ext-diff", "--no-color", "--"],
        )?;
        let worktree_diff = runtime_git_text(
            &repository_root,
            &["diff", "--no-ext-diff", "--no-color", "--"],
        )?;
        let untracked_files = runtime_git_untracked_files(&repository_root)?;
        let mut untracked_diffs = Vec::new();
        for file in &untracked_files {
            untracked_diffs.push(runtime_git_untracked_diff(&repository_root, file)?);
        }
        let mut lines = vec![format!(
            "vcs=git repository={} staged_diff_bytes={} worktree_diff_bytes={} untracked_files={} source=runtime-vcs-diff",
            json_escape(&repository_root.to_string_lossy()),
            staged_diff.len(),
            worktree_diff.len(),
            untracked_files.len()
        )];
        lines.push("[staged]".to_string());
        lines.push(if staged_diff.is_empty() {
            "(no staged changes)".to_string()
        } else {
            staged_diff
        });
        lines.push("[worktree]".to_string());
        lines.push(if worktree_diff.is_empty() {
            "(no unstaged changes)".to_string()
        } else {
            worktree_diff
        });
        lines.push("[untracked]".to_string());
        if untracked_files.is_empty() {
            lines.push("(no untracked files)".to_string());
        } else {
            for (file, diff) in untracked_files.iter().zip(untracked_diffs) {
                lines.push(format!("file={}", json_escape(file)));
                lines.push(diff);
            }
        }
        Ok(lines.join("\n"))
    }

    /// Executes `/init` by creating a project instruction scaffold.
    pub(super) fn execute_agent_shell_init_command(
        &mut self,
        pane_id: &str,
        input: &str,
        queue_for_adapter: bool,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("init command must be a slash command"))?;
        if !invocation.args.trim().is_empty() {
            return Err(MezError::invalid_args(
                "init slash command does not accept arguments",
            ));
        }
        let visibility = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
        let working_directory = self
            .pane_current_working_directory(pane_id)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let target = working_directory.join("AGENTS.md");
        if target.exists() {
            return Ok(AgentShellCommandOutcome::Display {
                command: "init".to_string(),
                body: format!(
                    "path={} created=false existing=true source=runtime-init",
                    json_escape(&target.to_string_lossy())
                ),
            });
        }
        let scaffold = runtime_agent_init_scaffold().as_bytes().to_vec();
        if queue_for_adapter {
            self.persistence.queue_config(RuntimeSideEffect::Persist {
                target: crate::runtime::PersistenceTarget::ProjectInstruction,
                path: target.clone(),
                bytes: scaffold,
                mode: crate::runtime::PersistenceWriteMode::CreateNew,
            });
        } else {
            std::fs::write(&target, &scaffold)?;
        }
        Ok(AgentShellCommandOutcome::Mutated {
            command: "init".to_string(),
            body: format!(
                "path={} created=true bytes={} source=runtime-init",
                json_escape(&target.to_string_lossy()),
                runtime_agent_init_scaffold().len()
            ),
            visibility,
        })
    }

    /// Executes `/copy` by copying the latest model-authored `say` text.
    pub(super) fn execute_agent_shell_copy_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("copy command must be a slash command"))?;
        let (body, mutated) =
            runtime_write_agent_copy_output_for_pane(self, pane_id, invocation.args.trim())?;
        if mutated {
            Ok(AgentShellCommandOutcome::Mutated {
                command: "copy".to_string(),
                body,
                visibility: self.agent_shell_visibility_for_pane(pane_id)?,
            })
        } else {
            Ok(AgentShellCommandOutcome::Display {
                command: "copy".to_string(),
                body,
            })
        }
    }

    /// Returns the latest model-authored `say` text retained for a pane.
    pub(in crate::runtime) fn latest_agent_copy_output_for_pane(
        &self,
        pane_id: &str,
    ) -> Option<(String, String, String)> {
        self.retained_agent_copy_output(pane_id).map(|output| {
            (
                output.turn_id.clone(),
                output.output.clone(),
                output.content_type.clone(),
            )
        })
    }

    /// Executes `/logout` through the runtime auth store.
    pub(super) fn execute_agent_shell_logout_command(
        &mut self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let visibility = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
        let Some(auth_store) = self.auth_store() else {
            return Ok(AgentShellCommandOutcome::Display {
                command: "logout".to_string(),
                body: "logged_out=false reason=auth-store-unavailable source=runtime-auth"
                    .to_string(),
            });
        };
        let changed = auth_store.logout()?;
        runtime_append_auth_logout_audit(self, changed)?;
        let body = format!("logged_out={changed} source=runtime-auth");
        Ok(AgentShellCommandOutcome::Mutated {
            command: "logout".to_string(),
            body,
            visibility,
        })
    }
}
