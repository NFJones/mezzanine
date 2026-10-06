//! Paired host-profile attachment setup through a protected shared broker.
//!
//! Preserves the caller's prepared routing intent and invocation key. This owner
//! never acquires an endpoint identity or pairs a principal. Eligible pinned
//! direct profiles may use elected startup with caller-retained child evidence.
//! Only initial missing/refused discovery permits startup or direct eligibility;
//! after attempted startup or connected readiness, failure is terminal. After local
//! readiness, failed setup may already have created a session and is never
//! retried through another endpoint. Explicit paired-profile X11 retains client-local
//! credential and channel cleanup; invitation X11 remains separately gated.

use super::*;
use crate::host::outbound_frontend::client::{OutboundFrontendClient, OutboundSessionClient};

mod invitation;
mod x11;

/// Retained session plus the client machine's independently selected clipboard
/// adapter and finite request budget. No remote credentials leave broker setup.
pub(in crate::cli) struct BrokerAttachment {
    pub(in crate::cli) session: OutboundSessionClient,
    pub(in crate::cli) clipboard: crate::host::terminal::HostClipboard,
    pub(in crate::cli) budget: std::time::Duration,
    pub(in crate::cli) primary: bool,
    pub(in crate::cli) x11: Option<BrokerX11>,
}

/// Client-local credentials and one exclusively owned channel opener. Matching
/// limits prevent independent opener/supervisor pools from invalidating siblings.
pub(in crate::cli) struct BrokerX11 {
    pub(in crate::cli) prepared: crate::cli::x11::PreparedX11Client,
    pub(in crate::cli) opener: crate::host::outbound_frontend::client::X11ChannelOpener,
    pub(in crate::cli) limit: usize,
    pub(in crate::cli) budget: std::time::Duration,
}

/// Tries existing broker reuse for a paired host profile. None means no admitted
/// broker operation occurred and the established direct transport remains valid.
/// Errors are terminal and contain no profile proof or remote response content.
#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "exact target, caller routing, role, geometry and explicit X11 intent are independent attachment inputs"
)]
pub(in crate::cli) async fn try_open(
    target: &super::super::ControlTargetSelection,
    env: &super::super::CliEnv,
    role: &str,
    routing: &IrohSessionRouting,
    columns: u16,
    rows: u16,
    term: &str,
    x11: bool,
) -> Result<Option<BrokerAttachment>> {
    try_open_inner(
        target,
        env,
        role,
        routing,
        columns,
        rows,
        term,
        x11.then_some((crate::runtime::x11::X11ForwardingMode::Untrusted, false)),
        None,
        None,
    )
    .await
}

/// Permits elected first-owner startup for eligible paired host attachments.
/// The caller retains launched-child evidence on failure or cancellation. Once
/// startup is attempted, errors cannot authorize direct endpoint fallback.
#[allow(
    clippy::too_many_arguments,
    reason = "attachment intent and caller-owned launch evidence are independent setup inputs"
)]
pub(in crate::cli) async fn try_open_starting(
    target: &super::super::ControlTargetSelection,
    env: &super::super::CliEnv,
    role: &str,
    routing: &IrohSessionRouting,
    columns: u16,
    rows: u16,
    term: &str,
    x11: Option<(crate::runtime::x11::X11ForwardingMode, bool)>,
    child: &mut Option<crate::cli::remote::broker::launch::LaunchedBroker>,
) -> Result<Option<BrokerAttachment>> {
    try_open_inner(
        target,
        env,
        role,
        routing,
        columns,
        rows,
        term,
        x11,
        Some(child),
        None,
    )
    .await
}

