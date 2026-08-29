//! Provider-neutral model catalog construction and selection policy.
//!
//! This module merges already-resolved configured, discovered, default, and
//! recommended model metadata without knowing about product configuration,
//! provider transports, credentials, caches, or UI rendering. Product adapters
//! translate their observations into candidates and apply typed selections.

use std::collections::BTreeMap;
use std::fmt;

use crate::{ProviderModelConfig, ProviderModelInfo};

/// Origin of one provider-neutral model catalog candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModelCatalogSource {
    /// Last-resort model recommended by a product adapter.
    Recommended,
    /// Built-in model supplied when explicit configuration is absent.
    Default,
    /// Model discovered through a live provider catalog.
    Discovered,
    /// Explicit model or profile supplied by resolved user configuration.
    Configured,
    /// Explicit model-profile override applied to configured base metadata.
    Profile,
}

impl ModelCatalogSource {
    /// Returns the stable source name used in diagnostics and tests.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recommended => "recommended",
            Self::Default => "default",
            Self::Discovered => "discovered",
            Self::Configured => "configured",
            Self::Profile => "profile",
        }
    }
}

/// Whether one catalog entry may be selected for new model work.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ModelAvailability {
    /// The model may be selected.
    #[default]
    Available,
    /// The model remains visible as metadata but may not be selected.
    Unavailable,
}

/// One model observation supplied to provider-neutral catalog construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalogCandidate {
    /// Candidate metadata parsed or resolved by the product adapter.
    pub model: ProviderModelInfo,
    /// Candidate origin used for deterministic metadata precedence.
    pub source: ModelCatalogSource,
    /// Alternate identifiers that may resolve to the canonical model id.
    pub aliases: Vec<String>,
    /// Optional replacement reasoning list; `Some(empty)` clears lower values.
    pub reasoning_levels: Option<Vec<String>>,
    /// Optional replacement capability list; `Some(empty)` clears lower values.
    pub capabilities: Option<Vec<String>>,
    /// Secret-free model provider options merged per key by source precedence.
    pub provider_options: BTreeMap<String, String>,
    /// Whether the observed model may be selected.
    pub availability: ModelAvailability,
}

impl ModelCatalogCandidate {
    /// Creates one available candidate without aliases.
    pub fn available(source: ModelCatalogSource, model: ProviderModelInfo) -> Self {
        let reasoning_levels =
            (!model.reasoning_levels.is_empty()).then(|| model.reasoning_levels.clone());
        let capabilities = (!model.capabilities.is_empty()).then(|| model.capabilities.clone());
        Self {
            model,
            source,
            aliases: Vec::new(),
            reasoning_levels,
            capabilities,
            provider_options: BTreeMap::new(),
            availability: ModelAvailability::Available,
        }
    }

    /// Projects reusable configured model metadata into a merge candidate.
    pub fn configured(model: &ProviderModelConfig) -> Self {
        Self {
            model: ProviderModelInfo {
                id: model.id.clone(),
                display_name: model.display_name.clone(),
                reasoning_levels: model.reasoning_levels.clone().unwrap_or_default(),
                context_window_tokens: model.context_window_tokens,
                max_input_tokens: model.max_input_tokens,
                max_output_tokens: model.max_output_tokens,
                capabilities: model.capabilities.clone().unwrap_or_default(),
            },
            source: ModelCatalogSource::Configured,
            aliases: model.aliases.clone(),
            reasoning_levels: model.reasoning_levels.clone(),
            capabilities: model.capabilities.clone(),
            provider_options: model.provider_options.clone(),
            availability: ModelAvailability::Available,
        }
    }
}

/// Explicit observations used to build one normalized model catalog.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ModelCatalogInput {
    /// Model candidates in adapter observation order.
    pub candidates: Vec<ModelCatalogCandidate>,
    /// Optional configured default model id or alias.
    pub default_model: Option<String>,
    /// Optional last-resort recommended model id or alias.
    pub recommended_model: Option<String>,
    /// Provider-wide reasoning levels not attached to individual models.
    pub reasoning_levels: Vec<String>,
}

