//! Prompt-facing memory context contracts.
//!
//! This module defines the bounded memory data the agent harness needs while
//! assembling model context. Durable record lifecycle, validation, retrieval,
//! and persistence remain responsibilities of the product adapter.

/// Identifies the scope shown for one prompt-facing memory record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryContextScope {
    /// Memory shared across projects and sessions.
    Global,
    /// Memory associated with one project root.
    Project {
        /// Project root used in the model-visible scope label.
        root: String,
    },
    /// Memory associated with one session.
    Session {
        /// Stable session identifier.
        session_id: String,
    },
    /// Memory associated with one window in a session.
    Window {
        /// Stable session identifier.
        session_id: String,
        /// Stable window identifier.
        window_id: String,
    },
    /// Memory associated with one pane in a session.
    Pane {
        /// Stable session identifier.
        session_id: String,
        /// Stable pane identifier.
        pane_id: String,
    },
    /// Memory associated with one agent in a session.
    Agent {
        /// Stable session identifier.
        session_id: String,
        /// Stable agent identifier.
        agent_id: String,
    },
}

impl MemoryContextScope {
    /// Returns the compact model-visible label for this scope.
    pub fn summary(&self) -> String {
        match self {
            Self::Global => "global".to_string(),
            Self::Project { root } => format!("project {root}"),
            Self::Session { session_id } => format!("session {session_id}"),
            Self::Window {
                session_id,
                window_id,
            } => format!("window {session_id}/{window_id}"),
            Self::Pane {
                session_id,
                pane_id,
            } => format!("pane {session_id}/{pane_id}"),
            Self::Agent {
                session_id,
                agent_id,
            } => format!("agent {session_id}/{agent_id}"),
        }
    }
}

/// Carries the bounded fields needed to inject one memory into model context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryContextRecord {
    /// Stable record identifier used for deterministic ordering and labels.
    pub id: String,
    /// Scope rendered in the context block label.
    pub scope: MemoryContextScope,
    /// Last update time used as the secondary ordering key.
    pub updated_at_unix_seconds: u64,
    /// Retrieval priority used as the primary ordering key.
    pub priority: u8,
    /// Model-visible memory content.
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::MemoryContextScope;

    #[test]
    /// Scope summaries preserve the stable identifiers needed to explain memory provenance.
    fn memory_context_scope_summaries_are_stable() {
        assert_eq!(MemoryContextScope::Global.summary(), "global");
        assert_eq!(
            MemoryContextScope::Pane {
                session_id: "$1".to_string(),
                pane_id: "%2".to_string(),
            }
            .summary(),
            "pane $1/%2"
        );
        assert_eq!(
            MemoryContextScope::Agent {
                session_id: "$1".to_string(),
                agent_id: "agent-3".to_string(),
            }
            .summary(),
            "agent $1/agent-3"
        );
    }
}
