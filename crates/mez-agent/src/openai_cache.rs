//! OpenAI request rendering and prompt-cache diagnostics.
//!
//! This module owns the OpenAI-specific conversion from canonical model
//! messages into Responses API `instructions` and `input` material. It also
//! computes non-model-visible prompt-cache fingerprints used for diagnostics.

use crate::context::{ContextEpochIdentity, ContextEpochTransition, ProviderRequestEpoch};
use crate::openai_request::{
    apply_openai_prompt_cache_breakpoint, openai_responses_request_control_shape_with_stream,
};
use crate::openai_schema::openai_maap_action_batch_tools;
use crate::provider::{
    MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME, OpenAiPromptCacheKeyPurpose,
    openai_prompt_cache_key_with_partition,
};
use crate::{
    ContextSourceKind, ModelInteractionKind, ModelRequest, OpenAiPromptCacheDiagnostics,
    OpenAiPromptCacheGeneration, OpenAiPromptCacheKeyDiagnostics, OpenAiRenderedMessages,
    ProviderRequestAssemblyError, ProviderRequestAssemblyResult,
    openai_auto_sizing_response_format, openai_macro_judge_response_format,
    openai_prompt_cache_diagnostics, openai_render_messages, openai_routed_handoff_response_format,
    openai_sandbox_failure_assessment_response_format, openai_stable_projection_material,
    validate_provider_request_required,
};
#[cfg(test)]
use crate::{ModelMessage, ModelMessageRole};
use sha2::{Digest, Sha256};

