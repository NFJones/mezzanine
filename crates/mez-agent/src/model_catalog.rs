//! Provider-neutral model catalog construction and selection policy.
//!
//! This module merges already-resolved configured, discovered, default, and
//! recommended model metadata without knowing about product configuration,
//! provider transports, credentials, caches, or UI rendering. Product adapters
//! translate their observations into candidates and apply typed selections.

use std::collections::BTreeMap;
use std::fmt;

use crate::ProviderModelInfo;

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
}

impl ModelCatalogSource {
    /// Returns the stable source name used in diagnostics and tests.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recommended => "recommended",
            Self::Default => "default",
            Self::Discovered => "discovered",
            Self::Configured => "configured",
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
    /// Whether the observed model may be selected.
    pub availability: ModelAvailability,
}

impl ModelCatalogCandidate {
    /// Creates one available candidate without aliases.
    pub fn available(source: ModelCatalogSource, model: ProviderModelInfo) -> Self {
        Self {
            model,
            source,
            aliases: Vec::new(),
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
    /// Ordered provider-neutral capability tags.
    pub capabilities: Vec<String>,
    /// Ordered alternate identifiers for selection.
    pub aliases: Vec<String>,
    /// Highest-precedence source that supplied this entry.
    pub source: ModelCatalogSource,
    /// Whether this entry may be selected.
    pub availability: ModelAvailability,
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
                capabilities: self.capabilities.clone(),
            },
            source: self.source,
            aliases: self.aliases.clone(),
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
        let mut entries = BTreeMap::<String, ModelCatalogEntry>::new();
        for candidate in input.candidates {
            let Some(incoming) = normalized_candidate(candidate) else {
                continue;
            };
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
    let id = candidate.model.id.trim();
    if id.is_empty() {
        return None;
    }
    let aliases = normalize_model_catalog_values(candidate.aliases)
        .into_iter()
        .filter(|alias| alias != id)
        .collect();
    Some(ModelCatalogEntry {
        id: id.to_string(),
        display_name: candidate
            .model
            .display_name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty()),
        reasoning_levels: normalize_model_catalog_values(candidate.model.reasoning_levels),
        context_window_tokens: candidate
            .model
            .context_window_tokens
            .filter(|limit| *limit > 0),
        capabilities: normalize_model_catalog_values(candidate.model.capabilities),
        aliases,
        source: candidate.source,
        availability: candidate.availability,
    })
}

/// Merges one duplicate entry according to source precedence.
fn merge_catalog_entry(existing: &mut ModelCatalogEntry, incoming: ModelCatalogEntry) {
    if incoming.source > existing.source {
        existing.display_name = incoming
            .display_name
            .or_else(|| existing.display_name.take());
        existing.context_window_tokens = incoming
            .context_window_tokens
            .or(existing.context_window_tokens);
        existing.reasoning_levels = normalize_model_catalog_values(
            incoming
                .reasoning_levels
                .into_iter()
                .chain(std::mem::take(&mut existing.reasoning_levels))
                .collect(),
        );
        existing.capabilities = normalize_model_catalog_values(
            incoming
                .capabilities
                .into_iter()
                .chain(std::mem::take(&mut existing.capabilities))
                .collect(),
        );
        existing.aliases = normalize_model_catalog_values(
            incoming
                .aliases
                .into_iter()
                .chain(std::mem::take(&mut existing.aliases))
                .collect(),
        );
        existing.source = incoming.source;
        existing.availability = incoming.availability;
    } else {
        if existing.display_name.is_none() {
            existing.display_name = incoming.display_name;
        }
        if existing.context_window_tokens.is_none() {
            existing.context_window_tokens = incoming.context_window_tokens;
        }
        existing.reasoning_levels = normalize_model_catalog_values(
            std::mem::take(&mut existing.reasoning_levels)
                .into_iter()
                .chain(incoming.reasoning_levels)
                .collect(),
        );
        existing.capabilities = normalize_model_catalog_values(
            std::mem::take(&mut existing.capabilities)
                .into_iter()
                .chain(incoming.capabilities)
                .collect(),
        );
        existing.aliases = normalize_model_catalog_values(
            std::mem::take(&mut existing.aliases)
                .into_iter()
                .chain(incoming.aliases)
                .collect(),
        );
    }
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
}
