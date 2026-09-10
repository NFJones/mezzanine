//! Agent discovery vocabulary and bounds for the read-only `list_agents` action.
//!
//! This module owns the provider-facing agent-kind vocabulary, the
//! `list_agents` agent-type filter default, and the documented result bounds
//! shared by the MAAP schema, MAAP validation, and the runtime discovery
//! executor. It carries no runtime state and performs no discovery itself.

/// Maximum number of agent rows returned by one `list_agents` action.
///
/// A larger matching set is truncated in stable agent-id order and reports
/// `truncated: true` in the structured result.
pub const AGENT_LIST_MAX_ROWS: usize = 64;

/// Maximum length in bytes of any single string inside one `list_agents` row.
///
/// Longer values are truncated on a UTF-8 boundary so one oversized objective
/// or capability cannot push a discovery result past its documented bound.
pub const AGENT_LIST_MAX_STRING_BYTES: usize = 512;

/// Maximum capabilities rendered for one `list_agents` row.
///
/// A registered identity may declare any number of capabilities; each row
/// carries at most this many, in registration order, and sets its own
/// `truncated` signal when it omits any of them.
pub const AGENT_LIST_MAX_CAPABILITIES: usize = 16;

/// Kind of one discoverable agent within the current session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Primary pane agent; the `primary` filter and the default discovery view.
    Primary,
    /// Subagent below a primary agent; hidden from default discovery.
    Subagent,
    /// Runtime-internal controller agent, such as a routed worker.
    Internal,
}

impl AgentKind {
    /// Returns the stable wire spelling for this agent kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Subagent => "subagent",
            Self::Internal => "internal",
        }
    }
}

/// Agent-type filter accepted by the `list_agents` agent-type parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentListFilter {
    /// Primary parent agents only; the default discovery view.
    Primary,
    /// Subagents only.
    Subagent,
    /// Runtime-internal controller agents only.
    Internal,
    /// Every discovered agent kind.
    All,
}

impl AgentListFilter {
    /// Parses one agent-type parameter value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "primary" => Some(Self::Primary),
            "subagent" => Some(Self::Subagent),
            "internal" => Some(Self::Internal),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    /// Returns the stable wire spelling for this filter.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Subagent => "subagent",
            Self::Internal => "internal",
            Self::All => "all",
        }
    }

    /// Returns the default filter used when the parameter is absent.
    pub const fn default_filter() -> Self {
        Self::Primary
    }

    /// Reports whether this filter admits one discovered agent kind.
    pub const fn accepts(self, kind: AgentKind) -> bool {
        match self {
            Self::All => true,
            Self::Primary => matches!(kind, AgentKind::Primary),
            Self::Subagent => matches!(kind, AgentKind::Subagent),
            Self::Internal => matches!(kind, AgentKind::Internal),
        }
    }
}

impl Default for AgentListFilter {
    fn default() -> Self {
        Self::default_filter()
    }
}

/// Returns one bounded, control-character-free string for a discovery row.
///
/// Discovery text is untrusted peer data. Control characters are replaced with
/// spaces and the value is truncated on a UTF-8 boundary to
/// [`AGENT_LIST_MAX_STRING_BYTES`].
pub fn agent_list_bounded_text(value: &str) -> String {
    let mut bounded = String::with_capacity(value.len().min(AGENT_LIST_MAX_STRING_BYTES));
    for character in value.chars() {
        if bounded.len() + character.len_utf8() > AGENT_LIST_MAX_STRING_BYTES {
            break;
        }
        if character.is_control() {
            bounded.push(' ');
        } else {
            bounded.push(character);
        }
    }
    bounded
}

/// Reports whether one discovery string is altered by the documented bound.
///
/// A row sets its own `truncated` signal from this helper plus
/// [`AGENT_LIST_MAX_CAPABILITIES`] so a model can tell that discovery text was
/// shortened or sanitized instead of silently receiving a different value.
pub fn agent_list_text_is_truncated(value: &str) -> bool {
    value.len() > AGENT_LIST_MAX_STRING_BYTES || value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the agent-type filter matrix matches the discovery contract:
    /// the default admits primary agents only and each explicit kind widens or
    /// narrows to the requested agent kinds.
    #[test]
    fn agent_type_filter_matrix_covers_every_kind() {
        let kinds = [AgentKind::Primary, AgentKind::Subagent, AgentKind::Internal];
        assert_eq!(AgentListFilter::default(), AgentListFilter::Primary);
        assert!(
            kinds
                .into_iter()
                .all(|kind| AgentListFilter::All.accepts(kind))
        );
        for (filter, admitted) in [
            (AgentListFilter::Primary, AgentKind::Primary),
            (AgentListFilter::Subagent, AgentKind::Subagent),
            (AgentListFilter::Internal, AgentKind::Internal),
        ] {
            for kind in kinds {
                assert_eq!(
                    filter.accepts(kind),
                    kind == admitted,
                    "{filter:?} admitted {kind:?}"
                );
            }
        }
    }

    /// Verifies filter parsing accepts exactly the documented wire values.
    #[test]
    fn agent_type_filter_parses_documented_values_only() {
        for filter in [
            AgentListFilter::Primary,
            AgentListFilter::Subagent,
            AgentListFilter::Internal,
            AgentListFilter::All,
        ] {
            assert_eq!(AgentListFilter::parse(filter.as_str()), Some(filter));
        }
        assert_eq!(AgentListFilter::parse("all "), None);
        assert_eq!(AgentListFilter::parse("peers"), None);
        assert_eq!(AgentListFilter::parse(""), None);
    }

    /// Verifies discovery strings are bounded, control-free, and UTF-8 safe.
    #[test]
    fn discovery_strings_stay_within_documented_bounds() {
        let oversized = "é".repeat(AGENT_LIST_MAX_STRING_BYTES);
        let bounded = agent_list_bounded_text(&oversized);
        assert!(bounded.len() <= AGENT_LIST_MAX_STRING_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));

        let controlled = agent_list_bounded_text("peer\nmessage\u{0}text");
        assert_eq!(controlled, "peer message text");
    }
}
