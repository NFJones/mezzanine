//! Immutable provider-message storage and typed request-epoch identity.
//!
//! Copy-on-write request edits preserve provenance and invalidate epoch metadata.
//! These records are projections, never a competing durable chronology store.

use super::{ContextPlacement, ContextSourceKind};

/// Provider-independent role of one model message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelMessageRole {
    /// System-level instructions.
    System,
    /// Developer-level instructions.
    Developer,
    /// User-authored input.
    User,
    /// Prior assistant output.
    Assistant,
    /// Tool or action evidence.
    Tool,
    /// Neutral controller, reference, or live context that is not user speech.
    Context,
}

/// Provider-independent message supplied to model request rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMessage {
    /// Provider-facing role of the message.
    pub role: ModelMessageRole,
    /// Provenance and stability class of the message.
    pub source: ContextSourceKind,
    /// Explicit cache and ordering lifecycle carried from the context producer.
    pub placement: ContextPlacement,
    /// Model-visible message content.
    pub content: String,
}

/// Clone-efficient ordered model messages for one provider request.
///
/// Request clones share both the collection and each immutable message. A
/// continuation that inserts, retains, or appends messages copies only `Arc`
/// pointers, so unchanged transcript content is not duplicated between rounds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelMessages {
    items: std::sync::Arc<Vec<std::sync::Arc<ModelMessage>>>,
    provider_request_epoch: Option<ProviderRequestEpoch>,
}

/// One typed cache-affecting component that begins a new model-context epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextEpochComponent {
    /// Provider routing namespace changed.
    ProviderNamespace,
    /// Provider implementation changed.
    Provider,
    /// Selected provider model changed.
    Model,
    /// Static prompt-profile or instruction bytes changed.
    StaticInstructions,
    /// The static MAAP schema version changed.
    MaapSchema,
    /// Provider response-format contract changed.
    ResponseFormat,
    /// Provider tool schema changed.
    ToolSchema,
    /// Provider tool-choice control changed.
    ToolChoice,
    /// Provider request controls changed.
    RequestControls,
    /// API or streaming shape changed.
    ApiShape,
    /// Prompt-cache lineage changed.
    CacheLineage,
    /// Compaction rewrote chronology.
    CompactionGeneration,
}

/// Typed identity for one immutable provider context epoch.
///
/// This non-model-visible value records every cache-affecting dimension that
/// must remain fixed while provider input grows by durable chronology alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEpochIdentity {
    pub provider_namespace: String,
    pub provider: String,
    pub model: String,
    pub static_instructions_sha256: String,
    pub maap_schema_version: String,
    pub response_format_sha256: String,
    pub tool_schema_sha256: String,
    pub tool_choice_sha256: String,
    pub request_controls_sha256: String,
    pub api_shape: String,
    pub cache_lineage: Option<String>,
    pub compaction_generation_sha256: String,
}

impl ContextEpochIdentity {
    /// Returns the first deterministically ordered component changed by a new
    /// epoch identity, if any.
    pub fn changed_component(&self, current: &Self) -> Option<ContextEpochComponent> {
        [
            (
                ContextEpochComponent::ProviderNamespace,
                self.provider_namespace != current.provider_namespace,
            ),
            (
                ContextEpochComponent::Provider,
                self.provider != current.provider,
            ),
            (ContextEpochComponent::Model, self.model != current.model),
            (
                ContextEpochComponent::StaticInstructions,
                self.static_instructions_sha256 != current.static_instructions_sha256,
            ),
            (
                ContextEpochComponent::MaapSchema,
                self.maap_schema_version != current.maap_schema_version,
            ),
            (
                ContextEpochComponent::ResponseFormat,
                self.response_format_sha256 != current.response_format_sha256,
            ),
            (
                ContextEpochComponent::ToolSchema,
                self.tool_schema_sha256 != current.tool_schema_sha256,
            ),
            (
                ContextEpochComponent::ToolChoice,
                self.tool_choice_sha256 != current.tool_choice_sha256,
            ),
            (
                ContextEpochComponent::RequestControls,
                self.request_controls_sha256 != current.request_controls_sha256,
            ),
            (
                ContextEpochComponent::ApiShape,
                self.api_shape != current.api_shape,
            ),
            (
                ContextEpochComponent::CacheLineage,
                self.cache_lineage != current.cache_lineage,
            ),
            (
                ContextEpochComponent::CompactionGeneration,
                self.compaction_generation_sha256 != current.compaction_generation_sha256,
            ),
        ]
        .into_iter()
        .find_map(|(component, changed)| changed.then_some(component))
    }
}

