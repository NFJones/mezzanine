//! Protocol frame and renderer data types.
//!
//! These types are shared by wire codecs and visible frame rendering while the
//! implementation details remain in focused sibling modules.

use std::collections::BTreeMap;

use crate::error::{MezError, Result};

/// Decoded content-length protocol frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolFrame {
    /// MIME-style content type advertised in the frame header.
    pub content_type: String,
    /// UTF-8 body carried by the frame.
    pub body: String,
}

/// Tokio codec for bounded content-length protocol frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolFrameCodec {
    /// Stores the max content length value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_content_length: usize,
}

/// Overflow policy for visible frame template rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOverflow {
    /// Cut text at the requested width.
    Truncate,
    /// Replace the last visible characters with an ellipsis.
    Elide,
    /// Insert line breaks at the requested width.
    Wrap,
}

/// Named values available to visible frame templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameContext {
    /// Stores the fields value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    fields: BTreeMap<String, String>,
}

impl FrameContext {
    /// Creates an empty render context.
    pub fn new() -> Self {
        Self {
            fields: BTreeMap::new(),
        }
    }

    /// Adds a field to the render context and returns the updated context.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }

    /// Runs the field operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn field(&self, key: &str) -> &str {
        self.fields.get(key).map(String::as_str).unwrap_or("")
    }
}

impl Default for FrameContext {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolFrame {
    /// Creates a frame from a content type and UTF-8 body.
    pub fn new(content_type: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            content_type: content_type.into(),
            body: body.into(),
        }
    }
}

impl ProtocolFrameCodec {
    /// Creates a frame codec with a maximum allowed body length.
    ///
    /// Returns an invalid-arguments error when the limit is zero.
    pub fn new(max_content_length: usize) -> Result<Self> {
        if max_content_length == 0 {
            return Err(MezError::invalid_args(
                "protocol frame codec max content length must be greater than zero",
            ));
        }
        Ok(Self { max_content_length })
    }

    /// Returns the maximum configured body length for this codec.
    pub fn max_content_length(self) -> usize {
        self.max_content_length
    }
}
