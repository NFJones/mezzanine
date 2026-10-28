//! Refusal of known shared Codex servers without requesting-client association.
//!
//! A daemon can inherit a pane's environment and ancestry while serving another
//! client's session. Native parent/root proof alone does not establish that
//! client. Bounded native argv is inspected only to refuse this known topology;
//! it never grants source authority. Private arguments stay local and are never
//! logged, transported or persisted. Unknown observations fail closed. The
//! caller independently fences the original native incarnation before and after.

use super::*;

/// Fixed capability gap; no endpoint, argument, path or callback content escapes.
const SHARED: &str = "unavailable-codex-native-client-session-association";

use crate::integrations::bootstrap::codex::topology::shared_server;

/// Refuses known shared modes and unknown native argument observations without
/// inferring ownership from labels, inherited MEZ, unique connected peers or cwd.
/// Success only means this refusal did not apply; full native authorization and
/// request-origin checks remain mandatory and cannot be replaced by argv.
pub(super) fn require_client_scope(pid: u32) -> Result<()> {
    let refused = mez_mux::process::process_argument_refusal_for_pid(pid, shared_server)
        .ok_or_else(|| MezError::forbidden("unavailable-codex-native-process-topology"))?;
    if refused {
        return Err(MezError::forbidden(SHARED));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Listener forms and managed mode refuse shared attribution even with an
    /// aliased executable. Ordinary TUI/shim argv and single-client stdio remain
    /// eligible for the separate native proof; a prompt containing one combined
    /// flag string does not become a daemon, and private values never escape.
    #[test]
    fn codex_topology_shared_listener_modes_do_not_borrow_launching_client() {
        for arguments in [
            vec!["alias", "app-server", "--listen", "unix://PRIVATE"],
            vec!["alias", "app-server", "--listen=ws://PRIVATE"],
            vec!["alias", "app-server", "--managed-daemon"],
            vec!["alias", "app-server", "--listen"],
        ] {
            let bytes = arguments
                .iter()
                .map(|value| value.as_bytes().to_vec())
                .collect::<Vec<_>>();
            assert!(shared_server(&bytes));
        }
        for arguments in [
            vec!["shim", "resume", "--last"],
            vec!["alias", "app-server"],
            vec!["alias", "app-server", "--listen", "stdio://"],
            vec!["alias", "app-server", "--listen=stdio://"],
            vec!["alias", "app-server --listen unix://PRIVATE"],
        ] {
            let bytes = arguments
                .iter()
                .map(|value| value.as_bytes().to_vec())
                .collect::<Vec<_>>();
            assert!(!shared_server(&bytes));
        }
        assert!(!SHARED.contains("PRIVATE"));
    }
}
