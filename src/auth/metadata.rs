//! Auth metadata validation and TOML-compatible encoding.
//!
//! Metadata is deliberately non-secret. This module rejects obvious credential
//! markers and serializes only provider state and credential-store references.

use std::collections::BTreeMap;

use crate::error::{MezError, Result};

use super::types::{AuthCredentialKind, AuthMetadata};

impl AuthMetadata {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(provider: impl Into<String>, selected_model_profile: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            credential_kind: AuthCredentialKind::ApiKey,
            account_id: None,
            organization_id: None,
            selected_model_profile: selected_model_profile.into(),
            credential_store_ref: None,
            refresh_credential_store_ref: None,
            token_expires_at: None,
        }
    }

    /// Runs the validate non secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate_non_secret(&self) -> Result<()> {
        let secret_markers = ["sk-", "Bearer ", "-----BEGIN", "token="];
        let fields = [
            self.provider.as_str(),
            self.credential_kind.as_str(),
            self.account_id.as_deref().unwrap_or_default(),
            self.organization_id.as_deref().unwrap_or_default(),
            self.selected_model_profile.as_str(),
            self.credential_store_ref.as_deref().unwrap_or_default(),
            self.refresh_credential_store_ref
                .as_deref()
                .unwrap_or_default(),
            self.token_expires_at.as_deref().unwrap_or_default(),
        ];

        if fields
            .iter()
            .any(|field| secret_markers.iter().any(|marker| field.contains(marker)))
        {
            return Err(MezError::config(
                "auth metadata must not contain raw provider credentials or tokens",
            ));
        }

        Ok(())
    }
}

/// Runs the encode metadata operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn encode_metadata(metadata: &AuthMetadata) -> String {
    format!(
        "provider = \"{}\"\ncredential_kind = \"{}\"\naccount_id = \"{}\"\norganization_id = \"{}\"\nselected_model_profile = \"{}\"\ncredential_store_ref = \"{}\"\nrefresh_credential_store_ref = \"{}\"\ntoken_expires_at = \"{}\"\n",
        toml_escape(&metadata.provider),
        toml_escape(metadata.credential_kind.as_str()),
        toml_escape(metadata.account_id.as_deref().unwrap_or_default()),
        toml_escape(metadata.organization_id.as_deref().unwrap_or_default()),
        toml_escape(&metadata.selected_model_profile),
        toml_escape(metadata.credential_store_ref.as_deref().unwrap_or_default()),
        toml_escape(
            metadata
                .refresh_credential_store_ref
                .as_deref()
                .unwrap_or_default()
        ),
        toml_escape(metadata.token_expires_at.as_deref().unwrap_or_default())
    )
}

/// Runs the decode metadata operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn decode_metadata(data: &str) -> Result<AuthMetadata> {
    let mut values = BTreeMap::new();
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(MezError::config("malformed auth metadata line"));
        };
        values.insert(key.trim(), parse_toml_string(value.trim())?);
    }

    let mut metadata = AuthMetadata::new(
        required(&values, "provider")?,
        required(&values, "selected_model_profile")?,
    );
    metadata.account_id = optional(&values, "account_id");
    metadata.organization_id = optional(&values, "organization_id");
    metadata.credential_store_ref = optional(&values, "credential_store_ref");
    metadata.refresh_credential_store_ref = optional(&values, "refresh_credential_store_ref");
    metadata.token_expires_at = optional(&values, "token_expires_at");
    metadata.credential_kind = optional(&values, "credential_kind")
        .as_deref()
        .map(parse_credential_kind)
        .transpose()?
        .unwrap_or_else(|| infer_credential_kind(&metadata));
    metadata.validate_non_secret()?;
    Ok(metadata)
}

/// Runs the parse credential kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_credential_kind(value: &str) -> Result<AuthCredentialKind> {
    AuthCredentialKind::from_metadata_value(value).ok_or_else(|| {
        MezError::config(format!(
            "unsupported auth credential kind `{value}`; expected `api-key` or `chatgpt`"
        ))
    })
}

/// Runs the infer credential kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn infer_credential_kind(metadata: &AuthMetadata) -> AuthCredentialKind {
    if metadata.refresh_credential_store_ref.is_some()
        || metadata.account_id.is_some()
        || metadata.organization_id.is_some()
        || metadata.token_expires_at.is_some()
    {
        AuthCredentialKind::ChatGpt
    } else {
        AuthCredentialKind::ApiKey
    }
}

/// Runs the required operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required(values: &BTreeMap<&str, String>, key: &str) -> Result<String> {
    values
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| MezError::config(format!("missing auth metadata field `{key}`")))
}

/// Runs the optional operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional(values: &BTreeMap<&str, String>, key: &str) -> Option<String> {
    values.get(key).filter(|value| !value.is_empty()).cloned()
}

/// Runs the parse toml string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_toml_string(value: &str) -> Result<String> {
    let Some(body) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err(MezError::config(
            "auth metadata values must be TOML strings",
        ));
    };
    let mut output = String::new();
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let escaped = chars
                .next()
                .ok_or_else(|| MezError::config("trailing escape in auth metadata"))?;
            output.push(match escaped {
                '"' => '"',
                '\\' => '\\',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
        } else {
            output.push(ch);
        }
    }
    Ok(output)
}

/// Runs the toml escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn toml_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}
