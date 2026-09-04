//! Canonical provider-native request continuity enforcement.
//!
//! Provider adapters render their own request bodies, but all adapters share
//! one invariant: within an unchanged epoch, the complete model-visible input
//! must extend the prior input by exact canonical items. This module compares
//! transient canonical renderings and retains only typed epoch metadata on the
//! request, never prior prompt or provider-input bytes.

use crate::{
    ContextEpochIdentity, ContextEpochTransition, ContextPlacement, ModelRequest,
    ProviderRequestAssemblyError, ProviderRequestAssemblyResult,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Exact canonical native projection used only while preparing one send.
struct ProviderNativeRequestProjection {
    envelope: Vec<u8>,
    envelope_value: Value,
    input: Vec<Vec<u8>>,
}

/// Canonical native request material compared before one provider dispatch.
///
/// Both bodies are deterministic projections rebuilt from durable chronology.
/// This input carries no retained provider-visible state beyond the current
/// comparison operation.
pub(crate) struct ProviderNativeRequestContinuity<'a> {
    pub(crate) cache_namespace: &'a str,
    pub(crate) provider_label: &'a str,
    pub(crate) current_api_shape: &'a str,
    pub(crate) previous_api_shape: &'a str,
    pub(crate) input_field: &'a str,
    pub(crate) current_body: &'a str,
    pub(crate) previous_body: Option<&'a str>,
}

/// Enforces append-only native input for one comparable provider request.
///
/// The supplied bodies must be deterministic provider-native request bodies
/// using the same input field. The function derives all compared bytes from
/// the supplied durable requests and stores only typed epoch metadata on
/// `request`.
pub(crate) fn prepare_provider_native_request_prefix_extension(
    request: &mut ModelRequest,
    previous: Option<&ModelRequest>,
    continuity: ProviderNativeRequestContinuity<'_>,
) -> ProviderRequestAssemblyResult<()> {
    let current = provider_native_request_projection(
        continuity.current_body,
        continuity.input_field,
        continuity.provider_label,
    )?;
    let current_epoch = provider_native_epoch_identity(
        request,
        continuity.cache_namespace,
        continuity.current_api_shape,
        &current.envelope_value,
    )?;
    let Some(previous) = previous else {
        request
            .messages
            .set_provider_request_epoch(crate::context::ProviderRequestEpoch {
                context_epoch: current_epoch,
                epoch_transition: ContextEpochTransition::Initial,
            });
        return Ok(());
    };
    let previous_body = continuity.previous_body.ok_or_else(|| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "{} request continuity is missing the prior canonical request body",
            continuity.provider_label,
        ))
    })?;
    let previous_projection = provider_native_request_projection(
        previous_body,
        continuity.input_field,
        continuity.provider_label,
    )?;
    let previous_epoch = previous
        .messages
        .provider_request_epoch()
        .map(|epoch| epoch.context_epoch.clone())
        .unwrap_or(provider_native_epoch_identity(
            previous,
            continuity.cache_namespace,
            continuity.previous_api_shape,
            &previous_projection.envelope_value,
        )?);
    if previous_epoch != current_epoch {
        let transition = ContextEpochTransition::Changed(
            previous_epoch
                .changed_component(&current_epoch)
                .expect("different provider epochs must identify a changed component"),
        );
        request
            .messages
            .set_provider_request_epoch(crate::context::ProviderRequestEpoch {
                context_epoch: current_epoch,
                epoch_transition: transition,
            });
        return Ok(());
    }
    if previous_projection.envelope != current.envelope {
        return Err(ProviderRequestAssemblyError::invalid_state(format!(
            "{} request chain changed an unclassified canonical envelope inside one epoch",
            continuity.provider_label,
        )));
    }
    if !current.input.starts_with(&previous_projection.input) {
        return Err(ProviderRequestAssemblyError::invalid_state(format!(
            "{} request chain rewrote canonical provider input inside one epoch",
            continuity.provider_label,
        )));
    }
    request
        .messages
        .set_provider_request_epoch(crate::context::ProviderRequestEpoch {
            context_epoch: current_epoch,
            epoch_transition: previous
                .messages
                .provider_request_epoch()
                .map_or(ContextEpochTransition::Initial, |epoch| {
                    epoch.epoch_transition
                }),
        });
    Ok(())
}

/// Splits one deterministic request body into its envelope and input items.
fn provider_native_request_projection(
    body: &str,
    input_field: &str,
    provider_label: &str,
) -> ProviderRequestAssemblyResult<ProviderNativeRequestProjection> {
    let mut body: Value = serde_json::from_str(body).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "{provider_label} request continuity could not decode canonical request body: {error}"
        ))
    })?;
    let object = body.as_object_mut().ok_or_else(|| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "{provider_label} canonical request body must be a JSON object"
        ))
    })?;
    let input = object.remove(input_field).ok_or_else(|| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "{provider_label} canonical request body has no `{input_field}` input"
        ))
    })?;
    let input = input.as_array().ok_or_else(|| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "{provider_label} canonical `{input_field}` input must be an array"
        ))
    })?;
    let input = input
        .iter()
        .map(canonical_json_bytes)
        .collect::<ProviderRequestAssemblyResult<Vec<_>>>()?;
    let envelope = canonical_json_bytes(&body)?;
    Ok(ProviderNativeRequestProjection {
        envelope,
        envelope_value: body,
        input,
    })
}

