//! Provider-facing context framing and accounting-only projection.
//!
//! Projection preserves producer provenance and the untrusted-data annotation;
//! estimates never enter model-visible text or mutate canonical chronology.

use super::{AgentContext, AgentContextResult, ContextBlock, ModelRequest};

/// One provider-bound view composed from durable stable and chronological
/// context plus non-model-visible previous-request metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedModelContext {
    durable: AgentContext,
    previous_request: Option<ModelRequest>,
}

impl PreparedModelContext {
    /// Builds and validates one prepared provider context.
    pub fn new(durable: AgentContext) -> AgentContextResult<Self> {
        durable.validate_durable()?;
        Ok(Self {
            durable,
            previous_request: None,
        })
    }

    /// Builds a prepared context with no request-local live state.
    pub fn from_durable(durable: AgentContext) -> AgentContextResult<Self> {
        Self::new(durable)
    }

    /// Carries the last concrete request emitted for this turn into the next
    /// provider worker without making it model-visible or transcript-durable.
    pub fn with_previous_request(mut self, previous_request: Option<ModelRequest>) -> Self {
        self.previous_request = previous_request;
        self
    }

    /// Returns the immutable stored portion of the request context.
    pub fn durable(&self) -> &AgentContext {
        &self.durable
    }

    /// Returns the last concrete request emitted for this turn, when present.
    pub fn previous_request(&self) -> Option<&ModelRequest> {
        self.previous_request.as_ref()
    }

    /// Returns the number of blocks visible to the provider.
    pub fn len(&self) -> usize {
        self.durable.blocks().len()
    }

    /// Reports whether the prepared request has no model-visible blocks.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clones the canonical provider-visible sequence without changing order.
    pub fn to_agent_context(&self) -> AgentContext {
        self.durable.clone()
    }

    /// Consumes the prepared view and returns its durable context.
    pub fn into_agent_context(self) -> AgentContext {
        self.durable
    }
}

/// Builds the bracketed provider-message header for one context block.
pub fn model_context_block_header(block: &ContextBlock) -> String {
    let trust = block.trust_domain();
    let domain_annotation = if trust.is_untrusted_by_default() {
        format!(" [untrusted:{}]", trust.as_str())
    } else {
        String::new()
    };
    format!("[{}{}]\n", block.label, domain_annotation)
}

/// Estimates the framed token cost of one context block for accounting only.
///
/// This is not provider-reported usage: providers report whole-request totals,
/// and wire-level role and schema overhead must be accounted for separately.
pub(super) fn context_block_input_token_estimate(block: &ContextBlock) -> usize {
    crate::provider_text_input_token_estimate(&format!(
        "{}{}",
        model_context_block_header(block),
        block.content
    ))
}
