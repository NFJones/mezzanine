//! Auth metadata validation and TOML-compatible encoding.
//!
//! Metadata is deliberately non-secret. This module rejects obvious credential
//! markers and serializes only provider state and credential-store references.

use std::collections::BTreeMap;

use crate::error::{MezError, Result};

use super::types::{AuthCredentialKind, AuthMetadata, McpAuthMetadata, McpCredentialKind};

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

impl McpAuthMetadata {
    /// Creates non-secret metadata for one MCP credential.
    pub fn new(
        server_id: impl Into<String>,
        url_origin: impl Into<String>,
        url_fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            server_id: server_id.into(),
            credential_kind: McpCredentialKind::OAuthBearer,
            url_origin: url_origin.into(),
            url_fingerprint: url_fingerprint.into(),
            scopes: Vec::new(),
            client_id: None,
            resource: None,
            authorization_endpoint: None,
            token_endpoint: None,
            credential_store_ref: None,
            refresh_credential_store_ref: None,
            token_expires_at: None,
        }
    }

    /// Rejects raw credential material in MCP metadata before persistence.
    pub fn validate_non_secret(&self) -> Result<()> {
        let secret_markers = ["sk-", "Bearer ", "-----BEGIN", "token="];
        let fields = [
            self.server_id.as_str(),
            self.credential_kind.as_str(),
            self.url_origin.as_str(),
            self.url_fingerprint.as_str(),
            self.client_id.as_deref().unwrap_or_default(),
            self.resource.as_deref().unwrap_or_default(),
            self.authorization_endpoint.as_deref().unwrap_or_default(),
            self.token_endpoint.as_deref().unwrap_or_default(),
            self.credential_store_ref.as_deref().unwrap_or_default(),
            self.refresh_credential_store_ref
                .as_deref()
                .unwrap_or_default(),
            self.token_expires_at.as_deref().unwrap_or_default(),
        ];

        for field in fields {
            if secret_markers.iter().any(|marker| field.contains(marker)) {
                return Err(MezError::config(
                    "MCP auth metadata must not contain raw credentials or tokens",
                ));
            }
        }
        for scope in &self.scopes {
            if secret_markers.iter().any(|marker| scope.contains(marker)) {
                return Err(MezError::config(
                    "MCP auth metadata must not contain raw credentials or tokens",
                ));
            }
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

/// Encodes all provider metadata entries into a single TOML-like key=value
/// document using `[provider]` table headers.
pub(super) fn encode_all_metadata(map: &BTreeMap<String, AuthMetadata>) -> String {
    let mut text = String::new();
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    for (index, provider) in keys.iter().enumerate() {
        let metadata = &map[*provider];
        if index > 0 {
            text.push('\n');
        }
        text.push_str(&format!("[{}]\n", toml_escape(provider)));
        text.push_str(&encode_metadata(metadata));
    }
    text
}

/// Encodes one MCP auth metadata entry into non-secret key/value fields.
pub(super) fn encode_mcp_metadata(metadata: &McpAuthMetadata) -> String {
    format!(
        "server_id = \"{}\"\ncredential_kind = \"{}\"\nurl_origin = \"{}\"\nurl_fingerprint = \"{}\"\nscopes = \"{}\"\nclient_id = \"{}\"\nresource = \"{}\"\nauthorization_endpoint = \"{}\"\ntoken_endpoint = \"{}\"\ncredential_store_ref = \"{}\"\nrefresh_credential_store_ref = \"{}\"\ntoken_expires_at = \"{}\"\n",
        toml_escape(&metadata.server_id),
        toml_escape(metadata.credential_kind.as_str()),
        toml_escape(&metadata.url_origin),
        toml_escape(&metadata.url_fingerprint),
        toml_escape(&metadata.scopes.join(",")),
        toml_escape(metadata.client_id.as_deref().unwrap_or_default()),
        toml_escape(metadata.resource.as_deref().unwrap_or_default()),
        toml_escape(
            metadata
                .authorization_endpoint
                .as_deref()
                .unwrap_or_default()
        ),
        toml_escape(metadata.token_endpoint.as_deref().unwrap_or_default()),
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

/// Encodes all MCP auth metadata entries into a single TOML-like document.
pub(super) fn encode_all_mcp_metadata(map: &BTreeMap<String, McpAuthMetadata>) -> String {
    let mut text = String::new();
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    for (index, server_id) in keys.iter().enumerate() {
        let metadata = &map[*server_id];
        if index > 0 {
            text.push('\n');
        }
        text.push_str(&format!("[{}]\n", toml_escape(server_id)));
        text.push_str(&encode_mcp_metadata(metadata));
    }
    text
}

/// Decodes one MCP auth metadata entry from non-secret key/value fields.
pub(super) fn decode_mcp_metadata(data: &str) -> Result<McpAuthMetadata> {
    let mut values = BTreeMap::new();
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(MezError::config("malformed MCP auth metadata line"));
        };
        values.insert(key.trim(), parse_toml_string(value.trim())?);
    }

    let mut metadata = McpAuthMetadata::new(
        required(&values, "server_id")?,
        required(&values, "url_origin")?,
        required(&values, "url_fingerprint")?,
    );
    metadata.credential_kind = optional(&values, "credential_kind")
        .as_deref()
        .map(parse_mcp_credential_kind)
        .transpose()?
        .unwrap_or(McpCredentialKind::OAuthBearer);
    metadata.scopes = optional(&values, "scopes")
        .map(|value| parse_scope_list(&value))
        .unwrap_or_default();
    metadata.client_id = optional(&values, "client_id");
    metadata.resource = optional(&values, "resource");
    metadata.authorization_endpoint = optional(&values, "authorization_endpoint");
    metadata.token_endpoint = optional(&values, "token_endpoint");
    metadata.credential_store_ref = optional(&values, "credential_store_ref");
    metadata.refresh_credential_store_ref = optional(&values, "refresh_credential_store_ref");
    metadata.token_expires_at = optional(&values, "token_expires_at");
    metadata.validate_non_secret()?;
    Ok(metadata)
}

/// Decodes all MCP auth metadata entries into a map keyed by server id.
pub(super) fn decode_all_mcp_metadata(data: &str) -> Result<BTreeMap<String, McpAuthMetadata>> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return Ok(BTreeMap::new());
    }
    if !trimmed.contains('[') {
        return decode_mcp_metadata(data).map(|entry| {
            let mut map = BTreeMap::new();
            map.insert(entry.server_id.clone(), entry);
            map
        });
    }
    let mut map = BTreeMap::new();
    let mut current_server_id: Option<String> = None;
    let mut current_fields = String::new();
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            if let Some(server_id) = current_server_id.take() {
                let metadata = decode_mcp_metadata(&current_fields)?;
                map.insert(server_id, metadata);
                current_fields = String::new();
            }
            current_server_id = Some(parse_toml_bare_key(inner)?);
        } else if current_server_id.is_some() {
            current_fields.push_str(line);
            current_fields.push('\n');
        }
    }
    if let Some(server_id) = current_server_id {
        let metadata = decode_mcp_metadata(&current_fields)?;
        map.insert(server_id, metadata);
    }
    Ok(map)
}

