//! Runtime provider model catalog helpers.
//!
//! This module owns runtime provider model catalog lookup, cache refresh,
//! provider-backed model listing, configured fallback catalog construction,
//! and markdown rendering helpers shared by model-oriented commands. Keeping
//! catalog mechanics separate from model selection keeps `/model` command
//! execution focused on state transitions and profile overrides.

use super::{
    AgentShellCommandOutcome, AsyncModelProvider, AuthCredentialKind, DEFAULT_PROVIDER_TIMEOUT_MS,
    MezError, ModelCatalog, ModelCatalogCandidate, ModelCatalogEntry, ModelCatalogInput,
    ModelCatalogSource, ModelProfile, ProviderApiCompatibility, ProviderModelCatalog,
    ProviderModelInfo, ProviderQuotaUsage, ReqwestProviderHttpTransport, Result,
    RuntimeSessionService,
    deepseek_chat_completions_provider_from_auth_store_with_provider_options, json_escape,
    normalize_model_catalog_values,
    openai_compatible_provider_from_auth_store_with_provider_options,
    openai_default_reasoning_levels_for_model,
    openai_responses_provider_from_auth_store_with_provider_options, parse_slash_command,
    resolve_provider_api, runtime_default_models_for_provider,
    runtime_recommended_model_for_provider,
};
use futures_util::StreamExt;

/// Immutable provider catalog inputs moved out of the serialized runtime actor.
pub(crate) struct RuntimeProviderInfoRefreshWork {
    entries: Vec<RuntimeProviderInfoRefreshEntry>,
    auth_store: Option<crate::security::auth::AuthStore>,
}

/// Captures one interactive prompt refresh before its provider I/O runs outside
/// the serialized runtime actor.
pub(crate) struct RuntimeAgentPromptProviderInfoRefresh {
    /// Primary client that submitted the prompt command.
    pub(crate) primary_client_id: mez_core::ids::ClientId,
    /// Pane whose prompt should receive the completed refresh output.
    pub(crate) pane_id: String,
    /// Original slash-command text retained for response rendering.
    pub(crate) input: String,
    /// Immutable provider catalog inputs for worker-owned I/O.
    pub(crate) work: Option<RuntimeProviderInfoRefreshWork>,
}

impl std::fmt::Debug for RuntimeAgentPromptProviderInfoRefresh {
    /// Formats prompt-refresh metadata without exposing worker-owned credentials.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeAgentPromptProviderInfoRefresh")
            .field("primary_client_id", &self.primary_client_id)
            .field("pane_id", &self.pane_id)
            .field("input", &self.input)
            .finish_non_exhaustive()
    }
}

/// One configured provider and its actor-computed fallback catalog.
struct RuntimeProviderInfoRefreshEntry {
    provider_id: String,
    provider_config: crate::runtime::RuntimeProviderConfig,
    fallback: RuntimeModelCatalog,
}

/// Provider catalog results returned to actor-owned cache state.
pub(crate) struct RuntimeProviderInfoRefreshOutcome {
    entries: Vec<RuntimeProviderInfoRefreshEntryOutcome>,
}

/// One provider worker result plus its credential-audit classification.
struct RuntimeProviderInfoRefreshEntryOutcome {
    entry: RuntimeProviderInfoRefreshEntry,
    result: Result<RuntimeModelCatalog>,
    credential_outcome: Option<&'static str>,
}

