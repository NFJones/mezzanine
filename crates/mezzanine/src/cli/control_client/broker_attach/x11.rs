//! Paired-profile X11 setup with client-local credential cleanup ownership.
//!
//! This adapter runs only after authenticated broker discovery. Original routing
//! and mutation identity remain unchanged. Local preparation never exports the
//! real credential or X destination. Failed setup/discovery explicitly cleans the
//! prepared lease, preserving the causal error; cancellation uses its Drop guard.
//! Success transfers one matching-capacity opener and credential owner to the
//! concrete attachment foreground. No direct fallback, endpoint acquisition or
//! initialization replay is permitted after entry to this operation.

use super::*;

/// Prepares local credentials only after grammar, role and frame-budget checks.
/// Host authorization independently governs requested trust and takeover.
#[allow(
    clippy::too_many_arguments,
    reason = "retained session, local policy, geometry and typed X11 intent are independent inputs"
)]
pub(super) async fn prepare_attachment(
    client: OutboundFrontendClient,
    alias: &str,
    params: serde_json::Value,
    clipboard: crate::host::terminal::HostClipboard,
    policy: &crate::runtime::RuntimeIrohTransportPolicy,
    columns: u16,
    rows: u16,
    role: &str,
    (mode, takeover): (crate::runtime::x11::X11ForwardingMode, bool),
) -> Result<BrokerAttachment> {
    if role != "primary" {
        return Err(MezError::forbidden(
            "broker X11 requires primary attachment",
        ));
    }
    let mut preflight = params.clone();
    install_offer(
        &mut preflight,
        &crate::runtime::x11::X11ForwardingOffer {
            version: crate::runtime::x11::X11_FORWARDING_VERSION,
            mode,
            auth_protocol: crate::runtime::x11::X11AuthProtocol::MitMagicCookie1,
            fake_cookie: crate::runtime::x11::X11Cookie::new([0; 16]),
            takeover,
        },
    );
    OutboundFrontendClient::validate_session_setup(
        alias,
        &preflight,
        columns,
        rows,
        policy.setup_timeout,
    )?;
    let prepared = crate::cli::x11::prepare_x11_client(mode).await?;
    finish_prepared(
        client, alias, params, clipboard, policy, columns, rows, prepared, takeover,
    )
    .await
}

/// Sends only the fake-cookie offer, then transfers cleanup ownership on success.
/// Every post-preparation error closes the generated local lease without replay.
#[allow(
    clippy::too_many_arguments,
    reason = "exact prepared transport and local credential lifetime must stay explicit"
)]
async fn finish_prepared(
    client: OutboundFrontendClient,
    alias: &str,
    mut params: serde_json::Value,
    clipboard: crate::host::terminal::HostClipboard,
    policy: &crate::runtime::RuntimeIrohTransportPolicy,
    columns: u16,
    rows: u16,
    prepared: crate::cli::x11::PreparedX11Client,
    takeover: bool,
) -> Result<BrokerAttachment> {
    let offer = prepared.offer(takeover);
    install_offer(&mut params, &offer);
    let result = async {
        let (session, _) =
            Box::pin(client.start_session(alias, params, columns, rows, policy.setup_timeout))
                .await?;
        let (session, name) = session.discover_x11(policy.setup_timeout).await?;
        let name = name.ok_or_else(|| {
            MezError::invalid_state("broker X11 publication unavailable after setup")
        })?;
        let limit = policy.x11.max_connections_per_route;
        let opener = session.x11_channel_opener(&name, limit)?;
        Ok::<_, MezError>((session, opener, limit))
    }
    .await;
    match result {
        Ok((session, opener, limit)) => Ok(BrokerAttachment {
            session,
            clipboard,
            budget: policy.setup_timeout,
            primary: true,
            x11: Some(BrokerX11 {
                prepared,
                opener,
                limit,
                budget: policy.x11.setup_timeout,
            }),
        }),
        Err(error) => {
            let _ = prepared.close().await;
            Err(error)
        }
    }
}

/// Uses the same fixed-size, network-safe offer for preflight and actual setup.
/// Only fake credential bytes are encoded; no local destination is included.
fn install_offer(params: &mut serde_json::Value, offer: &crate::runtime::x11::X11ForwardingOffer) {
    params["x11_forwarding"] = serde_json::json!({
        "version":offer.version,"mode":offer.mode.as_str(),
        "auth_protocol":offer.auth_protocol.as_str(),"takeover":offer.takeover,
        "fake_cookie_base64":base64::engine::general_purpose::STANDARD.encode(offer.fake_cookie.as_bytes()),
    });
}

#[cfg(test)]
mod tests;