/// Decodes a multi-provider metadata document into a map keyed by provider.
pub(super) fn decode_all_metadata(data: &str) -> Result<BTreeMap<String, AuthMetadata>> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return Ok(BTreeMap::new());
    }
    if !trimmed.contains('[') {
        return decode_metadata(data).map(|entry| {
            let mut map = BTreeMap::new();
            map.insert(entry.provider.clone(), entry);
            map
        });
    }
    let mut map = BTreeMap::new();
    let mut current_provider: Option<String> = None;
    let mut current_fields = String::new();
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            if let Some(provider) = current_provider.take() {
                let metadata = decode_metadata(&current_fields)?;
                map.insert(provider, metadata);
                current_fields = String::new();
            }
            current_provider = Some(parse_toml_bare_key(inner)?);
        } else if current_provider.is_some() {
            current_fields.push_str(line);
            current_fields.push('\n');
        }
    }
    if let Some(provider) = current_provider {
        let metadata = decode_metadata(&current_fields)?;
        map.insert(provider, metadata);
    }
    Ok(map)
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

/// Parses an MCP credential kind from its stable metadata value.
fn parse_mcp_credential_kind(value: &str) -> Result<McpCredentialKind> {
    McpCredentialKind::from_metadata_value(value).ok_or_else(|| {
        MezError::config(format!(
            "unsupported MCP credential kind `{value}`; expected `oauth-bearer` or `static-bearer`"
        ))
    })
}

/// Parses the compact comma-separated MCP scope list used in metadata files.
fn parse_scope_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(ToOwned::to_owned)
        .collect()
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

/// Parses a bare TOML key or table header value (without quotes).
fn parse_toml_bare_key(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(MezError::config(
            "auth metadata provider name must not be empty",
        ));
    }
    Ok(value.to_string())
}