/// Renders request messages from canonical durable chronology.
pub(super) fn openai_render_request_messages(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<OpenAiRenderedMessages> {
    openai_render_request_messages_without_chain(request)
}

/// Renders the canonical durable request before any retained OpenAI wire-chain override.
fn openai_render_request_messages_without_chain(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<OpenAiRenderedMessages> {
    openai_render_messages(&request.messages)
}

/// Prepares one exact append-only OpenAI input chain before a concrete send.
///
/// Ordinary requests in the same provider/model/lineage epoch must preserve
/// all cache-affecting envelope bytes and retain the prior complete canonical
/// input as an exact leading prefix. The current canonical input is rendered
/// solely from durable context; exceptional modes and explicit scope changes
/// start a new chain epoch. Comparison failures warn and start a fresh baseline
/// without blocking a valid current request or changing its canonical content.
pub fn prepare_openai_request_prefix_extension(
    request: &mut ModelRequest,
    previous: Option<&ModelRequest>,
) -> ProviderRequestAssemblyResult<()> {
    let cache_namespace = request.provider.clone();
    prepare_openai_request_prefix_extension_with_context(request, previous, &cache_namespace, false)
}

/// Prepares one exact OpenAI chain using the concrete routing and stream epoch.
pub fn prepare_openai_request_prefix_extension_with_context(
    request: &mut ModelRequest,
    previous: Option<&ModelRequest>,
    cache_namespace: &str,
    stream: bool,
) -> ProviderRequestAssemblyResult<()> {
    let canonical = openai_render_request_messages_without_chain(request)?;
    let current_epoch =
        openai_context_epoch_identity(request, &canonical, cache_namespace, stream)?;
    let Some(previous) = previous else {
        request
            .messages
            .set_provider_request_epoch(provider_request_epoch(
                current_epoch,
                ContextEpochTransition::Initial,
            ));
        return Ok(());
    };

    let prior = (|| {
        let canonical = openai_render_request_messages_without_chain(previous)?;
        let epoch = match previous.messages.provider_request_epoch() {
            Some(chain) => chain.context_epoch.clone(),
            None => openai_context_epoch_identity(previous, &canonical, cache_namespace, stream)?,
        };
        Ok::<_, ProviderRequestAssemblyError>((canonical, epoch))
    })();
    let (previous_canonical, previous_epoch) = match prior {
        Ok(prior) => prior,
        Err(_) => {
            request
                .messages
                .set_provider_request_epoch(provider_request_epoch(
                    current_epoch,
                    ContextEpochTransition::Warning("prior_baseline_unavailable"),
                ));
            return Ok(());
        }
    };
    if previous_epoch != current_epoch {
        let transition = ContextEpochTransition::Changed(
            previous_epoch
                .changed_component(&current_epoch)
                .expect("different context epochs must identify a changed component"),
        );
        request
            .messages
            .set_provider_request_epoch(provider_request_epoch(current_epoch, transition));
        return Ok(());
    }
    if !canonical.input.starts_with(&previous_canonical.input) {
        request
            .messages
            .set_provider_request_epoch(provider_request_epoch(
                current_epoch,
                ContextEpochTransition::Warning("canonical_input_rewritten"),
            ));
        return Ok(());
    }
    request
        .messages
        .set_provider_request_epoch(provider_request_epoch(
            current_epoch,
            previous.messages.provider_request_epoch().map_or(
                ContextEpochTransition::Initial,
                |chain| match chain.epoch_transition {
                    ContextEpochTransition::Warning(_) => ContextEpochTransition::Initial,
                    transition => transition,
                },
            ),
        ));
    Ok(())
}

/// Retains only the epoch classification for a prepared OpenAI request.
fn provider_request_epoch(
    context_epoch: ContextEpochIdentity,
    epoch_transition: ContextEpochTransition,
) -> ProviderRequestEpoch {
    ProviderRequestEpoch {
        context_epoch,
        epoch_transition,
    }
}

/// Operational controls excluded from prompt-cache identity material.
///
/// `reasoning`, `service_tier`, `text.verbosity`, and their provider-native
/// dialect spellings change provider behaviour at request time but not one byte
/// of the model-visible prefix or the provider-side cache key, so a change to
/// only these controls must neither rotate Mezzanine's local epoch nor report a
/// continuity divergence. They stay wire-level parameters - the emitted body
/// keeps them - and provider-side truth stays observable through usage counters.
/// New operational controls belong in this list instead of silently becoming
/// identity material.
const CACHE_IDENTITY_EXCLUDED_CONTROL_PATHS: &[&[&str]] = &[
    // The OpenAI Responses reasoning object: every member today is operational.
    &["reasoning"],
    // Provider-native reasoning spellings, including DeepSeek's thinking object.
    &["thinking"],
    &["output_config", "effort"],
    &["output_config", "verbosity"],
    &["reasoning_effort"],
    &["service_tier"],
    &["text", "verbosity"],
    &["verbosity"],
    // Sampling and output caps: they change generation, not one byte of cached
    // prefix material, and the OpenAI Responses body never emits them.
    &["temperature"],
    &["stop"],
    // Anthropic spells the stop control `stop_sequences`.
    &["stop_sequences"],
    &["max_tokens"],
];

/// Returns one request-control shape reduced to the fields that define
/// prompt-cache identity.
pub(crate) fn openai_cache_identity_control_projection(
    controls: &serde_json::Value,
) -> serde_json::Value {
    let mut projection = controls.clone();
    for path in CACHE_IDENTITY_EXCLUDED_CONTROL_PATHS {
        remove_cache_identity_control_path(&mut projection, path);
    }
    projection
}

/// Removes one excluded control path, pruning objects that become empty so an
/// excluded child cannot leave an empty parent behind in the identity material.
fn remove_cache_identity_control_path(value: &mut serde_json::Value, path: &[&str]) {
    let Some((head, rest)) = path.split_first() else {
        return;
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if rest.is_empty() {
        object.remove(*head);
        return;
    }
    let Some(child) = object.get_mut(*head) else {
        return;
    };
    remove_cache_identity_control_path(child, rest);
    if child.as_object().is_some_and(|child| child.is_empty()) {
        object.remove(*head);
    }
}

/// Builds the exact non-model-visible identity for one OpenAI Responses epoch.
///
/// Every field is derived from canonical provider-bound state. A difference
/// starts a fresh epoch before dispatch rather than permitting a same-epoch
/// rewrite of provider input.
fn openai_context_epoch_identity(
    request: &ModelRequest,
    rendered: &OpenAiRenderedMessages,
    cache_namespace: &str,
    stream: bool,
) -> ProviderRequestAssemblyResult<ContextEpochIdentity> {
    let response_format = openai_response_format(request).unwrap_or(serde_json::Value::Null);
    let (tools, tool_choice) = if request.interaction_kind.expects_structured_json() {
        (serde_json::json!([]), serde_json::json!("none"))
    } else if request.interaction_kind.expects_maap_batch() {
        (
            serde_json::json!(openai_maap_action_batch_tools(request)),
            serde_json::json!({
                "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                "type": "function"
            }),
        )
    } else {
        (serde_json::json!([]), serde_json::Value::Null)
    };
    let request_controls = openai_responses_request_control_shape_with_stream(request, stream)?;
    Ok(ContextEpochIdentity {
        provider_namespace: cache_namespace.to_string(),
        provider: request.provider.clone(),
        model: request.model.clone(),
        static_instructions_sha256: sha256_hex(rendered.instructions.as_bytes()),
        maap_schema_version: "maap/1".to_string(),
        response_format_sha256: canonical_json_sha256(&response_format)?,
        tool_schema_sha256: canonical_json_sha256(&tools)?,
        tool_choice_sha256: canonical_json_sha256(&tool_choice)?,
        request_controls_sha256: canonical_json_sha256(&openai_cache_identity_control_projection(
            &request_controls,
        ))?,
        api_shape: format!("openai-responses;stream={stream}"),
        cache_lineage: request.prompt_cache_lineage_id.clone(),
        compaction_generation_sha256: sha256_hex(
            openai_compaction_generation_material(request).as_bytes(),
        ),
    })
}

/// Returns the exact durable compaction markers that identify one chronology epoch.
fn openai_compaction_generation_material(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .filter(|message| {
            message.source == ContextSourceKind::Memory
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
                "{}:{}:{}",
                message.content.len(),
                message.source as u8,
                message.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Returns a deterministic SHA-256 digest for canonical JSON material.
fn canonical_json_sha256(value: &serde_json::Value) -> ProviderRequestAssemblyResult<String> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI context epoch JSON encoding failed: {error}"
        ))
    })?;
    Ok(sha256_hex(&bytes))
}

/// Returns lowercase SHA-256 text without retaining the hashed material.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Returns the OpenAI response-format field for special request modes.
pub(super) fn openai_response_format(request: &ModelRequest) -> Option<serde_json::Value> {
    match request.interaction_kind {
        ModelInteractionKind::AutoSizing => Some(openai_auto_sizing_response_format()),
        ModelInteractionKind::MacroJudge => Some(openai_macro_judge_response_format()),
        ModelInteractionKind::SandboxFailureAssessment => {
            Some(openai_sandbox_failure_assessment_response_format())
        }
        ModelInteractionKind::RoutedHandoff | ModelInteractionKind::RoutedHandoffRepair => {
            Some(openai_routed_handoff_response_format())
        }
        _ => None,
    }
}

/// Resolves the content-free routing purpose and partition for one request.
///
/// Pre-GPT-5.6 internal work uses a bounded deterministic shard so genuinely
/// shared fixed prefixes can retain routing affinity without one global hot
/// key. GPT-5.6+ uses the stable agent boundary instead because its key is an
/// accounting and anti-probing boundary rather than a routing optimization.
fn openai_prompt_cache_key_partition(
    request: &ModelRequest,
) -> (OpenAiPromptCacheKeyPurpose, String) {
    let generation = request
        .model_capabilities
        .openai_prompt_cache_generation
        .or_else(|| crate::openai_request::inferred_openai_prompt_cache_generation(&request.model));
    let is_gpt56_or_newer = generation == Some(OpenAiPromptCacheGeneration::Gpt56OrNewer);
    match request.interaction_kind {
        ModelInteractionKind::AutoSizing => {
            if is_gpt56_or_newer {
                (
                    OpenAiPromptCacheKeyPurpose::InternalRouter,
                    request.agent_id.clone(),
                )
            } else {
                (
                    OpenAiPromptCacheKeyPurpose::InternalRouter,
                    format!(
                        "routing-shard-{}",
                        stable_partition_shard(&request.agent_id)
                    ),
                )
            }
        }
        ModelInteractionKind::MacroJudge
        | ModelInteractionKind::SandboxFailureAssessment
        | ModelInteractionKind::RoutedHandoff
        | ModelInteractionKind::RoutedHandoffRepair => (
            OpenAiPromptCacheKeyPurpose::InternalWorkflow,
            request.agent_id.clone(),
        ),
        _ if request.prompt_cache_session_id.is_some() => (
            OpenAiPromptCacheKeyPurpose::Session,
            "session-boundary".to_string(),
        ),
        _ => (
            OpenAiPromptCacheKeyPurpose::UnknownCompatible,
            request.agent_id.clone(),
        ),
    }
}

/// Returns one of four stable routing shards from a non-content agent identity.
fn stable_partition_shard(agent_id: &str) -> u8 {
    let digest = Sha256::digest(agent_id.as_bytes());
    digest[0] % 4
}

/// Builds a stable, non-secret OpenAI prompt-cache routing key for a request.
pub(super) fn openai_prompt_cache_key(request: &ModelRequest) -> String {
    let (purpose, partition) = openai_prompt_cache_key_partition(request);
    openai_prompt_cache_key_with_partition(
        &request.provider,
        request.prompt_cache_lineage_id.as_deref(),
        request.prompt_cache_session_id.as_deref(),
        purpose,
        &partition,
    )
}

/// Returns non-model-visible OpenAI prompt-cache diagnostics for one request.
pub fn openai_prompt_cache_diagnostics_for_request(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<OpenAiPromptCacheDiagnostics> {
    openai_prompt_cache_diagnostics_for_request_with_stream(request, false)
}

/// Returns non-model-visible OpenAI prompt-cache diagnostics for one request and stream mode.
pub fn openai_prompt_cache_diagnostics_for_request_with_stream(
    request: &ModelRequest,
    stream: bool,
) -> ProviderRequestAssemblyResult<OpenAiPromptCacheDiagnostics> {
    validate_provider_request_required("OpenAI model", &request.model)?;
    let mut rendered = openai_render_request_messages(request)?;
    apply_openai_prompt_cache_breakpoint(request, &mut rendered.input)?;
    apply_openai_prompt_cache_breakpoint(request, &mut rendered.stable_input)?;
    let response_format = openai_response_format(request).unwrap_or(serde_json::Value::Null);
    let tools = if request.interaction_kind.expects_structured_json() {
        serde_json::json!([])
    } else if request.interaction_kind.expects_maap_batch() {
        serde_json::json!(openai_maap_action_batch_tools(request))
    } else {
        serde_json::json!([])
    };
    let tool_choice = if request.interaction_kind.expects_structured_json() {
        serde_json::json!("none")
    } else if request.interaction_kind.expects_maap_batch() {
        serde_json::json!({
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
            "type": "function"
        })
    } else {
        serde_json::Value::Null
    };
    let provider_request_shape =
        openai_responses_request_control_shape_with_stream(request, stream)?;
    let (prompt_cache_key_purpose, prompt_cache_partition) =
        openai_prompt_cache_key_partition(request);
    let prompt_cache_key = openai_prompt_cache_key_with_partition(
        &request.provider,
        request.prompt_cache_lineage_id.as_deref(),
        request.prompt_cache_session_id.as_deref(),
        prompt_cache_key_purpose,
        &prompt_cache_partition,
    );
    let mut complete_request = provider_request_shape.clone();
    complete_request["instructions"] = serde_json::json!(rendered.instructions);
    complete_request["input"] = serde_json::json!(rendered.input);
    complete_request["prompt_cache_key"] = serde_json::json!(prompt_cache_key);
    openai_prompt_cache_diagnostics(
        OpenAiPromptCacheKeyDiagnostics {
            key: prompt_cache_key,
            purpose: prompt_cache_key_purpose.as_str().to_string(),
            partition_sha256: sha256_hex(prompt_cache_partition.as_bytes()),
        },
        &rendered,
        &response_format,
        &tools,
        &tool_choice,
        &provider_request_shape,
        &complete_request,
    )
}

/// Returns the local instructions-and-stable-input projection for a request.
pub fn openai_stable_projection_material_for_request(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<String> {
    let rendered = openai_render_request_messages(request)?;
    openai_stable_projection_material(&rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowedActionSet, ProviderTranscriptEvent};

    /// Builds one ordinary OpenAI request for exact request-chain tests.
    fn request_chain_fixture(messages: Vec<ModelMessage>) -> ModelRequest {
        ModelRequest {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            model_capabilities: Default::default(),
            max_input_tokens: None,
            reasoning_effort: Some("medium".to_string()),
            thinking_enabled: None,
            latency_preference: Some("default".to_string()),
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            prompt_cache_session_id: Some("session-1".to_string()),
            prompt_cache_lineage_id: Some("lineage-1".to_string()),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: ModelInteractionKind::ActionExecution,
            allowed_actions: AllowedActionSet::action_execution_base(),
            stop: None,
            messages: messages.into(),
        }
    }

    /// Verifies a rebuilt request retains every prior OpenAI input item before
    /// appending newly settled chronology.
    ///
    /// The provider caches the complete rendered input rather than Mezzanine's
    /// logical placement classes. This regression therefore compares actual
    /// request JSON and fails if a request-local message is relocated.
    #[test]
    fn openai_request_chain_preserves_complete_wire_prefix() {
        let system = ModelMessage {
            role: ModelMessageRole::System,
            source: ContextSourceKind::System,
            placement: crate::ContextPlacement::StablePrefix,
            content: "stable instructions".to_string(),
        };
        let user = ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "inspect the cache".to_string(),
        };
        let tail = ModelMessage {
            role: ModelMessageRole::Context,
            source: ContextSourceKind::RuntimeHint,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "[mcp integrations]\navailable_tool=files/read".to_string(),
        };
        let mut first = request_chain_fixture(vec![system.clone(), user.clone(), tail.clone()]);
        crate::append_request_state_transition(&mut first);
        prepare_openai_request_prefix_extension(&mut first, None).unwrap();
        let first_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&first).unwrap()).unwrap();

        let assistant = ModelMessage {
            role: ModelMessageRole::Assistant,
            source: ContextSourceKind::TranscriptAssistant,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "read the file".to_string(),
        };
        let mut second = first.clone();
        second.messages.push(assistant);
        prepare_openai_request_prefix_extension(&mut second, Some(&first)).unwrap();
        let second_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&second).unwrap()).unwrap();
        let first_input = first_body["input"].as_array().unwrap();
        let second_input = second_body["input"].as_array().unwrap();

        assert_eq!(first_input, &second_input[..first_input.len()]);
        assert_eq!(second_input.len(), first_input.len() + 1);
        let first_diagnostics = openai_prompt_cache_diagnostics_for_request(&first).unwrap();
        let second_diagnostics = openai_prompt_cache_diagnostics_for_request(&second).unwrap();
        let continuity = crate::compare_openai_request_continuity(
            &first_diagnostics.continuity_snapshot,
            &second_diagnostics.continuity_snapshot,
        );
        assert!(continuity.messages_append_only, "{continuity:#?}");
        assert_eq!(continuity.common_message_prefix, first_input.len());
    }

    /// Verifies an ordinary request chain appends new chronology while
    /// warning on a rewrite of an already-sent chronological item without
    /// blocking the request or poisoning subsequent comparisons.
    #[test]
    fn openai_request_chain_appends_chronology_and_warns_on_rewrites() {
        let messages = vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "stable instructions".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "inspect the cache".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Context,
                source: ContextSourceKind::RuntimeHint,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "state=first".to_string(),
            },
        ];
        let mut first = request_chain_fixture(messages.clone());
        crate::append_request_state_transition(&mut first);
        prepare_openai_request_prefix_extension(&mut first, None).unwrap();

        let mut appended = first.clone();
        appended.messages.push(ModelMessage {
            role: ModelMessageRole::Context,
            source: ContextSourceKind::RuntimeHint,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "state=second".to_string(),
        });
        prepare_openai_request_prefix_extension(&mut appended, Some(&first)).unwrap();
        let first_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&first).unwrap()).unwrap();
        let appended_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&appended).unwrap())
                .unwrap();
        let first_input = first_body["input"].as_array().unwrap();
        let appended_input = appended_body["input"].as_array().unwrap();
        assert_eq!(first_input, &appended_input[..first_input.len()]);
        assert_eq!(appended_input.len(), first_input.len() + 1);

        let mut rewritten = first.clone();
        rewritten.messages = vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "stable instructions".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "inspect the cache".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Context,
                source: ContextSourceKind::RuntimeHint,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "state=rewritten".to_string(),
            },
        ]
        .into();
        prepare_openai_request_prefix_extension(&mut rewritten, Some(&first)).unwrap();
        assert_eq!(
            rewritten.messages.provider_continuity_warning(),
            Some("canonical_input_rewritten")
        );
        let mut next = rewritten.clone();
        prepare_openai_request_prefix_extension(&mut next, Some(&rewritten)).unwrap();
        assert_eq!(next.messages.provider_continuity_warning(), None);
    }

    /// Verifies missing or malformed native comparison baselines cannot reject
    /// a valid current body, while malformed current bodies remain errors.
    /// Warning metadata must not change model-visible request content.
    #[test]
    fn native_cache_baseline_failures_are_advisory() {
        use crate::provider_continuity::{
            ProviderNativeRequestContinuity, prepare_provider_native_request_prefix_extension,
        };
        let previous = request_chain_fixture(Vec::new());
        for baseline in [
            None,
            Some("not json"),
            Some("[]"),
            Some("{}"),
            Some(r#"{"messages":false}"#),
        ] {
            let mut current = previous.clone();
            prepare_provider_native_request_prefix_extension(
                &mut current,
                Some(&previous),
                ProviderNativeRequestContinuity {
                    cache_namespace: "test",
                    provider_label: "test",
                    current_api_shape: "test",
                    previous_api_shape: "test",
                    input_field: "messages",
                    current_body: r#"{"messages":[]}"#,
                    previous_body: baseline,
                },
            )
            .unwrap();
            assert_eq!(
                current.messages.provider_continuity_warning(),
                Some("prior_baseline_unavailable")
            );
            assert_eq!(current.messages.iter().count(), 0);
        }
        let mut current = previous.clone();
        assert!(
            prepare_provider_native_request_prefix_extension(
                &mut current,
                Some(&previous),
                ProviderNativeRequestContinuity {
                    cache_namespace: "test",
                    provider_label: "test",
                    current_api_shape: "test",
                    previous_api_shape: "test",
                    input_field: "messages",
                    current_body: "not json",
                    previous_body: None,
                },
            )
            .is_err()
        );
    }

    /// Verifies the cache-identity projection drops exactly the operational
    /// controls and nothing else.
    ///
    /// A new operational control must be added to the projection list instead of
    /// silently becoming identity material, so the exclusion set is asserted
    /// directly for both the OpenAI shape and the provider-native spellings, and
    /// an emptied parent object is expected to disappear rather than leave a
    /// residual empty node in the hashed material.
    #[test]
    fn openai_cache_identity_control_projection_excludes_operational_controls() {
        let controls = serde_json::json!({
            "model": "gpt-test",
            "stream": true,
            "reasoning": { "effort": "high" },
            "thinking": { "type": "enabled" },
            "service_tier": "priority",
            "text": { "format": { "type": "json_object" }, "verbosity": "low" },
            "output_config": { "effort": "high" },
            "reasoning_effort": "high",
            "verbosity": "low",
            "temperature": 0.9,
            "stop": ["END"],
            "stop_sequences": ["END"],
            "max_tokens": 4096,
        });
        let projection = openai_cache_identity_control_projection(&controls);
        assert_eq!(
            projection,
            serde_json::json!({
                "model": "gpt-test",
                "stream": true,
                "text": { "format": { "type": "json_object" } },
            })
        );

        // An excluded leaf must not leave an empty parent behind.
        let only_verbosity = serde_json::json!({ "text": { "verbosity": "low" } });
        assert_eq!(
            openai_cache_identity_control_projection(&only_verbosity),
            serde_json::json!({})
        );
    }

    /// Verifies an operational control change keeps the cache epoch, the
    /// rendered prefix, and the derived cache key.
    ///
    /// Auto-sizing can select a different reasoning effort or latency preference
    /// between turns. Those are wire parameters: the emitted body still carries
    /// them, but they must not rotate Mezzanine's local epoch nor appear as a
    /// request-control continuity divergence.
    #[test]
    fn openai_operational_controls_keep_the_cache_epoch_and_envelope() {
        let messages = vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "stable instructions".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "inspect the cache".to_string(),
            },
        ];
        let mut medium = request_chain_fixture(messages.clone());
        prepare_openai_request_prefix_extension(&mut medium, None).unwrap();

        let mut high = request_chain_fixture(messages);
        high.reasoning_effort = Some("high".to_string());
        high.latency_preference = Some("fast".to_string());
        prepare_openai_request_prefix_extension(&mut high, Some(&medium)).unwrap();

        let medium_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&medium).unwrap()).unwrap();
        let high_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&high).unwrap()).unwrap();
        assert_eq!(high_body["reasoning"]["effort"], "high");
        assert_eq!(high_body["service_tier"], "priority");
        assert_eq!(medium_body["instructions"], high_body["instructions"]);
        assert_eq!(medium_body["input"], high_body["input"]);
        assert_eq!(
            medium_body["prompt_cache_key"], high_body["prompt_cache_key"],
            "operational controls must not change the derived cache key"
        );
        assert_eq!(high.messages.provider_continuity_warning(), None);
        assert_eq!(
            medium
                .messages
                .provider_request_epoch()
                .unwrap()
                .context_epoch,
            high.messages
                .provider_request_epoch()
                .unwrap()
                .context_epoch,
            "operational controls must not rotate the local cache epoch"
        );

        let medium_diagnostics = openai_prompt_cache_diagnostics_for_request(&medium).unwrap();
        let high_diagnostics = openai_prompt_cache_diagnostics_for_request(&high).unwrap();
        let continuity = crate::compare_openai_request_continuity(
            &medium_diagnostics.continuity_snapshot,
            &high_diagnostics.continuity_snapshot,
        );
        assert!(continuity.cache_envelope_unchanged, "{continuity:#?}");
        assert!(continuity.request_prefix_append_only, "{continuity:#?}");
        assert_eq!(continuity.category, "identical", "{continuity:#?}");
        assert_ne!(
            medium_diagnostics.continuity_snapshot.request_sha256,
            high_diagnostics.continuity_snapshot.request_sha256,
            "the raw complete-request digest must still describe the literal body"
        );
        assert_eq!(
            medium_diagnostics
                .continuity_snapshot
                .request_control_sha256,
            high_diagnostics.continuity_snapshot.request_control_sha256,
            "only the cache-identity control digest is reduced"
        );
    }

    /// Verifies a native provider effort change does not warn about a changed
    /// canonical envelope or rotate the epoch.
    ///
    /// Anthropic spells the control `output_config.effort` and DeepSeek spells it
    /// `reasoning_effort`; both are operational, so the same-epoch drift check
    /// must ignore them while still catching genuinely cache-affecting changes.
    #[test]
    fn native_operational_effort_change_keeps_the_cache_epoch() {
        use crate::provider_continuity::{
            ProviderNativeRequestContinuity, prepare_provider_native_request_prefix_extension,
        };
        for (low_body, high_body) in [
            (
                r#"{"model":"m","messages":[],"output_config":{"effort":"low"}}"#,
                r#"{"model":"m","messages":[],"output_config":{"effort":"high"}}"#,
            ),
            (
                r#"{"model":"m","messages":[],"reasoning_effort":"low"}"#,
                r#"{"model":"m","messages":[],"reasoning_effort":"high"}"#,
            ),
            (
                r#"{"model":"m","messages":[],"thinking":{"type":"disabled"}}"#,
                r#"{"model":"m","messages":[],"thinking":{"type":"enabled"}}"#,
            ),
            (
                r#"{"model":"m","messages":[],"max_tokens":1024}"#,
                r#"{"model":"m","messages":[],"max_tokens":4096}"#,
            ),
            (
                r#"{"model":"m","messages":[],"temperature":0.2}"#,
                r#"{"model":"m","messages":[],"temperature":0.9}"#,
            ),
            (
                r#"{"model":"m","messages":[],"stop_sequences":["END"]}"#,
                r#"{"model":"m","messages":[],"stop_sequences":["STOP"]}"#,
            ),
        ] {
            let mut previous = request_chain_fixture(Vec::new());
            prepare_provider_native_request_prefix_extension(
                &mut previous,
                None,
                ProviderNativeRequestContinuity {
                    cache_namespace: "test",
                    provider_label: "test",
                    current_api_shape: "test",
                    previous_api_shape: "test",
                    input_field: "messages",
                    current_body: low_body,
                    previous_body: None,
                },
            )
            .unwrap();
            let mut current = previous.clone();
            prepare_provider_native_request_prefix_extension(
                &mut current,
                Some(&previous),
                ProviderNativeRequestContinuity {
                    cache_namespace: "test",
                    provider_label: "test",
                    current_api_shape: "test",
                    previous_api_shape: "test",
                    input_field: "messages",
                    current_body: high_body,
                    previous_body: Some(low_body),
                },
            )
            .unwrap();
            assert_eq!(
                current.messages.provider_continuity_warning(),
                None,
                "an effort-only change must not warn: {low_body} -> {high_body}"
            );
            assert_eq!(
                previous
                    .messages
                    .provider_request_epoch()
                    .unwrap()
                    .context_epoch,
                current
                    .messages
                    .provider_request_epoch()
                    .unwrap()
                    .context_epoch,
                "an effort-only change must not rotate the epoch"
            );
        }
    }

    /// Verifies invalid controls on a prior Responses baseline are advisory,
    /// without weakening current request validation or changing wire content.
    #[test]
    fn openai_unrenderable_baseline_does_not_block_valid_request() {
        let mut current = request_chain_fixture(vec![ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "continue normally".to_string(),
        }]);
        let wire = crate::openai_responses_request_body(&current).unwrap();
        let mut previous = current.clone();
        previous.model.clear();
        prepare_openai_request_prefix_extension(&mut current, Some(&previous)).unwrap();
        assert_eq!(
            current.messages.provider_continuity_warning(),
            Some("prior_baseline_unavailable")
        );
        assert_eq!(
            crate::openai_responses_request_body(&current).unwrap(),
            wire
        );
        assert!(prepare_openai_request_prefix_extension(&mut previous, Some(&current)).is_err());
    }

    /// Verifies a cache-affecting instruction change establishes a classified
    /// epoch instead of bypassing same-epoch canonical-input prefix checks.
    #[test]
    fn openai_instruction_change_starts_a_classified_context_epoch() {
        let user = ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "inspect the cache".to_string(),
        };
        let mut first = request_chain_fixture(vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "first epoch instructions".to_string(),
            },
            user.clone(),
        ]);
        crate::append_request_state_transition(&mut first);
        prepare_openai_request_prefix_extension(&mut first, None).unwrap();

        let mut changed = request_chain_fixture(vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "second epoch instructions".to_string(),
            },
            user,
        ]);
        crate::append_request_state_transition(&mut changed);
        prepare_openai_request_prefix_extension(&mut changed, Some(&first)).unwrap();

        assert!(matches!(
            changed
                .messages
                .provider_request_epoch()
                .unwrap()
                .epoch_transition,
            ContextEpochTransition::Changed(crate::ContextEpochComponent::StaticInstructions)
        ));
        let body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&changed).unwrap()).unwrap();
        assert_eq!(body["instructions"], "second epoch instructions");
    }

    /// Verifies a controller-only interaction-kind change keeps an OpenAI
    /// request in the same epoch when every provider-visible field is unchanged.
    ///
    /// Capability continuation and ordinary action execution use the same MAAP
    /// response shape in this fixture. The epoch must therefore retain the
    /// original canonical input prefix instead of treating the internal mode
    /// label as a provider cache boundary.
    #[test]
    fn openai_mode_only_transition_preserves_context_epoch_and_input_prefix() {
        let mut first = request_chain_fixture(vec![ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "continue cache reuse".to_string(),
        }]);
        prepare_openai_request_prefix_extension(&mut first, None).unwrap();
        let first_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&first).unwrap()).unwrap();

        let mut continued = first.clone();
        continued.interaction_kind = ModelInteractionKind::CapabilityContinuation;
        continued.messages.push(ModelMessage {
            role: ModelMessageRole::Assistant,
            source: ContextSourceKind::TranscriptAssistant,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "continuing work".to_string(),
        });
        prepare_openai_request_prefix_extension(&mut continued, Some(&first)).unwrap();
        let continued_body: serde_json::Value =
            serde_json::from_str(&crate::openai_responses_request_body(&continued).unwrap())
                .unwrap();

        assert!(matches!(
            continued
                .messages
                .provider_request_epoch()
                .unwrap()
                .epoch_transition,
            ContextEpochTransition::Initial
        ));
        assert_eq!(
            first_body["input"].as_array().unwrap(),
            &continued_body["input"].as_array().unwrap()
                [..first_body["input"].as_array().unwrap().len()]
        );
    }

    /// Verifies a mode transition that changes rendered provider instructions
    /// still starts a new epoch under the concrete instruction fingerprint.
    ///
    /// Controller mode labels themselves are not cache inputs, but the MAAP
    /// repair transition adds a stable provider-visible instruction. That
    /// instruction must remain an epoch boundary even after interaction-family
    /// metadata is removed from the cache identity.
    #[test]
    fn openai_mode_transition_with_changed_instructions_starts_instruction_epoch() {
        let mut first = request_chain_fixture(vec![ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "repair cache continuity".to_string(),
        }]);
        prepare_openai_request_prefix_extension(&mut first, None).unwrap();

        let mut repair = first.clone();
        crate::select_model_interaction_kind(&mut repair, ModelInteractionKind::MaapRepair);
        prepare_openai_request_prefix_extension(&mut repair, Some(&first)).unwrap();

        assert!(matches!(
            repair
                .messages
                .provider_request_epoch()
                .unwrap()
                .epoch_transition,
            ContextEpochTransition::Changed(crate::ContextEpochComponent::StaticInstructions)
        ));
    }

    /// Verifies OpenAI request rendering ignores hidden provider-native
    /// transcript events.
    ///
    /// DeepSeek can persist hidden replay metadata into shared transcript
    /// history. If a later request is routed through OpenAI, that metadata must
    /// not become an instruction or input message because OpenAI does not
    /// understand DeepSeek `reasoning_content` or Chat Completions tool-call
    /// replay fields.
    #[test]
    fn openai_rendering_omits_hidden_provider_transcript_events() {
        let event = ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content: "".to_string(),
            reasoning_content: Some("DeepSeek-only reasoning".to_string()),
            tool_calls: vec![serde_json::json!({
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "submit_maap_action_batch",
                    "arguments": "{}"
                }
            })],
        };
        let openai_output = ProviderTranscriptEvent::OpenAiResponseOutput {
            items: vec![
                serde_json::json!({
                    "type": "reasoning",
                    "id": "rs_openai",
                    "encrypted_content": "opaque-openai-ciphertext"
                }),
                serde_json::json!({
                    "type": "function_call",
                    "id": "fc_openai",
                    "call_id": "call_openai",
                    "name": "submit_maap_action_batch",
                    "arguments": "{}"
                }),
            ],
        };
        let openai_result = ProviderTranscriptEvent::OpenAiFunctionCallOutput {
            call_id: "call_openai".to_string(),
            output: "[action_result action-1 shell_command succeeded]\nexit_code: 0\noutput:\nopenai-live-output-sentinel"
                .to_string(),
        };
        let request = ModelRequest {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            model_capabilities: Default::default(),
            max_input_tokens: None,
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            stop: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: ModelInteractionKind::CapabilityDecision,
            allowed_actions: AllowedActionSet::capability_decision(),
            messages: vec![
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::System,
                    placement: crate::ContextPlacement::StablePrefix,
                    content: "system prompt".to_string(),
                },
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::Transcript,
                    placement: crate::ContextPlacement::ConversationAppend,
                    content: event.to_transcript_content(),
                },
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::Transcript,
                    placement: crate::ContextPlacement::ConversationAppend,
                    content: openai_output.to_transcript_content(),
                },
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::Transcript,
                    placement: crate::ContextPlacement::ConversationAppend,
                    content: openai_result.to_transcript_content(),
                },
                ModelMessage {
                    role: ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    placement: crate::ContextPlacement::ConversationAppend,
                    content: "continue".to_string(),
                },
            ]
            .into(),
        };

        let rendered = openai_render_request_messages(&request).unwrap();
        let rendered_json = serde_json::to_string(&rendered.input).unwrap();

        assert_eq!(rendered.input.len(), 4);
        assert!(rendered.instructions.contains("system prompt"));
        assert!(rendered_json.contains("continue"));
        assert!(!rendered_json.contains("[OpenAI request state]"));
        assert!(rendered_json.contains("opaque-openai-ciphertext"));
        assert!(rendered_json.contains("\"call_id\":\"call_openai\""));
        assert!(rendered_json.contains("function_call_output"));
        assert!(rendered_json.contains("openai-live-output-sentinel"));
        assert!(!rendered_json.contains("historical_output: omitted"));
        assert!(!rendered.instructions.contains("DeepSeek-only reasoning"));
        assert!(!rendered_json.contains("reasoning_content"));
        assert!(!rendered_json.contains("call_1"));
    }

    /// Verifies a reduced legacy Responses function-call result keeps its
    /// envelope in the assembled request without re-exposing the legacy body.
    #[test]
    fn openai_rendering_replays_paired_reduced_legacy_function_call_outputs() {
        let legacy_call =
            ProviderTranscriptEvent::validated_openai_response_output(vec![serde_json::json!({
                "type": "function_call",
                "id": "fc_legacy",
                "call_id": "call_legacy",
                "name": "submit_maap_action_batch",
                "arguments": "{}"
            })])
            .unwrap();
        let legacy_result = ProviderTranscriptEvent::OpenAiFunctionCallOutput {
            call_id: "call_legacy".to_string(),
            output: "[action_result a1 shell_command succeeded]\nexit_code: 0\noutput:\nopenai-legacy-secret-sentinel"
                .to_string(),
        };
        let reduced = legacy_result.sanitized_for_historical_replay().unwrap();
        let request = request_chain_fixture(vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                content: legacy_call.to_transcript_content(),
            },
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                content: reduced.to_transcript_content(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "continue".to_string(),
            },
        ]);

        let rendered = openai_render_request_messages(&request).unwrap();
        let rendered_json = serde_json::to_string(&rendered.input).unwrap();

        assert!(
            rendered_json.contains("function_call_output"),
            "{rendered_json}"
        );
        assert!(rendered_json.contains("call_legacy"), "{rendered_json}");
        assert!(
            rendered_json.contains("historical_output: omitted"),
            "{rendered_json}"
        );
        assert!(!rendered_json.contains("openai-legacy-secret-sentinel"));
    }

    /// Verifies a legacy Responses function-call result whose assistant call
    /// was lost is omitted instead of replayed as an unpaired result.
    #[test]
    fn openai_rendering_omits_unpaired_legacy_function_call_outputs() {
        let legacy_result = ProviderTranscriptEvent::OpenAiFunctionCallOutput {
            call_id: "call_legacy".to_string(),
            output: "[action_result a1 shell_command succeeded]\nexit_code: 0\noutput:\nopenai-legacy-secret-sentinel"
                .to_string(),
        };
        let reduced = legacy_result.sanitized_for_historical_replay().unwrap();
        let request = request_chain_fixture(vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                content: reduced.to_transcript_content(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "continue".to_string(),
            },
        ]);

        let rendered = openai_render_request_messages(&request).unwrap();
        let rendered_json = serde_json::to_string(&rendered.input).unwrap();

        assert!(
            !rendered_json.contains("function_call_output"),
            "{rendered_json}"
        );
        assert!(!rendered_json.contains("call_legacy"), "{rendered_json}");
        assert!(!rendered_json.contains("openai-legacy-secret-sentinel"));
    }

    /// Verifies auxiliary OpenAI requests use deterministic typed partitions
    /// instead of sharing the legacy unknown-session routing key.
    ///
    /// Pre-GPT-5.6 router traffic is intentionally bounded to four stable
    /// shards for prefix-routing affinity. GPT-5.6 keys are accounting and
    /// anti-probing boundaries, so they retain the individual agent boundary.
    #[test]
    fn openai_cache_key_partitions_auxiliary_workload_by_generation() {
        let mut pre_gpt56 = request_chain_fixture(vec![ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            content: "size this task".to_string(),
        }]);
        pre_gpt56.model = "gpt-5.5".to_string();
        pre_gpt56.prompt_cache_session_id = None;
        pre_gpt56.prompt_cache_lineage_id = None;
        pre_gpt56.interaction_kind = ModelInteractionKind::AutoSizing;

        let mut same_agent = pre_gpt56.clone();
        same_agent.turn_id = "turn-2".to_string();
        let mut other_agent = pre_gpt56.clone();
        other_agent.agent_id = "agent-2".to_string();

        let first = openai_prompt_cache_diagnostics_for_request(&pre_gpt56).unwrap();
        let repeated = openai_prompt_cache_diagnostics_for_request(&same_agent).unwrap();
        let other = openai_prompt_cache_diagnostics_for_request(&other_agent).unwrap();

        assert_eq!(first.prompt_cache_key_purpose, "internal_router");
        assert_eq!(first.prompt_cache_key, repeated.prompt_cache_key);
        assert_eq!(
            first.prompt_cache_partition_sha256,
            repeated.prompt_cache_partition_sha256
        );
        assert_ne!(first.prompt_cache_partition_sha256, "agent-1");
        assert_ne!(other.prompt_cache_partition_sha256, "agent-2");

        let mut gpt56 = pre_gpt56.clone();
        gpt56.model = "gpt-5.6".to_string();
        let gpt56_diagnostics = openai_prompt_cache_diagnostics_for_request(&gpt56).unwrap();

        assert_eq!(
            gpt56_diagnostics.prompt_cache_key_purpose,
            "internal_router"
        );
        assert_ne!(first.prompt_cache_key, gpt56_diagnostics.prompt_cache_key);
        assert_ne!(
            first.prompt_cache_partition_sha256,
            gpt56_diagnostics.prompt_cache_partition_sha256
        );

        let mut routed_handoff = pre_gpt56.clone();
        routed_handoff.interaction_kind = ModelInteractionKind::RoutedHandoff;
        routed_handoff.prompt_cache_session_id = Some("session-1".to_string());
        let mut routed_handoff_repair = routed_handoff.clone();
        routed_handoff_repair.interaction_kind = ModelInteractionKind::RoutedHandoffRepair;
        routed_handoff_repair.prompt_cache_session_id = None;

        let routed_handoff = openai_prompt_cache_diagnostics_for_request(&routed_handoff).unwrap();
        let routed_handoff_repair =
            openai_prompt_cache_diagnostics_for_request(&routed_handoff_repair).unwrap();

        assert_eq!(routed_handoff.prompt_cache_key_purpose, "internal_workflow");
        assert_eq!(
            routed_handoff_repair.prompt_cache_key_purpose,
            "internal_workflow"
        );
    }
}