/// Shares policy/profile validation and one original-key session setup. The
/// optional child slot authorizes only the reviewed elected startup composition.
#[allow(
    clippy::too_many_arguments,
    reason = "exact routing, geometry and optional retained launcher ownership are independent"
)]
async fn try_open_inner(
    target: &super::super::ControlTargetSelection,
    env: &super::super::CliEnv,
    role: &str,
    routing: &IrohSessionRouting,
    columns: u16,
    rows: u16,
    term: &str,
    x11: Option<(crate::runtime::x11::X11ForwardingMode, bool)>,
    child: Option<&mut Option<crate::cli::remote::broker::launch::LaunchedBroker>>,
    executable: Option<&Path>,
) -> Result<Option<BrokerAttachment>> {
    if let super::super::ControlTargetSelection::IrohInvitation { path, save_as } = target {
        return Box::pin(invitation::try_open(
            path,
            save_as.as_deref(),
            env,
            role,
            routing,
            columns,
            rows,
            term,
            x11.is_some(),
        ))
        .await;
    }
    let super::super::ControlTargetSelection::IrohProfile(alias) = target else {
        return Ok(None);
    };
    let paths = env.config_paths()?;
    let layers = super::super::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    if !policy.outbound_enabled {
        return Err(MezError::config(
            "outbound Iroh connections are disabled by transport.iroh.outbound_enabled",
        ));
    }
    let profile = RemoteClientProfileStore::under_config_root(paths.root())
        .load(alias)?
        .ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "Iroh client profile not found",
            )
        })?;
    ensure_iroh_attach_role_allowed(profile.role, role)?;
    routing.validate_target_scope(profile.scope)?;
    if profile.scope != RemoteClientProfileScope::Host {
        return Ok(None);
    }
    let params = initialize_params(role, routing, columns, rows, term)?;
    let clipboard = crate::runtime::runtime_client_host_clipboard_from_config(&structured)?;
    let client = match OutboundFrontendClient::connect(paths.root(), policy.setup_timeout).await {
        Ok(client) => client,
        Err(error)
            if matches!(
                error.io_kind(),
                Some(std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
            ) =>
        {
            let Some(child) = child else {
                return Ok(None);
            };
            // The shared transport currently qualifies only pinned direct
            // routes. Leave other route policies on their established path
            // before starting any owner, never after uncertain startup.
            if !policy.direct_connections
                || policy.port_mapping
                || !matches!(
                    policy.address_lookup,
                    crate::runtime::RuntimeIrohAddressLookupPolicy::Disabled
                        | crate::runtime::RuntimeIrohAddressLookupPolicy::Local
                )
                || !matches!(
                    policy.relay,
                    crate::runtime::RuntimeIrohRelayPolicy::Disabled
                )
                || profile.server_addr.ip_addrs().next().is_none()
                || profile.server_addr.relay_urls().next().is_some()
            {
                return Ok(None);
            }
            if let Some(executable) = executable {
                Box::pin(crate::cli::remote::broker::launch::connect_owned(
                    executable,
                    env,
                    policy.setup_timeout,
                    child,
                ))
                .await?
            } else {
                Box::pin(crate::cli::remote::broker::connect_cli(
                    env,
                    policy.setup_timeout,
                    child,
                ))
                .await?
            }
        }
        Err(error) => return Err(error),
    };
    if let Some(request) = x11 {
        return x11::prepare_attachment(
            client, alias, params, clipboard, &policy, columns, rows, role, request,
        )
        .await
        .map(Some);
    }
    let (session, _) =
        Box::pin(client.start_session(alias, params, columns, rows, policy.setup_timeout)).await?;
    Ok(Some(BrokerAttachment {
        session,
        clipboard,
        budget: policy.setup_timeout,
        primary: role == "primary",
        x11: None,
    }))
}

/// Builds credential-free protocol-v3 parameters from the same routing owner as
/// direct attach. Primary requests explicit clipboard-v2; observers request v1.
/// No new key is allocated here, and there is no existing-session Create fallback.
fn initialize_params(
    role: &str,
    routing: &IrohSessionRouting,
    columns: u16,
    rows: u16,
    term: &str,
) -> Result<serde_json::Value> {
    if !matches!(role, "primary" | "observer")
        || !(1..=4096).contains(&columns)
        || !(1..=4096).contains(&rows)
    {
        return Err(MezError::invalid_args(
            "outbound attachment role or geometry invalid",
        ));
    }
    let mut client = serde_json::json!({"name":"remote-cli","interactive":true,
        "terminal":{"columns":columns,"rows":rows,"term":term}});
    if let Some(name) = routing.session_name() {
        client["metadata"] = serde_json::json!({"session_name":name});
    }
    let mut params = serde_json::json!({"client_name":"remote-cli","requested_version":3,
        "requested_role":role,"detach_primary_on_disconnect":role == "primary",
        "event_stream_version":if role == "primary" { 2 } else { 1 },
        "session_intent":routing.intent(),"client":client});
    if let Some(target) = routing.session_target() {
        params["session_target"] = target;
    }
    if let Some(key) = routing.idempotency_key() {
        params["idempotency_key"] = serde_json::json!(key);
    }
    crate::control::initialize_params_from_json(&params.to_string())?;
    Ok(params)
}

#[cfg(test)]
mod tests;
