//! Literal human agent-browser options, independent of runtime authority.
//!
//! The slash grammar is deliberately not a shell/project-expression parser.
//! Quoting, escaping, duplicate flags and operands never synthesize --all; colon
//! has its own argument-free grammar and does not consume this codec.

/// Stable usage shared by errors and command documentation.
pub const LIST_AGENTS_USAGE: &str = "usage: /list-agents [--all]";

/// Explicit human inventory scope; parsing does not qualify project evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListAgentsOptions {
    /// Include all current session registrations instead of the current project.
    pub all: bool,
}

/// Accepts empty whitespace or exactly one raw literal --all. No parser fallback
/// or evaluation is allowed, including apparently harmless quoted flag text.
pub fn parse_list_agents_options(args: &str) -> Result<ListAgentsOptions, &'static str> {
    let mut tokens = args.split_whitespace();
    match (tokens.next(), tokens.next()) {
        (None, None) => Ok(ListAgentsOptions::default()),
        (Some("--all"), None) => Ok(ListAgentsOptions { all: true }),
        _ => Err(LIST_AGENTS_USAGE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All callers share one strict literal grammar. Whitespace is inert, but
    /// shell quoting, escaped flags, duplicates and value-bearing options fail.
    #[test]
    fn list_agents_options_accept_only_empty_or_single_literal_all() {
        for args in ["", " \t\n "] {
            assert_eq!(
                parse_list_agents_options(args),
                Ok(ListAgentsOptions::default())
            );
        }
        for args in ["--all", " \t--all\n "] {
            assert_eq!(
                parse_list_agents_options(args),
                Ok(ListAgentsOptions { all: true })
            );
        }
        for args in [
            "--all --all",
            "--unknown",
            "project",
            "--all=true",
            "--all project",
            "\"--all\"",
            "'--all'",
            "\"--all",
            "--all\"",
            "--\\all",
            "--",
            "--ALL",
        ] {
            assert_eq!(
                parse_list_agents_options(args),
                Err(LIST_AGENTS_USAGE),
                "{args}"
            );
        }
    }
}