impl RuntimeSessionService {
    /// Validates one `/refresh-provider-info` command before refresh work starts.
    pub(super) fn validate_agent_shell_refresh_provider_info_command(
        &self,
        input: &str,
    ) -> Result<()> {
        let slash = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("refresh-provider-info command must be a slash command")
        })?;
        if !slash.args.trim().is_empty() {
            return Err(MezError::invalid_args(
                "refresh-provider-info does not accept arguments",
            ));
        }
        Ok(())
    }

    /// Executes `/refresh-provider-info` without actor orchestration.
    pub(super) async fn execute_agent_shell_refresh_provider_info_command(
        &mut self,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        self.validate_agent_shell_refresh_provider_info_command(input)?;
        Ok(AgentShellCommandOutcome::Display {
            command: "refresh-provider-info".to_string(),
            body: self.refresh_provider_info_async().await?,
        })
    }

    pub(super) fn runtime_model_catalog_for_provider(
        &mut self,
        provider_id: &str,
    ) -> Result<RuntimeModelCatalog> {
        let provider_config = self
            .provider_registry()
            .provider(provider_id)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!("provider `{provider_id}` is not configured"))
            })?;
        let observed = self.cached_provider_model_catalog(provider_id);
        Ok(runtime_effective_model_catalog(
            provider_id,
            &provider_config,
            self.provider_registry(),
            observed.as_ref(),
        ))
    }

    /// Runs the runtime model catalog for provider async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[allow(
        dead_code,
        reason = "retained for direct service adapters outside the serialized actor"
    )]
    async fn runtime_model_catalog_for_provider_async(
        &mut self,
        provider_id: &str,
    ) -> Result<RuntimeModelCatalog> {
        let provider_config = self
            .provider_registry()
            .provider(provider_id)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!("provider `{provider_id}` is not configured"))
            })?;
        let fallback = runtime_configured_model_catalog(
            provider_id,
            &provider_config,
            self.provider_registry(),
        );
        if let Some(catalog) = self.cached_provider_model_catalog(provider_id) {
            return Ok(runtime_effective_model_catalog(
                provider_id,
                &provider_config,
                self.provider_registry(),
                Some(&catalog),
            ));
        }
        match resolve_provider_api(&provider_config.kind, provider_config.api.as_deref())? {
            ProviderApiCompatibility::OpenAiResponses
            | ProviderApiCompatibility::OpenAiChatCompletions
            | ProviderApiCompatibility::DeepSeekChatCompletions => match self
                .runtime_api_model_catalog_async(provider_id, &provider_config)
                .await
            {
                Ok(catalog) => {
                    let catalog = RuntimeModelCatalog::from_provider(catalog);
                    self.cache_provider_model_catalog(provider_id, catalog.clone())?;
                    Ok(runtime_effective_model_catalog(
                        provider_id,
                        &provider_config,
                        self.provider_registry(),
                        Some(&catalog),
                    ))
                }
                Err(_error) => Ok(fallback),
            },
            ProviderApiCompatibility::AnthropicMessages => Ok(fallback),
        }
    }

    /// Captures configured provider catalog inputs before external worker I/O.
    pub(crate) fn prepare_provider_info_refresh(
        &mut self,
    ) -> Result<RuntimeProviderInfoRefreshWork> {
        let registry = self.provider_registry().clone();
        let mut entries = Vec::with_capacity(registry.providers().len());
        for (provider_id, provider_config) in registry.providers() {
            let fallback =
                runtime_configured_model_catalog(provider_id, provider_config, &registry);
            if matches!(
                resolve_provider_api(&provider_config.kind, provider_config.api.as_deref())?,
                ProviderApiCompatibility::OpenAiResponses
                    | ProviderApiCompatibility::OpenAiChatCompletions
                    | ProviderApiCompatibility::DeepSeekChatCompletions
            ) {
                self.append_credential_access_audit(
                    provider_id,
                    &provider_config.auth_profile,
                    "provider_model_list",
                    "requested",
                )?;
            }
            entries.push(RuntimeProviderInfoRefreshEntry {
                provider_id: provider_id.clone(),
                provider_config: provider_config.clone(),
                fallback,
            });
        }
        Ok(RuntimeProviderInfoRefreshWork {
            entries,
            auth_store: self.integration.auth_store().cloned(),
        })
    }

    /// Fetches configured provider catalogs concurrently outside the actor.
    pub(crate) async fn execute_provider_info_refresh(
        work: RuntimeProviderInfoRefreshWork,
    ) -> RuntimeProviderInfoRefreshOutcome {
        let auth_store = work.auth_store;
        let entries = futures_util::stream::iter(work.entries.into_iter().map(|entry| {
            let auth_store = auth_store.clone();
            async move { execute_provider_info_refresh_entry(entry, auth_store.as_ref()).await }
        }))
        .buffer_unordered(4)
        .collect()
        .await;
        RuntimeProviderInfoRefreshOutcome { entries }
    }

    /// Applies worker results to the current provider cache and renders status.
    pub(crate) fn apply_provider_info_refresh(
        &mut self,
        outcome: RuntimeProviderInfoRefreshOutcome,
    ) -> Result<String> {
        let provider_count = outcome.entries.len();
        let mut refreshed = 0usize;
        let mut failed = 0usize;
        let mut lines = Vec::new();
        for entry_outcome in outcome.entries {
            let provider_id = entry_outcome.entry.provider_id;
            let provider_config = entry_outcome.entry.provider_config;
            if self.provider_registry().provider(&provider_id) != Some(&provider_config) {
                failed = failed.saturating_add(1);
                lines.push(format!(
                    "{} refresh=stale error=provider configuration changed during refresh",
                    json_escape(&provider_id)
                ));
                continue;
            }
            if let Some(credential_outcome) = entry_outcome.credential_outcome {
                self.append_credential_access_audit(
                    &provider_id,
                    &provider_config.auth_profile,
                    "provider_model_list",
                    credential_outcome,
                )?;
            }
            self.remove_cached_provider_model_catalog(&provider_id)?;
            match entry_outcome.result {
                Ok(catalog) => {
                    refreshed = refreshed.saturating_add(1);
                    self.cache_provider_model_catalog(&provider_id, catalog.clone())?;
                    let provider_error = catalog
                        .provider_error
                        .as_deref()
                        .map(runtime_model_catalog_unavailable_reason)
                        .unwrap_or_else(|| "none".to_string());
                    lines.push(format!(
                        "{} source={} models={} reasoning_levels={} quota_entries={} provider_error={}",
                        json_escape(&provider_id),
                        json_escape(&catalog.source),
                        catalog.catalog.entries().len(),
                        catalog.catalog.reasoning_levels().len(),
                        catalog.quota_usage.len(),
                        provider_error
                    ));
                }
                Err(error) => {
                    failed = failed.saturating_add(1);
                    lines.push(format!(
                        "{} refresh=failed error={}",
                        json_escape(&provider_id),
                        json_escape(error.message())
                    ));
                }
            }
        }
        let mut body = format!(
            "providers={} refreshed={} failed={}",
            provider_count, refreshed, failed
        );
        if !lines.is_empty() {
            body.push('\n');
            body.push_str(&lines.join("\n"));
        }
        Ok(body)
    }

    /// Refreshes provider information without requiring actor orchestration.
    pub(crate) async fn refresh_provider_info_async(&mut self) -> Result<String> {
        let work = self.prepare_provider_info_refresh()?;
        let outcome = Self::execute_provider_info_refresh(work).await;
        self.apply_provider_info_refresh(outcome)
    }

    /// Seeds the live model catalog cache for focused runtime tests.
    #[cfg(test)]
    pub(crate) fn cache_provider_model_catalog_for_tests(
        &mut self,
        provider_id: &str,
        models: Vec<ProviderModelInfo>,
        reasoning_levels: Vec<String>,
    ) {
        self.cache_provider_model_catalog(
            provider_id,
            RuntimeModelCatalog::from_provider(ProviderModelCatalog {
                provider: provider_id.to_string(),
                source: "provider".to_string(),
                models,
                reasoning_levels,
                quota_usage: Vec::new(),
            }),
        )
        .expect("test provider catalog should rematerialize profiles");
    }

    /// Runs the runtime API model catalog async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[allow(
        dead_code,
        reason = "retained for direct service adapters outside the serialized actor"
    )]
    async fn runtime_api_model_catalog_async(
        &mut self,
        provider_id: &str,
        provider_config: &crate::runtime::RuntimeProviderConfig,
    ) -> Result<ProviderModelCatalog> {
        let api = resolve_provider_api(&provider_config.kind, provider_config.api.as_deref())?;
        self.append_credential_access_audit(
            provider_id,
            &provider_config.auth_profile,
            "provider_model_list",
            "requested",
        )?;
        let Some(auth_store) = self.integration.auth_store() else {
            self.append_credential_access_audit(
                provider_id,
                &provider_config.auth_profile,
                "provider_model_list",
                "denied",
            )?;
            return Err(MezError::invalid_state(
                "provider model listing requires an attached auth store",
            ));
        };
        let metadata = auth_store
            .read_metadata_for_provider(provider_id)?
            .ok_or_else(|| {
                MezError::invalid_state(format!(
                    "provider `{provider_id}` model listing requires an authenticated provider"
                ))
            })?;
        if metadata.credential_kind == AuthCredentialKind::ChatGpt {
            self.append_credential_access_audit(
                provider_id,
                &provider_config.auth_profile,
                "provider_model_list",
                "unsupported",
            )?;
            return Err(MezError::invalid_state(
                "ChatGPT browser credentials do not expose an OpenAI-compatible model catalog",
            ));
        }
        let endpoint_override = provider_config
            .base_url
            .as_deref()
            .filter(|endpoint| !endpoint.is_empty());
        let provider_result: Result<Box<dyn AsyncModelProvider>> = match api {
            ProviderApiCompatibility::OpenAiResponses => {
                openai_responses_provider_from_auth_store_with_provider_options(
                    auth_store,
                    provider_id,
                    endpoint_override,
                    &provider_config.options,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(|provider| Box::new(provider) as Box<dyn AsyncModelProvider>)
            }
            ProviderApiCompatibility::OpenAiChatCompletions => {
                openai_compatible_provider_from_auth_store_with_provider_options(
                    auth_store,
                    provider_id,
                    endpoint_override,
                    &provider_config.options,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(|provider| Box::new(provider) as Box<dyn AsyncModelProvider>)
            }
            ProviderApiCompatibility::DeepSeekChatCompletions => {
                deepseek_chat_completions_provider_from_auth_store_with_provider_options(
                    auth_store,
                    provider_id,
                    endpoint_override,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(|provider| Box::new(provider) as Box<dyn AsyncModelProvider>)
            }
            ProviderApiCompatibility::AnthropicMessages => Err(MezError::invalid_state(
                "Anthropic provider model listing is not implemented yet",
            )),
        };
        let provider = match provider_result {
            Ok(provider) => {
                self.append_credential_access_audit(
                    provider_id,
                    &provider_config.auth_profile,
                    "provider_model_list",
                    "granted",
                )?;
                provider
            }
            Err(error) => {
                self.append_credential_access_audit(
                    provider_id,
                    &provider_config.auth_profile,
                    "provider_model_list",
                    "denied",
                )?;
                return Err(error);
            }
        };
        provider.list_models_async().await
    }
}

/// Fetches one provider catalog without borrowing actor-owned runtime state.
async fn execute_provider_info_refresh_entry(
    entry: RuntimeProviderInfoRefreshEntry,
    auth_store: Option<&crate::security::auth::AuthStore>,
) -> RuntimeProviderInfoRefreshEntryOutcome {
    let api = match resolve_provider_api(
        &entry.provider_config.kind,
        entry.provider_config.api.as_deref(),
    ) {
        Ok(api) => api,
        Err(error) => {
            return RuntimeProviderInfoRefreshEntryOutcome {
                entry,
                result: Err(MezError::from(error)),
                credential_outcome: None,
            };
        }
    };
    if api == ProviderApiCompatibility::AnthropicMessages {
        let fallback = entry.fallback.clone();
        return RuntimeProviderInfoRefreshEntryOutcome {
            entry,
            result: Ok(fallback),
            credential_outcome: None,
        };
    }
    let Some(auth_store) = auth_store else {
        let fallback = entry.fallback.clone();
        return RuntimeProviderInfoRefreshEntryOutcome {
            entry,
            result: Ok(fallback),
            credential_outcome: Some("denied"),
        };
    };
    let metadata = match auth_store.read_metadata_for_provider(&entry.provider_id) {
        Ok(Some(metadata)) => metadata,
        Ok(None) | Err(_) => {
            let fallback = entry.fallback.clone();
            return RuntimeProviderInfoRefreshEntryOutcome {
                entry,
                result: Ok(fallback),
                credential_outcome: Some("denied"),
            };
        }
    };
    if metadata.credential_kind == AuthCredentialKind::ChatGpt {
        let fallback = entry.fallback.clone();
        return RuntimeProviderInfoRefreshEntryOutcome {
            entry,
            result: Ok(fallback),
            credential_outcome: Some("unsupported"),
        };
    }
    let endpoint_override = entry
        .provider_config
        .base_url
        .as_deref()
        .filter(|endpoint| !endpoint.is_empty());
    let provider: Result<Box<dyn AsyncModelProvider>> = match api {
        ProviderApiCompatibility::OpenAiResponses => {
            openai_responses_provider_from_auth_store_with_provider_options(
                auth_store,
                &entry.provider_id,
                endpoint_override,
                &entry.provider_config.options,
                DEFAULT_PROVIDER_TIMEOUT_MS,
                ReqwestProviderHttpTransport,
            )
            .map(|provider| Box::new(provider) as Box<dyn AsyncModelProvider>)
        }
        ProviderApiCompatibility::OpenAiChatCompletions => {
            openai_compatible_provider_from_auth_store_with_provider_options(
                auth_store,
                &entry.provider_id,
                endpoint_override,
                &entry.provider_config.options,
                DEFAULT_PROVIDER_TIMEOUT_MS,
                ReqwestProviderHttpTransport,
            )
            .map(|provider| Box::new(provider) as Box<dyn AsyncModelProvider>)
        }
        ProviderApiCompatibility::DeepSeekChatCompletions => {
            deepseek_chat_completions_provider_from_auth_store_with_provider_options(
                auth_store,
                &entry.provider_id,
                endpoint_override,
                DEFAULT_PROVIDER_TIMEOUT_MS,
                ReqwestProviderHttpTransport,
            )
            .map(|provider| Box::new(provider) as Box<dyn AsyncModelProvider>)
        }
        ProviderApiCompatibility::AnthropicMessages => unreachable!(),
    };
    let Ok(provider) = provider else {
        let fallback = entry.fallback.clone();
        return RuntimeProviderInfoRefreshEntryOutcome {
            entry,
            result: Ok(fallback),
            credential_outcome: Some("denied"),
        };
    };
    let fallback = entry.fallback.clone();
    let result = provider
        .list_models_async()
        .await
        .map(RuntimeModelCatalog::from_provider)
        .or(Ok(fallback));
    RuntimeProviderInfoRefreshEntryOutcome {
        entry,
        result,
        credential_outcome: Some("granted"),
    }
}

/// Carries Runtime Model Catalog state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeModelCatalog {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) provider: String,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) source: String,
    /// Stores the provider error value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) provider_error: Option<String>,
    /// Provider-neutral normalized catalog policy state.
    pub(super) catalog: ModelCatalog,
    /// Provider-reported quota usage percentages from the catalog request.
    pub(super) quota_usage: Vec<ProviderQuotaUsage>,
}

impl RuntimeModelCatalog {
    /// Returns the normalized provider-neutral catalog used for profile rebasing.
    pub(crate) fn catalog(&self) -> &ModelCatalog {
        &self.catalog
    }

    /// Runs the from provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_provider(catalog: ProviderModelCatalog) -> Self {
        let ProviderModelCatalog {
            provider,
            source,
            models,
            reasoning_levels,
            quota_usage,
        } = catalog;
        Self {
            provider,
            source,
            provider_error: None,
            catalog: ModelCatalog::from_input(ModelCatalogInput {
                candidates: models
                    .into_iter()
                    .map(|model| {
                        ModelCatalogCandidate::available(ModelCatalogSource::Discovered, model)
                    })
                    .collect(),
                reasoning_levels,
                ..ModelCatalogInput::default()
            }),
            quota_usage,
        }
    }
}