/// Origin of the epoch that owns one exact provider input chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextEpochTransition {
    /// The request established the first chain for this context.
    Initial,
    /// One typed cache-affecting component started a new epoch.
    Changed(ContextEpochComponent),
    /// A cache comparison failed; the valid current request starts a new baseline.
    Warning(&'static str),
}

/// Typed epoch metadata retained across one append-only provider request chain.
///
/// This state deliberately contains no model-visible provider input. Adapters
/// re-render canonical input from durable chronology before every send and use
/// the epoch only to classify an intentional envelope transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRequestEpoch {
    pub(crate) context_epoch: ContextEpochIdentity,
    pub(crate) epoch_transition: ContextEpochTransition,
}

impl ModelMessages {
    /// Returns the number of ordered messages.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Reports whether the request contains no messages.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns the final ordered message, when present.
    pub fn last(&self) -> Option<&ModelMessage> {
        self.items.last().map(std::sync::Arc::as_ref)
    }

    /// Iterates immutable messages without exposing their shared ownership.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &ModelMessage> + ExactSizeIterator {
        self.items.iter().map(std::sync::Arc::as_ref)
    }

    /// Appends one request-local message using copy-on-write pointer storage.
    pub fn push(&mut self, message: ModelMessage) {
        self.provider_request_epoch = None;
        std::sync::Arc::make_mut(&mut self.items).push(std::sync::Arc::new(message));
    }

    /// Inserts one request-local message at the requested chronological index.
    pub fn insert(&mut self, index: usize, message: ModelMessage) {
        self.provider_request_epoch = None;
        std::sync::Arc::make_mut(&mut self.items).insert(index, std::sync::Arc::new(message));
    }

    /// Retains only messages accepted by `predicate`.
    pub fn retain(&mut self, mut predicate: impl FnMut(&ModelMessage) -> bool) {
        self.provider_request_epoch = None;
        std::sync::Arc::make_mut(&mut self.items).retain(|message| predicate(message.as_ref()));
    }

    /// Returns the typed provider epoch selected for this prepared request.
    pub(crate) fn provider_request_epoch(&self) -> Option<&ProviderRequestEpoch> {
        self.provider_request_epoch.as_ref()
    }

    /// Returns a content-free advisory cache warning for this request only.
    pub fn provider_continuity_warning(&self) -> Option<&'static str> {
        match self.provider_request_epoch.as_ref()?.epoch_transition {
            ContextEpochTransition::Warning(code) => Some(code),
            _ => None,
        }
    }

    /// Installs provider epoch metadata without retaining provider-visible input.
    pub(crate) fn set_provider_request_epoch(&mut self, epoch: ProviderRequestEpoch) {
        self.provider_request_epoch = Some(epoch);
    }

    /// Reports whether two collections share the same immutable backing store.
    #[cfg(test)]
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.items, &other.items)
    }
}

impl From<Vec<ModelMessage>> for ModelMessages {
    fn from(messages: Vec<ModelMessage>) -> Self {
        Self {
            items: std::sync::Arc::new(messages.into_iter().map(std::sync::Arc::new).collect()),
            provider_request_epoch: None,
        }
    }
}

impl std::ops::Index<usize> for ModelMessages {
    type Output = ModelMessage;

    fn index(&self, index: usize) -> &Self::Output {
        self.items[index].as_ref()
    }
}

impl<'a> IntoIterator for &'a ModelMessages {
    type Item = &'a ModelMessage;
    type IntoIter = ModelMessagesIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        ModelMessagesIter(self.items.iter())
    }
}

/// Borrowed iterator over shared model messages.
pub struct ModelMessagesIter<'a>(std::slice::Iter<'a, std::sync::Arc<ModelMessage>>);

impl<'a> Iterator for ModelMessagesIter<'a> {
    type Item = &'a ModelMessage;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(std::sync::Arc::as_ref)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl DoubleEndedIterator for ModelMessagesIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(std::sync::Arc::as_ref)
    }
}

impl ExactSizeIterator for ModelMessagesIter<'_> {}

impl ModelMessage {
    /// Returns the provider-neutral cache lifecycle disposition for this message.
    pub fn cache_disposition(&self) -> ContextPlacement {
        self.placement
    }
}
