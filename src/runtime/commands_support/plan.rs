//! Typed runtime terminal-command execution planning.
//!
//! This module classifies a parsed terminal command sequence before runtime
//! mutation begins. Most commands execute immediately through the serialized
//! runtime coordinator; provider metadata refresh is identified as an awaited
//! effect so the async host can execute it without duplicating ordinary
//! command dispatch.

use super::{CommandInvocation, Result, parse_command_sequence};

/// One parsed terminal command together with its effect-execution class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RuntimeTerminalCommandPlan {
    /// A command whose complete execution is synchronous inside the runtime.
    Immediate(CommandInvocation),
    /// Provider metadata discovery that must be awaited by the async host.
    RefreshProviderInfo(CommandInvocation),
}

impl RuntimeTerminalCommandPlan {
    /// Returns the parsed command invocation owned by this plan.
    pub(super) fn invocation(&self) -> &CommandInvocation {
        match self {
            Self::Immediate(invocation) | Self::RefreshProviderInfo(invocation) => invocation,
        }
    }
}

/// Parses and classifies a complete terminal command sequence once.
pub(super) fn runtime_terminal_command_plan(
    input: &str,
) -> Result<Vec<RuntimeTerminalCommandPlan>> {
    Ok(parse_command_sequence(input)?
        .into_iter()
        .map(|invocation| {
            if invocation.name == "refresh-provider-info" {
                RuntimeTerminalCommandPlan::RefreshProviderInfo(invocation)
            } else {
                RuntimeTerminalCommandPlan::Immediate(invocation)
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{RuntimeTerminalCommandPlan, runtime_terminal_command_plan};

    /// Verifies ordinary commands retain their parsed order in one immediate
    /// execution plan so sync and async callers share identical sequencing.
    #[test]
    fn terminal_command_plan_preserves_immediate_command_order() {
        let plan = runtime_terminal_command_plan("list-panes; show-metrics").unwrap();

        assert_eq!(plan.len(), 2);
        assert!(matches!(plan[0], RuntimeTerminalCommandPlan::Immediate(_)));
        assert!(matches!(plan[1], RuntimeTerminalCommandPlan::Immediate(_)));
        assert_eq!(plan[0].invocation().name, "list-panes");
        assert_eq!(plan[1].invocation().name, "show-metrics");
    }

    /// Verifies provider refresh is classified as the sole awaited terminal
    /// effect while surrounding commands remain immediate runtime work.
    #[test]
    fn terminal_command_plan_identifies_provider_refresh_effect() {
        let plan = runtime_terminal_command_plan("list-panes; refresh-provider-info").unwrap();

        assert!(matches!(
            plan[1],
            RuntimeTerminalCommandPlan::RefreshProviderInfo(_)
        ));
        assert_eq!(plan[1].invocation().name, "refresh-provider-info");
    }
}