/// One canonical model catalog entry after deterministic merging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalogEntry {
    /// Stable canonical model identifier.
    pub id: String,
    /// Optional user-facing display label.
    pub display_name: Option<String>,
    /// Ordered supported reasoning levels.
    pub reasoning_levels: Vec<String>,
    /// Known positive context-window size in tokens.
    pub context_window_tokens: Option<usize>,
    /// Known positive maximum request-input size in tokens.
    pub max_input_tokens: Option<usize>,
    /// Known positive maximum response-output size in tokens.
    pub max_output_tokens: Option<usize>,
    /// Ordered provider-neutral capability tags.
    pub capabilities: Vec<String>,
    /// Secret-free model provider options after per-key precedence merging.
    pub provider_options: BTreeMap<String, String>,
    /// Ordered alternate identifiers for selection.
    pub aliases: Vec<String>,
    /// Highest-precedence source that supplied this entry.
    pub source: ModelCatalogSource,
    /// Whether this entry may be selected.
    pub availability: ModelAvailability,
    reasoning_levels_explicit: bool,
    capabilities_explicit: bool,
}

impl ModelCatalogEntry {
    /// Reprojects canonical metadata as an input candidate for catalog merging.
    ///
    /// Product adapters use this when combining an already normalized live
    /// catalog with separately resolved configured fallback observations.
    pub fn to_candidate(&self) -> ModelCatalogCandidate {
        ModelCatalogCandidate {
            model: ProviderModelInfo {
                id: self.id.clone(),
                display_name: self.display_name.clone(),
                reasoning_levels: self.reasoning_levels.clone(),
                context_window_tokens: self.context_window_tokens,
                max_input_tokens: self.max_input_tokens,
                max_output_tokens: self.max_output_tokens,
                capabilities: self.capabilities.clone(),
            },
            source: self.source,
            aliases: self.aliases.clone(),
            reasoning_levels: self
                .reasoning_levels_explicit
                .then(|| self.reasoning_levels.clone()),
            capabilities: self
                .capabilities_explicit
                .then(|| self.capabilities.clone()),
            provider_options: self.provider_options.clone(),
            availability: self.availability,
        }
    }
}

/// Normalized provider-neutral model catalog and preferred selection.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ModelCatalog {
    entries: Vec<ModelCatalogEntry>,
    reasoning_levels: Vec<String>,
    preferred_model: Option<String>,
}