/// Builds one effective catalog from configured, built-in, profile, and
/// provider-discovered model observations.
fn runtime_effective_model_catalog(
    provider_id: &str,
    provider_config: &crate::runtime::RuntimeProviderConfig,
    registry: &crate::runtime::RuntimeProviderRegistry,
    observed: Option<&RuntimeModelCatalog>,
) -> RuntimeModelCatalog {
    let mut candidates =
        runtime_configured_model_catalog_candidates(provider_id, provider_config, registry);
    if let Some(observed) = observed {
        candidates.extend(
            observed
                .catalog
                .entries()
                .iter()
                .map(ModelCatalogEntry::to_candidate),
        );
    }
    let reasoning_levels = normalize_model_catalog_values(
        observed
            .into_iter()
            .flat_map(|catalog| catalog.catalog.reasoning_levels().iter().cloned())
            .collect(),
    );
    let recommended_model = runtime_provider_recommended_model(provider_config);
    RuntimeModelCatalog {
        provider: provider_id.to_string(),
        source: observed
            .map(|catalog| catalog.source.clone())
            .unwrap_or_else(|| "config".to_string()),
        provider_error: observed.and_then(|catalog| catalog.provider_error.clone()),
        catalog: ModelCatalog::from_input(ModelCatalogInput {
            candidates,
            default_model: provider_config.default_model.clone(),
            recommended_model: recommended_model.map(str::to_string),
            reasoning_levels,
        }),
        quota_usage: observed
            .map(|catalog| catalog.quota_usage.clone())
            .unwrap_or_default(),
    }
}

