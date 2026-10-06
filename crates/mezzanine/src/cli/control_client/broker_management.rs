//! Active-owner management setup for protected profiles and host invitations.
//!
//! Initial missing/refused discovery alone permits the existing direct path.
//! Invitations are validated before redemption and passed only by protected file
//! reference. Pairing consumes one local stream; fresh authenticated readiness
//! then serves the original management operation. Post-pair errors never restore
//! direct eligibility or replay redemption. No endpoint key or proof crosses IPC.

use super::*;
use crate::host::outbound_frontend::client::OutboundFrontendClient;
use std::time::Duration;

/// Returns an exact management owner and profile alias, or initial absence.
/// Destructive requests reject observer invitations before consuming first-use
/// proof; remote authority and target visibility remain independently enforced.
pub(super) async fn try_open(
    selection: &crate::cli::ControlTargetSelection,
    env: &crate::cli::CliEnv,
    destructive: bool,
) -> Result<Option<(OutboundFrontendClient, String, Duration)>> {
    let paths = env.config_paths()?;
    let layers = crate::cli::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    if !policy.outbound_enabled {
        return Err(MezError::config(
            "outbound Iroh connections are disabled by transport.iroh.outbound_enabled",
        ));
    }
    let invitation = match selection {
        crate::cli::ControlTargetSelection::IrohProfile(_) => None,
        crate::cli::ControlTargetSelection::IrohInvitation { path, save_as } => {
            let target = parse_iroh_invitation_file(path, save_as.as_deref())?;
            if target.scope() != RemoteClientProfileScope::Host {
                return Ok(None);
            }
            if destructive {
                ensure_iroh_attach_role_allowed(target.role(), "primary")?;
            }
            if let IrohControlTarget::Invitation {
                expires_at_unix_seconds,
                ..
            } = &target
                && current_unix_seconds_for_iroh_client()? > *expires_at_unix_seconds
            {
                return Err(MezError::forbidden(
                    "Iroh pairing invitation expired before connection setup",
                ));
            }
            preflight_iroh_invitation_profile(paths.root(), &target)?;
            Some(target)
        }
        _ => return Ok(None),
    };
    let client = match OutboundFrontendClient::connect(paths.root(), policy.setup_timeout).await {
        Ok(client) => client,
        Err(error)
            if matches!(
                error.io_kind(),
                Some(std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if let Some(invitation) = invitation {
        let crate::cli::ControlTargetSelection::IrohInvitation { path, save_as } = selection else {
            return Err(MezError::invalid_state(
                "outbound management invitation unavailable",
            ));
        };
        let alias = invitation.profile_name().to_string();
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()?.join(path)
        };
        client
            .pair_invitation(&absolute, save_as.as_deref(), &alias, policy.setup_timeout)
            .await?;
        // This is the next logical operation, not a replay. Disappearance here
        // is terminal even when its I/O kind ordinarily denotes discovery absence.
        let client = OutboundFrontendClient::connect(paths.root(), policy.setup_timeout).await?;
        Ok(Some((client, alias, policy.setup_timeout)))
    } else if let crate::cli::ControlTargetSelection::IrohProfile(alias) = selection {
        Ok(Some((client, alias.clone(), policy.setup_timeout)))
    } else {
        Err(MezError::invalid_state(
            "outbound management profile unavailable",
        ))
    }
}

#[cfg(test)]
mod tests;