impl ModelCatalog {
    /// Builds a normalized catalog with stable id ordering and source precedence.
    ///
    /// Empty identifiers are ignored. Higher-precedence candidates override
    /// scalar metadata and availability while ordered list metadata is merged
    /// without duplicates. Missing optional metadata is filled from lower
    /// precedence candidates instead of erasing useful observations.
    pub fn from_input(input: ModelCatalogInput) -> Self {
        let mut candidates = input
            .candidates
            .into_iter()
            .filter_map(normalized_candidate)
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| candidate.source);
        let mut entries = BTreeMap::<String, ModelCatalogEntry>::new();
        for incoming in candidates {
            match entries.entry(incoming.id.clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(incoming);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    merge_catalog_entry(entry.get_mut(), incoming);
                }
            }
        }
        let entries = entries.into_values().collect::<Vec<_>>();
        let reasoning_levels = normalize_model_catalog_values(
            entries
                .iter()
                .flat_map(|entry| entry.reasoning_levels.iter().cloned())
                .chain(input.reasoning_levels)
                .collect(),
        );
        let default_model = normalized_optional_identifier(input.default_model.as_deref())
            .and_then(|requested| resolve_available_id(&entries, requested));
        let recommended_model = normalized_optional_identifier(input.recommended_model.as_deref())
            .and_then(|requested| resolve_available_id(&entries, requested));
        let preferred_model = default_model
            .or(recommended_model)
            .or_else(|| first_available_id(&entries));
        Self {
            entries,
            reasoning_levels,
            preferred_model,
        }
    }

    /// Returns canonical entries in stable model-id order.
    pub fn entries(&self) -> &[ModelCatalogEntry] {
        self.entries.as_slice()
    }

    /// Iterates selectable entries in stable model-id order.
    pub fn available_entries(&self) -> impl Iterator<Item = &ModelCatalogEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.availability == ModelAvailability::Available)
    }

    /// Returns the ordered union of catalog reasoning levels.
    pub fn reasoning_levels(&self) -> &[String] {
        self.reasoning_levels.as_slice()
    }

    /// Returns the configured default, recommended fallback, or first available
    /// canonical model id, in that order.
    pub fn preferred_model(&self) -> Option<&str> {
        self.preferred_model.as_deref()
    }

    /// Resolves a canonical model id or alias, including unavailable entries.
    pub fn resolve(&self, requested: &str) -> Option<&ModelCatalogEntry> {
        let requested = requested.trim();
        self.entries
            .iter()
            .find(|entry| entry.id == requested)
            .or_else(|| {
                self.entries
                    .iter()
                    .filter(|entry| entry.aliases.iter().any(|alias| alias == requested))
                    .max_by_key(|entry| entry.source)
            })
    }

    /// Returns model-specific reasoning levels or the catalog-wide fallback.
    pub fn reasoning_levels_for(&self, requested: &str) -> Option<&[String]> {
        self.resolve(requested).map(|entry| {
            if entry.reasoning_levels.is_empty() {
                self.reasoning_levels.as_slice()
            } else {
                entry.reasoning_levels.as_slice()
            }
        })
    }

    /// Validates one model and optional reasoning selection against the catalog.
    pub fn select(
        &self,
        requested_model: &str,
        requested_reasoning: Option<&str>,
    ) -> Result<ModelCatalogSelection, ModelCatalogSelectionError> {
        let requested_model = requested_model.trim();
        if requested_model.is_empty() {
            return Err(ModelCatalogSelectionError::new(
                ModelCatalogSelectionErrorKind::EmptyModel,
                "model name must not be empty",
            ));
        }
        let entry = self.resolve(requested_model).ok_or_else(|| {
            ModelCatalogSelectionError::new(
                ModelCatalogSelectionErrorKind::UnknownModel,
                format!("model `{requested_model}` is not available"),
            )
        })?;
        if entry.availability == ModelAvailability::Unavailable {
            return Err(ModelCatalogSelectionError::new(
                ModelCatalogSelectionErrorKind::UnavailableModel,
                format!("model `{}` is currently unavailable", entry.id),
            ));
        }
        let reasoning = requested_reasoning
            .map(str::trim)
            .map(|reasoning| {
                if reasoning.is_empty() {
                    Err(ModelCatalogSelectionError::new(
                        ModelCatalogSelectionErrorKind::EmptyReasoning,
                        "reasoning level must not be empty",
                    ))
                } else {
                    Ok(reasoning)
                }
            })
            .transpose()?;
        let levels = if entry.reasoning_levels.is_empty() {
            self.reasoning_levels.as_slice()
        } else {
            entry.reasoning_levels.as_slice()
        };
        if let Some(reasoning) = reasoning
            && !levels.is_empty()
            && !levels.iter().any(|level| level == reasoning)
        {
            return Err(ModelCatalogSelectionError::new(
                ModelCatalogSelectionErrorKind::UnknownReasoning,
                format!(
                    "reasoning level `{reasoning}` is not available for model `{}`; available={}",
                    entry.id,
                    levels.join(",")
                ),
            ));
        }
        Ok(ModelCatalogSelection {
            model: entry.clone(),
            reasoning: reasoning.map(str::to_string),
        })
    }
}

/// Validated canonical model and reasoning selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalogSelection {
    /// Canonical selected model metadata.
    pub model: ModelCatalogEntry,
    /// Validated optional reasoning level.
    pub reasoning: Option<String>,
}

/// Stable category for model catalog selection failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCatalogSelectionErrorKind {
    /// Requested model identifier was empty.
    EmptyModel,
    /// No canonical id or alias matched the request.
    UnknownModel,
    /// The matching model was explicitly unavailable.
    UnavailableModel,
    /// Requested reasoning level was empty.
    EmptyReasoning,
    /// Requested reasoning level was not advertised for the model.
    UnknownReasoning,
}

/// Typed model catalog selection failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalogSelectionError {
    kind: ModelCatalogSelectionErrorKind,
    message: String,
}