/// Runs the runtime configured model catalog operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_configured_model_catalog(
    provider_id: &str,
    provider_config: &crate::runtime::RuntimeProviderConfig,
    registry: &crate::runtime::RuntimeProviderRegistry,
) -> RuntimeModelCatalog {
    runtime_effective_model_catalog(provider_id, provider_config, registry, None)
}

/// Collects raw local catalog candidates without collapsing field provenance.
fn runtime_configured_model_catalog_candidates(
    provider_id: &str,
    provider_config: &crate::runtime::RuntimeProviderConfig,
    registry: &crate::runtime::RuntimeProviderRegistry,
) -> Vec<ModelCatalogCandidate> {
    let mut candidates = Vec::new();
    if let Some(default_model) = provider_config.default_model.as_deref()
        && !default_model.is_empty()
        && provider_config
            .model(default_model)
            .ok()
            .flatten()
            .is_none()
    {
        candidates.push(runtime_catalog_candidate(
            default_model,
            runtime_configured_reasoning_levels_for_model(provider_config, default_model),
            ModelCatalogSource::Configured,
        ));
    }
    for model in &provider_config.models {
        candidates.push(runtime_catalog_candidate(
            &model.id,
            runtime_configured_reasoning_levels_for_model(provider_config, &model.id),
            ModelCatalogSource::Default,
        ));
        candidates.push(ModelCatalogCandidate::configured(model));
    }
    let default_models = if provider_config.models.is_empty() {
        runtime_provider_default_models(provider_config)
    } else {
        Vec::new()
    };
    for model in &default_models {
        candidates.push(runtime_catalog_candidate(
            model,
            runtime_configured_reasoning_levels_for_model(provider_config, model),
            ModelCatalogSource::Default,
        ));
    }
    for profile in registry
        .profiles()
        .values()
        .filter(|profile| profile.provider == provider_id)
    {
        if provider_config
            .model(&profile.model)
            .ok()
            .flatten()
            .is_some()
        {
            continue;
        }
        let mut reasoning_levels =
            runtime_configured_reasoning_levels_for_model(provider_config, &profile.model);
        if let Some(reasoning) = profile.reasoning_profile.as_deref() {
            reasoning_levels.push(reasoning.to_string());
        }
        candidates.push(runtime_catalog_candidate(
            &profile.model,
            reasoning_levels,
            ModelCatalogSource::Configured,
        ));
    }
    if !candidates
        .iter()
        .any(|candidate: &ModelCatalogCandidate| !candidate.model.id.trim().is_empty())
        && let Some(recommended_model) = runtime_provider_recommended_model(provider_config)
    {
        candidates.push(runtime_catalog_candidate(
            recommended_model,
            runtime_configured_reasoning_levels_for_model(provider_config, recommended_model),
            ModelCatalogSource::Recommended,
        ));
    }
    candidates
}

