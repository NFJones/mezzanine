//! Render request and flush value types exchanged with the async actor.

use super::{
    AttachedTerminalOutputModes, ClientId, RenderedClientView, TerminalClientLoopConfig,
    TerminalStyleSpan,
};
use std::ops::Deref;
use std::sync::Arc;

// Async runtime actor request and report types.

/// Immutable actor-resolved terminal configuration shared across client requests.
///
/// The generation changes whenever actor-owned terminal interaction or
/// presentation state is invalidated. Cloning this value only clones the
/// `Arc`, allowing unchanged terminal batches to avoid copying frame context,
/// bindings, hit regions, and presentation settings.
#[derive(Debug, Clone)]
pub struct AsyncTerminalClientConfigSnapshot {
    generation: u64,
    config: Arc<TerminalClientLoopConfig>,
}

impl AsyncTerminalClientConfigSnapshot {
    /// Builds one resolved snapshot for the supplied actor generation.
    pub(in crate::host::async_runtime) fn new(
        generation: u64,
        config: TerminalClientLoopConfig,
    ) -> Self {
        Self {
            generation,
            config: Arc::new(config),
        }
    }

    /// Returns the actor generation represented by this snapshot.
    pub(in crate::host::async_runtime) fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the immutable resolved configuration.
    pub fn config(&self) -> &TerminalClientLoopConfig {
        self.config.as_ref()
    }
}

impl Deref for AsyncTerminalClientConfigSnapshot {
    type Target = TerminalClientLoopConfig;

    /// Borrows the immutable resolved configuration carried by this snapshot.
    fn deref(&self) -> &Self::Target {
        self.config()
    }
}

/// Raw or previously resolved terminal configuration accepted by the actor.
pub(in crate::host::async_runtime) enum AsyncTerminalClientConfigInput {
    /// Configuration that must be resolved against current actor state.
    Raw(Box<TerminalClientLoopConfig>),
    /// Configuration that may be reused when its generation is still current.
    Snapshot(AsyncTerminalClientConfigSnapshot),
}

/// Carries Async Rendered Client Frame state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct AsyncRenderedClientFrame {
    /// Actor-resolved configuration used to compose this frame.
    pub config: AsyncTerminalClientConfigSnapshot,
    /// Stores the view value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub view: Option<RenderedClientView>,
}

/// Carries Async Rendered Client Flush state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncRenderedClientFlush {
    /// Stores the client id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client_id: ClientId,
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub lines: Vec<String>,
    /// Stores the line style spans value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Stores the modes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub modes: AttachedTerminalOutputModes,
}
