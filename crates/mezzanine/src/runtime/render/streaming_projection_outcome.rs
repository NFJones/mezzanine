//! Content-free projection acceptance and rejection evidence.
//!
//! The first failed fence is retained in deterministic validation order. Reasons
//! contain no source text or owner IDs and cannot authorize delivery or replay.
//! An accepted identical screen remains distinct from rejection and redraw.

/// The first failed ownership, geometry or rendering-policy fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeStreamingSayProjectionRejection {
    /// No source owner remains for the captured pane.
    MissingOwner,
    /// Ordinary publication already retired the provisional layout.
    RetiredLayout,
    /// A different turn owns the source.
    Turn,
    /// A different provider response owns the source.
    Response,
    /// The source belongs to a different conversation.
    SourceConversation,
    /// Newer source was received after capture.
    SourceRevision,
    /// The installed screen is no longer the captured generation.
    ScreenLineage,
    /// The pane's active conversation no longer matches.
    ActiveConversation,
    /// The pane screen is unavailable.
    MissingScreen,
    /// Terminal dimensions changed after capture.
    ScreenSize,
    /// Thinking visibility changed after capture.
    Thinking,
    /// The command-highlighting dialect changed after capture.
    ShellClassification,
    /// The presentation column budget changed after capture.
    PresentationColumns,
    /// The framed body width changed after capture.
    FrameWidth,
    /// The table width changed after capture.
    TableWidth,
    /// The rendering theme changed after capture.
    Theme,
    /// The candidate baseline carries different log-frame settings.
    LogFrameSettings,
}

impl RuntimeStreamingSayProjectionRejection {
    /// Returns a fixed, bounded diagnostic code, never captured input values.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::MissingOwner => "missing_owner",
            Self::RetiredLayout => "retired_layout",
            Self::Turn => "turn",
            Self::Response => "response",
            Self::SourceConversation => "source_conversation",
            Self::SourceRevision => "source_revision",
            Self::ScreenLineage => "screen_lineage",
            Self::ActiveConversation => "active_conversation",
            Self::MissingScreen => "missing_screen",
            Self::ScreenSize => "screen_size",
            Self::Thinking => "thinking",
            Self::ShellClassification => "shell_classification",
            Self::PresentationColumns => "presentation_columns",
            Self::FrameWidth => "frame_width",
            Self::TableWidth => "table_width",
            Self::Theme => "theme",
            Self::LogFrameSettings => "log_frame_settings",
        }
    }
}

/// Acceptance of one fenced projection, separate from physical redraw intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeStreamingSayProjectionApplyOutcome {
    /// A failed fence committed neither metadata nor screen state.
    Rejected(RuntimeStreamingSayProjectionRejection),
    /// All fences passed and metadata advanced without replacing the screen.
    AcceptedUnchanged,
    /// All fences passed and the new screen was installed atomically.
    AcceptedChanged,
}

impl RuntimeStreamingSayProjectionApplyOutcome {
    /// Includes metadata-only reconciliation of an unchanged screen.
    pub(crate) fn is_accepted(self) -> bool {
        !matches!(self, Self::Rejected(_))
    }

    /// Reports whether the accepted result replaced the screen and needs redraw.
    pub(crate) fn screen_changed(self) -> bool {
        matches!(self, Self::AcceptedChanged)
    }

    /// Retains a rejected fence in display-only diagnostics without input text.
    pub(crate) fn require_accepted(self, boundary: &str) -> crate::Result<()> {
        match self {
            Self::Rejected(reason) => Err(crate::MezError::invalid_state(format!(
                "{boundary}: reason={}; outbound_projection_sha256={}",
                reason.code(),
                env!("MEZ_OUTBOUND_PROJECTION_SHA256"),
            ))),
            Self::AcceptedUnchanged | Self::AcceptedChanged => {
                debug_assert!(self.is_accepted());
                Ok(())
            }
        }
    }
}