/// Converts one resolved product model into a provider-neutral catalog candidate.
///
/// Product configuration and provider API interpretation happen before this
/// function; lower-crate policy receives only model metadata and source.
fn runtime_catalog_candidate(
    model: &str,
    reasoning_levels: Vec<String>,
    source: ModelCatalogSource,
) -> ModelCatalogCandidate {
    ModelCatalogCandidate::available(
        source,
        ProviderModelInfo {
            id: model.to_string(),
            display_name: None,
            reasoning_levels,
            context_window_tokens: mez_agent::known_model_context_window_tokens(model),
            max_input_tokens: None,
            max_output_tokens: None,
            capabilities: Vec::new(),
        },
    )
}

/// Runs the runtime configured reasoning levels for model operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_configured_reasoning_levels_for_model(
    provider_config: &crate::runtime::RuntimeProviderConfig,
    model: &str,
) -> Vec<String> {
    let mut levels = provider_config
        .options
        .get("reasoning_effort")
        .or_else(|| provider_config.options.get("reasoning_profile"))
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if let Ok(provider_api) =
        resolve_provider_api(&provider_config.kind, provider_config.api.as_deref())
    {
        match provider_api {
            ProviderApiCompatibility::OpenAiResponses => {
                levels.extend(openai_default_reasoning_levels_for_model(model));
            }
            ProviderApiCompatibility::DeepSeekChatCompletions => {
                levels.extend(deepseek_default_reasoning_effort_levels());
            }
            ProviderApiCompatibility::AnthropicMessages => {
                levels.extend(anthropic_default_reasoning_effort_levels());
            }
            ProviderApiCompatibility::OpenAiChatCompletions => {}
        }
    }
    normalize_model_catalog_values(levels)
}