impl ModelCatalogSelectionError {
    /// Creates one typed catalog selection failure.
    fn new(kind: ModelCatalogSelectionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the stable selection failure category.
    pub fn kind(&self) -> ModelCatalogSelectionErrorKind {
        self.kind
    }

    /// Returns the provider-neutral diagnostic message.
    pub fn message(&self) -> &str {
        self.message.as_str()
    }
}

impl fmt::Display for ModelCatalogSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ModelCatalogSelectionError {}

/// Normalizes ordered model metadata values by trimming, dropping empty values,
/// and preserving the first occurrence of each value.
pub fn normalize_model_catalog_values(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !normalized.iter().any(|existing| existing == value) {
            normalized.push(value.to_string());
        }
    }
    normalized
}

/// Converts one candidate into canonical entry metadata, rejecting empty ids.
fn normalized_candidate(candidate: ModelCatalogCandidate) -> Option<ModelCatalogEntry> {
    let ModelCatalogCandidate {
        model,
        source,
        aliases,
        reasoning_levels,
        capabilities,
        provider_options,
        availability,
    } = candidate;
    let id = model.id.trim();
    if id.is_empty() {
        return None;
    }
    let aliases = normalize_model_catalog_values(aliases)
        .into_iter()
        .filter(|alias| alias != id)
        .collect();
    let reasoning_levels_explicit = reasoning_levels.is_some();
    let capabilities_explicit = capabilities.is_some();
    Some(ModelCatalogEntry {
        id: id.to_string(),
        display_name: model
            .display_name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty()),
        reasoning_levels: normalize_model_catalog_values(
            reasoning_levels.unwrap_or(model.reasoning_levels),
        ),
        context_window_tokens: model.context_window_tokens.filter(|limit| *limit > 0),
        max_input_tokens: model.max_input_tokens.filter(|limit| *limit > 0),
        max_output_tokens: model.max_output_tokens.filter(|limit| *limit > 0),
        capabilities: normalize_model_catalog_values(capabilities.unwrap_or(model.capabilities)),
        provider_options,
        aliases,
        source,
        availability,
        reasoning_levels_explicit,
        capabilities_explicit,
    })
}

/// Merges one duplicate entry according to source precedence.
fn merge_catalog_entry(existing: &mut ModelCatalogEntry, incoming: ModelCatalogEntry) {
    debug_assert!(incoming.source >= existing.source);
    if incoming.display_name.is_some() {
        existing.display_name = incoming.display_name;
    }
    if incoming.context_window_tokens.is_some() {
        existing.context_window_tokens = incoming.context_window_tokens;
    }
    if incoming.max_input_tokens.is_some() {
        existing.max_input_tokens = incoming.max_input_tokens;
    }
    if incoming.max_output_tokens.is_some() {
        existing.max_output_tokens = incoming.max_output_tokens;
    }
    if incoming.reasoning_levels_explicit {
        existing.reasoning_levels = incoming.reasoning_levels;
        existing.reasoning_levels_explicit = true;
    }
    if incoming.capabilities_explicit {
        existing.capabilities = incoming.capabilities;
        existing.capabilities_explicit = true;
    }
    existing.provider_options.extend(incoming.provider_options);
    existing.aliases = normalize_model_catalog_values(
        incoming
            .aliases
            .into_iter()
            .chain(std::mem::take(&mut existing.aliases))
            .collect(),
    );
    existing.source = incoming.source;
    existing.availability = incoming.availability;
}

/// Returns a normalized optional identifier.
fn normalized_optional_identifier(identifier: Option<&str>) -> Option<&str> {
    identifier
        .map(str::trim)
        .filter(|identifier| !identifier.is_empty())
}

/// Resolves one available canonical id or alias from normalized entries.
fn resolve_available_id(entries: &[ModelCatalogEntry], requested: &str) -> Option<String> {
    entries
        .iter()
        .find(|entry| entry.availability == ModelAvailability::Available && entry.id == requested)
        .or_else(|| {
            entries
                .iter()
                .filter(|entry| {
                    entry.availability == ModelAvailability::Available
                        && entry.aliases.iter().any(|alias| alias == requested)
                })
                .max_by_key(|entry| entry.source)
        })
        .map(|entry| entry.id.clone())
}

