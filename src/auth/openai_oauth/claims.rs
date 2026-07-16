//! Provider token claim parsing and credential projection.

use super::*;

/// Parses the JSON claims from a provider JWT without validating its signature.
pub(super) fn parse_jwt_claims(token: &str) -> Option<Value> {
    let claims = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.as_bytes())
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

/// Runs the provider credential from tokens operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn provider_credential_from_tokens(tokens: TokenResponse) -> OpenAiProviderCredential {
    let id_claims = tokens
        .id_token
        .as_deref()
        .and_then(parse_jwt_claims)
        .unwrap_or_default();
    let access_claims = parse_jwt_claims(&tokens.access_token).unwrap_or_default();
    let token_expires_at = access_claims
        .get("exp")
        .or_else(|| id_claims.get("exp"))
        .and_then(Value::as_u64)
        .or_else(|| {
            tokens.expires_in.and_then(|expires_in| {
                current_unix_seconds().map(|now| now.saturating_add(expires_in))
            })
        })
        .map(|expires_at| expires_at.to_string());
    OpenAiProviderCredential {
        api_key: tokens.access_token,
        refresh_token: tokens.refresh_token,
        account_id: account_id_from_claims(&id_claims)
            .or_else(|| account_id_from_claims(&access_claims)),
        organization_id: organization_id_from_claims(&id_claims)
            .or_else(|| organization_id_from_claims(&access_claims)),
        token_expires_at,
    }
}

/// Runs the account id from claims operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn account_id_from_claims(claims: &Value) -> Option<String> {
    auth_claim_string(claims, "chatgpt_account_id")
        .or_else(|| claim_string(claims, "chatgpt_account_id"))
        .or_else(|| claim_string(claims, "account_id"))
        .or_else(|| claim_string(claims, "sub"))
}

/// Runs the organization id from claims operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn organization_id_from_claims(claims: &Value) -> Option<String> {
    auth_claim_string(claims, "organization_id")
        .or_else(|| claim_string(claims, "organization_id"))
        .or_else(|| first_organization_id(claims))
}

/// Runs the auth claim string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn auth_claim_string(claims: &Value, field: &str) -> Option<String> {
    claims
        .get("https://api.openai.com/auth")
        .and_then(|value| value.get(field))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

/// Runs the claim string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn claim_string(claims: &Value, field: &str) -> Option<String> {
    claims
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

/// Runs the first organization id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn first_organization_id(claims: &Value) -> Option<String> {
    let organizations = claims
        .get("https://api.openai.com/auth")
        .and_then(|value| value.get("organizations"))
        .or_else(|| claims.get("organizations"))?;
    organizations.as_array()?.iter().find_map(|organization| {
        organization
            .get("organization_id")
            .or_else(|| organization.get("id"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    })
}

/// Runs the deserialize device interval operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deserialize_device_interval<'de, D>(
    deserializer: D,
) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Number(number) => Ok(number.as_u64().unwrap_or(5)),
        Value::String(text) => text.trim().parse().map_err(serde::de::Error::custom),
        _ => Ok(5),
    }
}

/// Runs the deserialize optional u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deserialize_optional_u64<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        Some(Value::Number(number)) => Ok(number.as_u64()),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => text
            .trim()
            .parse()
            .map(Some)
            .map_err(serde::de::Error::custom),
        Some(_) | None => Ok(None),
    }
}

/// Runs the current unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn current_unix_seconds() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

/// Runs the oauth error message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn oauth_error_message(error_code: &str, error_description: Option<&str>) -> String {
    let base = match error_code {
        "access_denied" => "access was denied",
        "invalid_request" => "the authorization request was invalid",
        "server_error" => "the authorization server returned an error",
        other => other,
    };
    match error_description.filter(|description| !description.trim().is_empty()) {
        Some(description) => format!("{base}: {description}"),
        None => base.to_string(),
    }
}
