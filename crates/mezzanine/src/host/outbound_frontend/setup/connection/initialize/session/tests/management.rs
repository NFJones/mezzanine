//! Invitation administration acceptance beside live shared-owner attachments.
//!
//! This helper holds the large public CLI-dispatch future outside the sibling
//! fixture's stack. It resumes existing invitation authority through the
//! retained endpoint and exposes only sanitized management settlement. No task,
//! replacement endpoint or retry is introduced by this storage boundary.

use crate::host::iroh::HostIrohRuntime;
use crate::security::remote::RemotePairingInvitation;
use secrecy::ExposeSecret;
use std::path::Path;

/// Executes one public invitation list or kill operation using exact host
/// authority. The surrounding fixture verifies lease state, session count,
/// identity exclusion and continued sibling activity after this exchange.
pub(super) async fn exchange(
    root: &Path,
    host: &HostIrohRuntime,
    invitation: &RemotePairingInvitation,
    env: &crate::cli::CliEnv,
    target: Option<&str>,
) -> serde_json::Value {
    let alias = if target.is_some() {
        "management-kill"
    } else {
        "management-list"
    };
    let path = root.join(format!("{alias}.json"));
    crate::security::remote::write_remote_invitation_file_new(
        &path,
        serde_json::json!({
            "format_version":1,"profile_name":alias,
            "server_addr":host.endpoint_addr().unwrap(),"role":"primary",
            "profile_scope":"host","token":invitation.token.expose_secret(),
            "expires_at_unix_seconds":invitation.expires_at_unix_seconds
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let mut args = vec![
        "mez".into(),
        "--iroh-invite-file".into(),
        path.to_str().unwrap().into(),
        "--json".into(),
    ];
    if let Some(target) = target {
        args.extend(["kill".into(), "--force".into(), target.into()]);
    } else {
        args.push("list".into());
    }
    let mut output = Vec::new();
    let mut error = Vec::new();
    let code = Box::pin(crate::cli::run_with(
        args,
        env.clone(),
        false,
        &mut output,
        &mut error,
    ))
    .await
    .unwrap();
    assert_eq!(code, 0);
    assert!(error.is_empty());
    assert!(!String::from_utf8_lossy(&output).contains(invitation.token.expose_secret()));
    serde_json::from_slice(&output).unwrap()
}