/// Returns the first available canonical id in stable catalog order.
fn first_available_id(entries: &[ModelCatalogEntry]) -> Option<String> {
    entries
        .iter()
        .find(|entry| entry.availability == ModelAvailability::Available)
        .map(|entry| entry.id.clone())
}

#[cfg(test)]
mod tests {
    use super::{
        ModelAvailability, ModelCatalog, ModelCatalogCandidate, ModelCatalogInput,
        ModelCatalogSelectionErrorKind, ModelCatalogSource,
    };
    use crate::ProviderModelInfo;

    /// Builds one model candidate with explicit policy-relevant metadata.
    fn candidate(
        source: ModelCatalogSource,
        id: &str,
        display_name: Option<&str>,
        reasoning_levels: &[&str],
        context_window_tokens: Option<usize>,
    ) -> ModelCatalogCandidate {
        ModelCatalogCandidate::available(
            source,
            ProviderModelInfo {
                id: id.to_string(),
                display_name: display_name.map(str::to_string),
                reasoning_levels: reasoning_levels
                    .iter()
                    .map(|level| (*level).to_string())
                    .collect(),
                context_window_tokens,
                max_input_tokens: None,
                max_output_tokens: None,
                capabilities: Vec::new(),
            },
        )
    }

    /// Verifies duplicate models merge by explicit source precedence while
    /// preserving useful lower-precedence metadata and stable id ordering.
    ///
    /// Configured values must lead discovered/default values without erasing a
    /// discovered display name or context limit that configuration omitted.
    #[test]
    fn model_catalog_merges_duplicate_sources_with_configured_precedence() {
        let catalog = ModelCatalog::from_input(ModelCatalogInput {
            candidates: vec![
                candidate(ModelCatalogSource::Default, "z-model", None, &["low"], None),
                candidate(
                    ModelCatalogSource::Discovered,
                    "a-model",
                    Some("Provider A"),
                    &["medium"],
                    Some(200_000),
                ),
                candidate(
                    ModelCatalogSource::Configured,
                    "a-model",
                    None,
                    &["high", "medium"],
                    None,
                ),
            ],
            ..ModelCatalogInput::default()
        });

        assert_eq!(
            catalog
                .entries()
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-model", "z-model"]
        );
        let merged = &catalog.entries()[0];
        assert_eq!(merged.source, ModelCatalogSource::Configured);
        assert_eq!(merged.display_name.as_deref(), Some("Provider A"));
        assert_eq!(merged.context_window_tokens, Some(200_000));
        assert_eq!(merged.reasoning_levels, vec!["high", "medium"]);
    }

    /// Verifies empty identifiers and metadata values are removed while
    /// unknown context limits remain absent rather than receiving a fake size.
    ///
    /// Product adapters may receive partial or malformed provider records; the
    /// pure catalog boundary must normalize safe values deterministically.
    #[test]
    fn model_catalog_normalizes_empty_values_and_unknown_context_windows() {
        let mut valid = candidate(
            ModelCatalogSource::Discovered,
            " model-a ",
            Some(" "),
            &[" high ", "", "high"],
            Some(0),
        );
        valid.model.capabilities = vec![" tool_use ".to_string(), "tool_use".to_string()];
        valid.aliases = vec![" short ".to_string(), "".to_string()];
        let catalog = ModelCatalog::from_input(ModelCatalogInput {
            candidates: vec![
                candidate(ModelCatalogSource::Configured, " ", None, &[], None),
                valid,
            ],
            ..ModelCatalogInput::default()
        });

        assert_eq!(catalog.entries().len(), 1);
        let entry = &catalog.entries()[0];
        assert_eq!(entry.id, "model-a");
        assert_eq!(entry.display_name, None);
        assert_eq!(entry.reasoning_levels, vec!["high"]);
        assert_eq!(entry.capabilities, vec!["tool_use"]);
        assert_eq!(entry.aliases, vec!["short"]);
        assert_eq!(entry.context_window_tokens, None);
    }

