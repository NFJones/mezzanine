//! Shared status flag grammar, independent of report storage and presentation.
//!
//! Scope flags never accept paths or values. Parsing grants no authority and
//! callers without runtime accounting must refuse scoped/extended reports.

/// Stable grammar used by runtime errors, help and lower-level callers.
pub const STATUS_USAGE: &str = "usage: /status [--extended] [--project | --all-projects]";

/// Accounting scope selected explicitly by the invoking user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StatusScope {
    /// Preserve ordinary overall totals.
    #[default]
    Overall,
    /// Invoking pane's currently eligible accounting project.
    Project,
    /// Registered projects, historical partitions and unattributed remainder.
    AllProjects,
}

/// Typed options; parsing does not resolve current project or query history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusOptions {
    /// Request rolling durable history in addition to current totals.
    pub extended: bool,
    /// Explicit accounting report scope.
    pub scope: StatusScope,
}

/// Parses both flag orders, rejecting duplicates, operands and value-bearing flags.
pub fn parse_status_options(args: &str) -> Result<StatusOptions, &'static str> {
    let mut options = StatusOptions::default();
    for arg in args.split_whitespace() {
        match arg {
            "--extended" if !options.extended => options.extended = true,
            "--project" if options.scope == StatusScope::Overall => {
                options.scope = StatusScope::Project
            }
            "--all-projects" if options.scope == StatusScope::Overall => {
                options.scope = StatusScope::AllProjects
            }
            _ => return Err(STATUS_USAGE),
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Accepted scope/extended combinations are order independent. Duplicate,
    /// conflicting and value-bearing flags never silently select overall totals.
    #[test]
    fn status_options_share_strict_scope_grammar() {
        for (args, scope, extended) in [
            ("", StatusScope::Overall, false),
            ("--extended", StatusScope::Overall, true),
            ("--project", StatusScope::Project, false),
            ("--all-projects", StatusScope::AllProjects, false),
            ("--extended --project", StatusScope::Project, true),
            ("--project --extended", StatusScope::Project, true),
            ("--extended --all-projects", StatusScope::AllProjects, true),
            ("--all-projects --extended", StatusScope::AllProjects, true),
        ] {
            assert_eq!(
                parse_status_options(args),
                Ok(StatusOptions { scope, extended })
            );
        }
        for args in [
            "--project /tmp",
            "--project=x",
            "--extended=yes",
            "--unknown",
            "x",
            "--project --all-projects",
            "--project --project",
            "--all-projects --all-projects",
            "--extended --extended",
        ] {
            assert_eq!(parse_status_options(args), Err(STATUS_USAGE));
        }
    }
}
