//! PKCE authorization URL, token, form, and query helpers.

use super::{MezError, OPENAI_OAUTH_SCOPE, PkceCodes, Result};
use base64::Engine;
use rand::Rng;
use sha2::Digest;
use std::collections::BTreeMap;

/// Builds the provider authorization URL for a PKCE login.
pub(super) fn build_authorize_url(
    issuer: &str,
    client_id: &str,
    redirect_uri: &str,
    pkce: &PkceCodes,
    state: &str,
) -> String {
    let query = form_body(&[
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", OPENAI_OAUTH_SCOPE),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", "mezzanine_cli"),
    ]);
    format!("{}/oauth/authorize?{query}", issuer.trim_end_matches('/'))
}

/// Runs the generate pkce operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn generate_pkce() -> PkceCodes {
    let code_verifier = random_urlsafe_token(64);
    let digest = sha2::Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    PkceCodes {
        code_verifier,
        code_challenge,
    }
}

/// Runs the random urlsafe token operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn random_urlsafe_token(length: usize) -> String {
    let mut bytes = vec![0u8; length];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Runs the form body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn form_body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Runs the parse query operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_query(query: &str) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = urlencoding::decode(key)
            .map_err(|_| MezError::invalid_state("OpenAI browser callback had malformed query"))?;
        let value = urlencoding::decode(value)
            .map_err(|_| MezError::invalid_state("OpenAI browser callback had malformed query"))?;
        values.insert(key.into_owned(), value.into_owned());
    }
    Ok(values)
}