    /// Verifies configured defaults outrank recommendations and both fall back
    /// to the first available stable entry when their targets are unavailable.
    ///
    /// Preferred selection consumes explicit observations only and never
    /// invents a model for an empty catalog.
    #[test]
    fn model_catalog_selects_default_recommended_and_available_fallbacks() {
        let mut unavailable = candidate(
            ModelCatalogSource::Configured,
            "configured",
            None,
            &[],
            None,
        );
        unavailable.availability = ModelAvailability::Unavailable;
        let catalog = ModelCatalog::from_input(ModelCatalogInput {
            candidates: vec![
                candidate(
                    ModelCatalogSource::Recommended,
                    "recommended",
                    None,
                    &[],
                    None,
                ),
                unavailable,
            ],
            default_model: Some("configured".to_string()),
            recommended_model: Some("recommended".to_string()),
            reasoning_levels: Vec::new(),
        });
        assert_eq!(catalog.preferred_model(), Some("recommended"));

        let empty = ModelCatalog::from_input(ModelCatalogInput::default());
        assert_eq!(empty.preferred_model(), None);
    }

    /// Verifies aliases resolve to canonical ids and exact canonical ids win
    /// over an alias collision from another entry.
    ///
    /// Alias matching is deterministic and does not mutate the provider model
    /// identifier stored in a validated selection.
    #[test]
    fn model_catalog_resolves_aliases_to_canonical_models() {
        let mut aliased = candidate(ModelCatalogSource::Configured, "model-a", None, &[], None);
        aliased.aliases = vec!["fast".to_string(), "model-b".to_string()];
        let catalog = ModelCatalog::from_input(ModelCatalogInput {
            candidates: vec![
                aliased,
                candidate(ModelCatalogSource::Discovered, "model-b", None, &[], None),
            ],
            ..ModelCatalogInput::default()
        });

        assert_eq!(catalog.select("fast", None).unwrap().model.id, "model-a");
        assert_eq!(catalog.select("model-b", None).unwrap().model.id, "model-b");
    }

    /// Verifies selection rejects empty, unknown, unavailable, and unsupported
    /// reasoning requests with stable typed categories.
    ///
    /// Product adapters can map these categories to their own error surface
    /// without parsing lower-crate diagnostic text.
    #[test]
    fn model_catalog_selection_returns_typed_failures() {
        let mut unavailable = candidate(ModelCatalogSource::Discovered, "offline", None, &[], None);
        unavailable.availability = ModelAvailability::Unavailable;
        let catalog = ModelCatalog::from_input(ModelCatalogInput {
            candidates: vec![
                candidate(
                    ModelCatalogSource::Discovered,
                    "ready",
                    None,
                    &["low", "high"],
                    None,
                ),
                unavailable,
            ],
            ..ModelCatalogInput::default()
        });

        assert_eq!(
            catalog
                .available_entries()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ready"]
        );

        assert_eq!(
            catalog.select("", None).unwrap_err().kind(),
            ModelCatalogSelectionErrorKind::EmptyModel
        );
        assert_eq!(
            catalog.select("missing", None).unwrap_err().kind(),
            ModelCatalogSelectionErrorKind::UnknownModel
        );
        assert_eq!(
            catalog.select("offline", None).unwrap_err().kind(),
            ModelCatalogSelectionErrorKind::UnavailableModel
        );
        assert_eq!(
            catalog.select("ready", Some("")).unwrap_err().kind(),
            ModelCatalogSelectionErrorKind::EmptyReasoning
        );
        assert_eq!(
            catalog.select("ready", Some("max")).unwrap_err().kind(),
            ModelCatalogSelectionErrorKind::UnknownReasoning
        );
    }

    /// Verifies model-specific reasoning levels override the catalog-wide
    /// fallback while empty model metadata inherits the provider-wide list.
    ///
    /// This single lookup contract prevents picker and generated-profile paths
    /// from implementing different fallback behavior.
    #[test]
    fn model_catalog_reasoning_lookup_uses_model_then_catalog_fallback() {
        let catalog = ModelCatalog::from_input(ModelCatalogInput {
            candidates: vec![
                candidate(
                    ModelCatalogSource::Discovered,
                    "specific",
                    None,
                    &["high"],
                    None,
                ),
                candidate(ModelCatalogSource::Discovered, "fallback", None, &[], None),
            ],
            reasoning_levels: vec!["low".to_string(), "medium".to_string()],
            ..ModelCatalogInput::default()
        });

        assert_eq!(
            catalog.reasoning_levels_for("specific").unwrap(),
            &["high".to_string()]
        );
        assert_eq!(
            catalog.reasoning_levels_for("fallback").unwrap(),
            &["high".to_string(), "low".to_string(), "medium".to_string()]
        );
    }