/// Returns built-in default models only when the provider's selected API keeps
/// the provider's built-in model catalog semantics.
pub(super) fn runtime_provider_default_models(
    provider_config: &crate::runtime::RuntimeProviderConfig,
) -> Vec<String> {
    match resolve_provider_api(&provider_config.kind, provider_config.api.as_deref()) {
        Ok(ProviderApiCompatibility::OpenAiResponses) if provider_config.kind == "openai" => {
            runtime_default_models_for_provider(&provider_config.kind)
                .map(|models| models.iter().map(|model| (*model).to_string()).collect())
                .unwrap_or_default()
        }
        Ok(ProviderApiCompatibility::AnthropicMessages) if provider_config.kind == "anthropic" => {
            runtime_default_models_for_provider(&provider_config.kind)
                .map(|models| models.iter().map(|model| (*model).to_string()).collect())
                .unwrap_or_default()
        }
        Ok(ProviderApiCompatibility::DeepSeekChatCompletions)
            if provider_config.kind == "deepseek" =>
        {
            runtime_default_models_for_provider(&provider_config.kind)
                .map(|models| models.iter().map(|model| (*model).to_string()).collect())
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Returns a built-in recommended model only when the provider and API share
/// the built-in provider's catalog contract.
fn runtime_provider_recommended_model(
    provider_config: &crate::runtime::RuntimeProviderConfig,
) -> Option<&'static str> {
    match resolve_provider_api(&provider_config.kind, provider_config.api.as_deref()) {
        Ok(ProviderApiCompatibility::OpenAiResponses) if provider_config.kind == "openai" => {
            runtime_recommended_model_for_provider(&provider_config.kind).ok()
        }
        Ok(ProviderApiCompatibility::AnthropicMessages) if provider_config.kind == "anthropic" => {
            runtime_recommended_model_for_provider(&provider_config.kind).ok()
        }
        Ok(ProviderApiCompatibility::DeepSeekChatCompletions)
            if provider_config.kind == "deepseek" =>
        {
            runtime_recommended_model_for_provider(&provider_config.kind).ok()
        }
        _ => None,
    }
}

/// Returns the reasoning effort levels supported by DeepSeek providers.
fn deepseek_default_reasoning_effort_levels() -> Vec<String> {
    vec!["high".to_string(), "max".to_string()]
}

/// Returns the reasoning effort levels supported by Anthropic Messages.
fn anthropic_default_reasoning_effort_levels() -> Vec<String> {
    ["low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Formats the current routing auto-sizing model profile.
pub(super) fn runtime_routing_model_profile_display(
    routing_name: &str,
    routing_profile: &ModelProfile,
    active_profile: &ModelProfile,
) -> String {
    format!(
        "scope=routing profile={} provider={} model={} reasoning_profile={} active_provider={} active_model={} source=runtime-routing-model",
        json_escape(routing_name),
        json_escape(&routing_profile.provider),
        json_escape(&routing_profile.model),
        routing_profile
            .reasoning_profile
            .as_deref()
            .unwrap_or("none"),
        json_escape(&active_profile.provider),
        json_escape(&active_profile.model)
    )
}

/// Runs the runtime model catalog display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_catalog_display(
    active_name: &str,
    active_profile: &ModelProfile,
    catalog: &RuntimeModelCatalog,
) -> String {
    let context_limit = format!("{} tokens", active_profile.context_window_tokens());
    let mut lines = vec!["## Model Catalog".to_string(), String::new()];
    if let Some(error) = catalog.provider_error.as_deref() {
        lines.push(format!(
            "**Provider catalog unavailable:** `{}`",
            runtime_model_catalog_unavailable_reason(error)
        ));
        lines.push(String::new());
    }
    if !catalog.catalog.reasoning_levels().is_empty() {
        lines.push(format!(
            "**Reasoning levels:** `{}`",
            catalog.catalog.reasoning_levels().join(", ")
        ));
        lines.push(String::new());
    }
    let model_rows = catalog
        .catalog
        .entries()
        .iter()
        .map(|model| {
            let model_name = runtime_model_catalog_model_name(model);
            let active_model =
                catalog.provider == active_profile.provider && model.id == active_profile.model;
            let model_name = if active_model {
                format!("★ {model_name}")
            } else {
                model_name
            };
            vec![
                catalog.provider.clone(),
                model_name,
                runtime_model_catalog_reasoning_display(
                    &model.reasoning_levels,
                    active_model.then_some(active_profile.reasoning_profile.as_deref()),
                ),
                context_limit.clone(),
                if catalog.source == "config" {
                    "config".to_string()
                } else {
                    model.source.as_str().to_string()
                },
                if active_model {
                    active_name.to_string()
                } else {
                    String::new()
                },
            ]
        })
        .collect::<Vec<_>>();
    if !model_rows.is_empty() {
        lines.extend(runtime_markdown_table(
            &[
                "Provider",
                "Model",
                "Reasoning levels",
                "Context limit",
                "Source",
                "Active profile",
            ],
            &model_rows,
        ));
    }
    lines.join("\n")
}

/// Formats a provider model name with optional display metadata.
fn runtime_model_catalog_model_name(model: &ModelCatalogEntry) -> String {
    match model.display_name.as_deref() {
        Some(display_name) if !display_name.is_empty() => {
            format!("{} ({display_name})", model.id)
        }
        _ => model.id.clone(),
    }
}

/// Formats reasoning choices and marks the active reasoning level.
fn runtime_model_catalog_reasoning_display(
    levels: &[String],
    active_reasoning: Option<Option<&str>>,
) -> String {
    let mut values = if levels.is_empty() {
        vec!["default".to_string()]
    } else {
        levels.to_vec()
    };
    let active = active_reasoning.flatten().unwrap_or("default");
    if !values.iter().any(|level| level == active) {
        values.insert(0, active.to_string());
    }
    if active_reasoning.is_some() {
        values
            .into_iter()
            .map(|level| {
                if level == active {
                    format!("★ {level}")
                } else {
                    level
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        values.join(", ")
    }
}

/// Builds a plain markdown table from already formatted cell values.
pub(super) fn runtime_markdown_table(headers: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let header = headers
        .iter()
        .map(|cell| runtime_markdown_table_cell(cell))
        .collect::<Vec<_>>()
        .join(" | ");
    let separator = headers
        .iter()
        .map(|_| "---")
        .collect::<Vec<_>>()
        .join(" | ");
    let mut lines = vec![format!("| {header} |"), format!("| {separator} |")];
    lines.extend(rows.iter().map(|row| {
        let cells = row
            .iter()
            .map(|cell| runtime_markdown_table_cell(cell))
            .collect::<Vec<_>>()
            .join(" | ");
        format!("| {cells} |")
    }));
    lines
}

/// Escapes markdown table separators without changing the copyable value.
fn runtime_markdown_table_cell(value: &str) -> String {
    value.replace('|', r"\|").replace('\n', "<br>")
}

/// Runs the runtime model catalog unavailable reason operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_model_catalog_unavailable_reason(error: &str) -> String {
    if error.contains("ChatGPT browser credentials") {
        "browser-auth-catalog-unsupported".to_string()
    } else if error.contains("api.model.read") || error.contains("Missing scopes") {
        "missing-model-read-scope".to_string()
    } else if error.contains("attached auth store") || error.contains("authenticated provider") {
        "auth-unavailable".to_string()
    } else if error.contains("has not been refreshed") {
        "provider-info-not-refreshed".to_string()
    } else if error.contains("Models API returned status")
        || error.contains("model catalog")
        || error.contains("provider HTTP request failed")
    {
        "live-provider-catalog-unavailable".to_string()
    } else {
        error.replace(char::is_whitespace, "-")
    }
}

#[cfg(test)]
mod tests {
    use super::runtime_configured_reasoning_levels_for_model;
    use std::collections::BTreeMap;

    /// Verifies configured Anthropic providers expose documented Messages API
    /// effort levels even when live model listing is unavailable.
    ///
    /// The `/model` command and pane-frame reasoning picker use configured
    /// fallback metadata for Anthropic because there is no implemented live
    /// model-catalog refresh. Anthropic documents `output_config.effort` values
    /// `low`, `medium`, `high`, `xhigh`, and `max`; the fallback must preserve
    /// a configured default first while advertising the remaining choices.
    #[test]
    fn anthropic_configured_reasoning_levels_include_api_efforts() {
        let provider_config = crate::runtime::RuntimeProviderConfig {
            provider_id: "anthropic".to_string(),
            kind: "anthropic".to_string(),
            api: Some("anthropic-messages".to_string()),
            auth_profile: "default".to_string(),
            base_url: None,
            models: vec![mez_agent::ProviderModelConfig::named("claude-fable-5")],
            default_model: Some("claude-fable-5".to_string()),
            options: BTreeMap::from([("reasoning_effort".to_string(), "high".to_string())]),
        };

        assert_eq!(
            runtime_configured_reasoning_levels_for_model(&provider_config, "claude-fable-5"),
            vec!["high", "low", "medium", "xhigh", "max"]
        );
    }
}
