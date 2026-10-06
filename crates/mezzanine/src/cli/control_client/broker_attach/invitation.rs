//! Active-owner pairing followed by exact host-session attachment.
//!
//! Protected invitation scope, role, expiry and alias conflict checks precede
//! redemption. Only initial absent/refused broker discovery leaves the existing
//! direct path eligible. Once pairing is submitted, every later failure is
//! terminal: reconnecting local IPC is not permission to acquire another endpoint
//! or replay redemption/creation. Original prepared routing and mutation keys
//! survive this two-connection handoff; neither connection exports private proof.
//! Explicit X11 preparation precedes redemption and retains client-local cleanup.
//! This active-owner path starts no broker and does not activate legacy pairing.

use super::*;

/// Pairs a host invitation through an active owner, then submits the original
/// attachment once on fresh authenticated IPC. Local reconnect is necessary
/// because pairing consumes its management connection; it does not replay work.
#[allow(
    clippy::too_many_arguments,
    reason = "invitation evidence, prepared routing and terminal intent are independent handoff inputs"
)]
pub(super) async fn try_open(
    path: &Path,
    save_as: Option<&str>,
    env: &crate::cli::CliEnv,
    role: &str,
    routing: &IrohSessionRouting,
    columns: u16,
    rows: u16,
    term: &str,
    x11: Option<(crate::runtime::x11::X11ForwardingMode, bool)>,
) -> Result<Option<BrokerAttachment>> {
    Box::pin(try_open_with_preparation(
        path,
        save_as,
        env,
        role,
        routing,
        columns,
        rows,
        term,
        x11,
        crate::cli::x11::prepare_x11_client,
    ))
    .await
}

/// Shares production handoff with an explicit local credential-preparation seam.
/// All role/envelope checks precede invoking preparation, and preparation precedes
/// redemption. The injected future changes no pairing or cleanup ownership.
#[allow(
    clippy::too_many_arguments,
    reason = "exact invitation, attachment and credential inputs are independent"
)]
async fn try_open_with_preparation<P, W>(
    path: &Path,
    save_as: Option<&str>,
    env: &crate::cli::CliEnv,
    role: &str,
    routing: &IrohSessionRouting,
    columns: u16,
    rows: u16,
    term: &str,
    x11: Option<(crate::runtime::x11::X11ForwardingMode, bool)>,
    prepare: P,
) -> Result<Option<BrokerAttachment>>
where
    P: FnOnce(crate::runtime::x11::X11ForwardingMode) -> W,
    W: std::future::Future<Output = Result<crate::cli::x11::PreparedX11Client>>,
{
    let paths = env.config_paths()?;
    let target = parse_iroh_invitation_file(path, save_as)?;
    ensure_iroh_attach_role_allowed(target.role(), role)?;
    routing.validate_target_scope(target.scope())?;
    let IrohControlTarget::Invitation {
        profile_name,
        scope,
        expires_at_unix_seconds,
        ..
    } = &target
    else {
        return Err(MezError::invalid_state(
            "invitation attachment evidence unavailable",
        ));
    };
    if *scope != RemoteClientProfileScope::Host {
        return Ok(None);
    }
    let layers = crate::cli::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    if !policy.outbound_enabled {
        return Err(MezError::config(
            "outbound Iroh connections are disabled by transport.iroh.outbound_enabled",
        ));
    }
    if current_unix_seconds_for_iroh_client()? > *expires_at_unix_seconds {
        return Err(MezError::forbidden(
            "Iroh pairing invitation expired before connection setup",
        ));
    }
    preflight_iroh_invitation_profile(paths.root(), &target)?;
    // Validate grammar, geometry and local clipboard policy before consuming
    // first-use proof; malformed attachment input must not redeem an invitation.
    let initialize = initialize_params(role, routing, columns, rows, term)?;
    let clipboard = crate::runtime::runtime_client_host_clipboard_from_config(&structured)?;
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
    OutboundFrontendClient::validate_session_setup(
        profile_name,
        &initialize,
        columns,
        rows,
        policy.setup_timeout,
    )?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let prepared = if let Some(request) = x11 {
        super::x11::validate_local(
            profile_name,
            &initialize,
            &policy,
            columns,
            rows,
            role,
            request,
        )?;
        Some(prepare(request.0).await?)
    } else {
        None
    };
    let paired = async {
        client
            .pair_invitation(&absolute, save_as, profile_name, policy.setup_timeout)
            .await?;
        // Absence after redemption is changed owner state, never direct eligibility.
        OutboundFrontendClient::connect(paths.root(), policy.setup_timeout).await
    }
    .await;
    let client = match paired {
        Ok(client) => client,
        Err(error) => {
            if let Some(prepared) = prepared {
                let _ = prepared.close().await;
            }
            return Err(error);
        }
    };
    if let Some(prepared) = prepared {
        return super::x11::finish_prepared(
            client,
            profile_name,
            initialize,
            clipboard,
            &policy,
            columns,
            rows,
            prepared,
            x11.is_some_and(|(_, takeover)| takeover),
        )
        .await
        .map(Some);
    }
    let (session, _) = Box::pin(client.start_session(
        profile_name,
        initialize,
        columns,
        rows,
        policy.setup_timeout,
    ))
    .await?;
    Ok(Some(BrokerAttachment {
        session,
        clipboard,
        budget: policy.setup_timeout,
        primary: role == "primary",
        x11: None,
    }))
}

#[cfg(test)]
mod tests;