    /// Verifies configured metadata replaces lower-precedence list values,
    /// overrides individual option keys, and inherits omitted scalar fields.
    ///
    /// Explicitly empty configured lists are meaningful replacements rather
    /// than missing observations, while catalog values continue to fill gaps.
    #[test]
    fn model_catalog_resolves_field_precedence_and_explicit_list_replacement() {
        let mut discovered = candidate(
            ModelCatalogSource::Discovered,
            "model-a",
            Some("Discovered label"),
            &["low", "high"],
            Some(200_000),
        );
        discovered.model.max_input_tokens = Some(180_000);
        discovered.model.max_output_tokens = Some(8_000);
        discovered.model.capabilities = vec!["vision".to_string(), "tools".to_string()];
        discovered.provider_options = std::collections::BTreeMap::from([
            ("shared".to_string(), "catalog".to_string()),
            ("catalog-only".to_string(), "retained".to_string()),
        ]);

        let mut configured = candidate(ModelCatalogSource::Configured, "model-a", None, &[], None);
        configured.reasoning_levels = Some(Vec::new());
        configured.capabilities = Some(vec!["tools".to_string()]);
        configured.model.max_output_tokens = Some(16_000);
        configured.provider_options = std::collections::BTreeMap::from([
            ("shared".to_string(), "configured".to_string()),
            ("configured-only".to_string(), "present".to_string()),
        ]);

        let mut profile = candidate(ModelCatalogSource::Profile, "model-a", None, &[], None);
        profile.model.max_input_tokens = Some(170_000);
        profile.provider_options =
            std::collections::BTreeMap::from([("shared".to_string(), "profile".to_string())]);

        let catalog = ModelCatalog::from_input(ModelCatalogInput {
            candidates: vec![profile, configured, discovered],
            ..ModelCatalogInput::default()
        });
        let entry = &catalog.entries()[0];

        assert_eq!(entry.display_name.as_deref(), Some("Discovered label"));
        assert_eq!(entry.context_window_tokens, Some(200_000));
        assert_eq!(entry.max_input_tokens, Some(170_000));
        assert_eq!(entry.max_output_tokens, Some(16_000));
        assert!(entry.reasoning_levels.is_empty());
        assert_eq!(entry.capabilities, ["tools"]);
        assert_eq!(entry.provider_options["shared"], "profile");
        assert_eq!(entry.provider_options["catalog-only"], "retained");
        assert_eq!(entry.provider_options["configured-only"], "present");
    }

    /// Verifies a configured provider-model record projects its omitted and
    /// explicitly empty fields into a catalog candidate without losing either.
    ///
    /// This conversion is the bridge used by later schema and profile work to
    /// feed configured base metadata into the provider-neutral resolver.
    #[test]
    fn configured_provider_model_preserves_optional_catalog_metadata() {
        let configured = crate::ProviderModelConfig {
            id: "model-a".to_string(),
            aliases: vec!["alias-a".to_string()],
            reasoning_levels: Some(Vec::new()),
            capabilities: None,
            max_output_tokens: Some(32_000),
            provider_options: std::collections::BTreeMap::from([(
                "service_tier".to_string(),
                "priority".to_string(),
            )]),
            ..crate::ProviderModelConfig::default()
        };

        let candidate = ModelCatalogCandidate::configured(&configured);
        assert_eq!(candidate.reasoning_levels, Some(Vec::new()));
        assert_eq!(candidate.capabilities, None);
        assert_eq!(candidate.model.max_output_tokens, Some(32_000));
        assert_eq!(candidate.aliases, ["alias-a"]);
        assert_eq!(candidate.provider_options["service_tier"], "priority");
    }
}