/// Builds the typed epoch identity for one native provider rendering.
fn provider_native_epoch_identity(
    request: &ModelRequest,
    cache_namespace: &str,
    api_shape: &str,
    envelope: &Value,
) -> ProviderRequestAssemblyResult<ContextEpochIdentity> {
    let stable_messages = request
        .messages
        .iter()
        .filter(|message| message.placement == ContextPlacement::StablePrefix)
        .map(|message| {
            serde_json::json!({
                "role": format!("{:?}", message.role),
                "source": format!("{:?}", message.source),
                "content": message.content,
            })
        })
        .collect::<Vec<_>>();
    let static_projection = serde_json::json!({
        "messages": stable_messages,
        "system": envelope.get("system").cloned().unwrap_or(Value::Null),
    });
    let response_format = envelope
        .get("response_format")
        .cloned()
        .unwrap_or(Value::Null);
    let tools = envelope.get("tools").cloned().unwrap_or(Value::Null);
    let tool_choice = envelope.get("tool_choice").cloned().unwrap_or(Value::Null);
    let mut controls = envelope.clone();
    let controls = controls.as_object_mut().ok_or_else(|| {
        ProviderRequestAssemblyError::invalid_state(
            "canonical provider request envelope must be a JSON object",
        )
    })?;
    for field in ["model", "system", "response_format", "tools", "tool_choice"] {
        controls.remove(field);
    }
    Ok(ContextEpochIdentity {
        provider_namespace: cache_namespace.to_string(),
        provider: request.provider.clone(),
        model: request.model.clone(),
        static_instructions_sha256: sha256_hex(&canonical_json_bytes(&static_projection)?),
        maap_schema_version: "maap/1".to_string(),
        response_format_sha256: sha256_hex(&canonical_json_bytes(&response_format)?),
        interaction_family: request.interaction_kind.as_str().to_string(),
        tool_schema_sha256: sha256_hex(&canonical_json_bytes(&tools)?),
        tool_choice_sha256: sha256_hex(&canonical_json_bytes(&tool_choice)?),
        request_controls_sha256: sha256_hex(&canonical_json_bytes(&Value::Object(
            controls.clone(),
        ))?),
        api_shape: api_shape.to_string(),
        cache_lineage: request.prompt_cache_lineage_id.clone(),
        compaction_generation_sha256: sha256_hex(
            compaction_generation_material(request).as_bytes(),
        ),
    })
}

/// Encodes JSON with recursively sorted object keys for exact byte comparison.
fn canonical_json_bytes(value: &Value) -> ProviderRequestAssemblyResult<Vec<u8>> {
    let mut bytes = Vec::new();
    write_canonical_json(value, &mut bytes)?;
    Ok(bytes)
}

/// Writes one JSON value using deterministic map ordering.
fn write_canonical_json(value: &Value, bytes: &mut Vec<u8>) -> ProviderRequestAssemblyResult<()> {
    match value {
        Value::Array(items) => {
            bytes.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    bytes.push(b',');
                }
                write_canonical_json(item, bytes)?;
            }
            bytes.push(b']');
        }
        Value::Object(object) => {
            bytes.push(b'{');
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    bytes.push(b',');
                }
                serde_json::to_writer(&mut *bytes, key).map_err(json_encoding_error)?;
                bytes.push(b':');
                write_canonical_json(&object[key], bytes)?;
            }
            bytes.push(b'}');
        }
        _ => serde_json::to_writer(&mut *bytes, value).map_err(json_encoding_error)?,
    }
    Ok(())
}

/// Converts canonical JSON serialization failures into request-assembly errors.
fn json_encoding_error(error: serde_json::Error) -> ProviderRequestAssemblyError {
    ProviderRequestAssemblyError::invalid_state(format!(
        "canonical provider request encoding failed: {error}"
    ))
}

/// Returns the durable compaction markers that identify one chronology epoch.
fn compaction_generation_material(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .filter(|message| {
            message.source == crate::ContextSourceKind::Memory
                && (message
                    .content
                    .starts_with("[context compaction summary]\n")
                    || message
                        .content
                        .starts_with("[conversation compaction notice]\n")
                    || message.content.starts_with("[memory compact-"))
        })
        .map(|message| {
            format!(
                "{}:{:?}:{}",
                message.content.len(),
                message.source,
                message.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Returns lowercase SHA-256 hexadecimal text without retaining source bytes.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
