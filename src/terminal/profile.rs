//! Terminal Profile implementation.
//!
//! This module owns the terminal profile boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{DEFAULT_TERMINAL_PROFILE_NAME, MezError, Result};

// Terminal compatibility profiles and terminfo selection.

/// A named terminal compatibility profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalProfile {
    /// Represents the Xterm Compatible case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    XtermCompatible,
    /// Represents the Dumb case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Dumb,
}

impl TerminalProfile {
    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn name(self) -> &'static str {
        match self {
            Self::XtermCompatible => DEFAULT_TERMINAL_PROFILE_NAME,
            Self::Dumb => "dumb",
        }
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn capabilities(self) -> TerminalCapabilities {
        match self {
            Self::XtermCompatible => TerminalCapabilities::xterm_compatible(),
            Self::Dumb => TerminalCapabilities::dumb(),
        }
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn compatibility_profile(self) -> TerminalCompatibilityProfile {
        TerminalCompatibilityProfile {
            profile: self,
            name: self.name(),
            capabilities: self.capabilities(),
        }
    }
}

/// Public profile metadata used by diagnostics, configuration, and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCompatibilityProfile {
    /// Stores the profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub profile: TerminalProfile,
    /// Stores the name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: &'static str,
    /// Stores the capabilities value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub capabilities: TerminalCapabilities,
}

/// Support level for a terminal capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CapabilitySupport {
    /// Represents the Unsupported case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    #[default]
    Unsupported,
    /// Represents the Supported case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Supported,
    /// Represents the Host Dependent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HostDependent,
    /// Represents the Policy Gated case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PolicyGated,
}

impl CapabilitySupport {
    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn is_available(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

/// SGR color and text-attribute support advertised by a profile or TERM entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SgrCapabilities {
    /// Stores the attributes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub attributes: CapabilitySupport,
    /// Stores the basic colors value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub basic_colors: CapabilitySupport,
    /// Stores the bright colors value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bright_colors: CapabilitySupport,
    /// Stores the indexed 256 colors value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub indexed_256_colors: CapabilitySupport,
    /// Stores the true color value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub true_color: CapabilitySupport,
}

/// DEC private mode support carried by the active terminal profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecPrivateModeCapabilities {
    /// Stores the modes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub modes: CapabilitySupport,
    /// Stores the alternate screen value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub alternate_screen: CapabilitySupport,
    /// Stores the application cursor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub application_cursor: CapabilitySupport,
    /// Stores the application keypad value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub application_keypad: CapabilitySupport,
    /// Stores the bracketed paste value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bracketed_paste: CapabilitySupport,
    /// Stores the focus events value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub focus_events: CapabilitySupport,
    /// Stores the sgr mouse value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sgr_mouse: CapabilitySupport,
}

/// Save and restore behavior for cursor and DEC private modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SaveRestoreCapabilities {
    /// Stores the cursor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cursor: CapabilitySupport,
    /// Stores the modes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub modes: CapabilitySupport,
}

/// Terminal behavior that can be safely exposed to pane applications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalCapabilities {
    /// Stores the line oriented output value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub line_oriented_output: CapabilitySupport,
    /// Stores the c0 controls value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub c0_controls: CapabilitySupport,
    /// Stores the esc sequences value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub esc_sequences: CapabilitySupport,
    /// Stores the csi sequences value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub csi_sequences: CapabilitySupport,
    /// Stores the osc string controls value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub osc_string_controls: CapabilitySupport,
    /// Stores the dcs string controls value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub dcs_string_controls: CapabilitySupport,
    /// Stores the sgr value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sgr: SgrCapabilities,
    /// Stores the dec private modes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub dec_private_modes: DecPrivateModeCapabilities,
    /// Stores the title setting value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub title_setting: CapabilitySupport,
    /// Stores the clipboard value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub clipboard: CapabilitySupport,
    /// Stores the save restore value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub save_restore: SaveRestoreCapabilities,
}

impl TerminalCapabilities {
    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn xterm_compatible() -> Self {
        Self {
            line_oriented_output: CapabilitySupport::Supported,
            c0_controls: CapabilitySupport::Supported,
            esc_sequences: CapabilitySupport::Supported,
            csi_sequences: CapabilitySupport::Supported,
            osc_string_controls: CapabilitySupport::Supported,
            dcs_string_controls: CapabilitySupport::Supported,
            sgr: SgrCapabilities {
                attributes: CapabilitySupport::Supported,
                basic_colors: CapabilitySupport::Supported,
                bright_colors: CapabilitySupport::Supported,
                indexed_256_colors: CapabilitySupport::Supported,
                true_color: CapabilitySupport::Supported,
            },
            dec_private_modes: DecPrivateModeCapabilities {
                modes: CapabilitySupport::Supported,
                alternate_screen: CapabilitySupport::Supported,
                application_cursor: CapabilitySupport::Supported,
                application_keypad: CapabilitySupport::Supported,
                bracketed_paste: CapabilitySupport::Supported,
                focus_events: CapabilitySupport::Supported,
                sgr_mouse: CapabilitySupport::Supported,
            },
            title_setting: CapabilitySupport::Supported,
            clipboard: CapabilitySupport::Supported,
            save_restore: SaveRestoreCapabilities {
                cursor: CapabilitySupport::Supported,
                modes: CapabilitySupport::Supported,
            },
        }
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn screen_256color_fallback() -> Self {
        Self {
            line_oriented_output: CapabilitySupport::Supported,
            c0_controls: CapabilitySupport::Supported,
            esc_sequences: CapabilitySupport::Supported,
            csi_sequences: CapabilitySupport::Supported,
            osc_string_controls: CapabilitySupport::Unsupported,
            dcs_string_controls: CapabilitySupport::Unsupported,
            sgr: SgrCapabilities {
                attributes: CapabilitySupport::Supported,
                basic_colors: CapabilitySupport::Supported,
                bright_colors: CapabilitySupport::Supported,
                indexed_256_colors: CapabilitySupport::Supported,
                true_color: CapabilitySupport::Unsupported,
            },
            dec_private_modes: DecPrivateModeCapabilities {
                modes: CapabilitySupport::Supported,
                alternate_screen: CapabilitySupport::Supported,
                application_cursor: CapabilitySupport::Supported,
                application_keypad: CapabilitySupport::Supported,
                bracketed_paste: CapabilitySupport::Unsupported,
                focus_events: CapabilitySupport::Unsupported,
                sgr_mouse: CapabilitySupport::Unsupported,
            },
            title_setting: CapabilitySupport::Unsupported,
            clipboard: CapabilitySupport::Unsupported,
            save_restore: SaveRestoreCapabilities {
                cursor: CapabilitySupport::Supported,
                modes: CapabilitySupport::Unsupported,
            },
        }
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn screen_fallback() -> Self {
        Self {
            line_oriented_output: CapabilitySupport::Supported,
            c0_controls: CapabilitySupport::Supported,
            esc_sequences: CapabilitySupport::Supported,
            csi_sequences: CapabilitySupport::Supported,
            osc_string_controls: CapabilitySupport::Unsupported,
            dcs_string_controls: CapabilitySupport::Unsupported,
            sgr: SgrCapabilities {
                attributes: CapabilitySupport::Supported,
                basic_colors: CapabilitySupport::Supported,
                bright_colors: CapabilitySupport::Supported,
                indexed_256_colors: CapabilitySupport::Unsupported,
                true_color: CapabilitySupport::Unsupported,
            },
            dec_private_modes: DecPrivateModeCapabilities {
                modes: CapabilitySupport::Supported,
                alternate_screen: CapabilitySupport::Supported,
                application_cursor: CapabilitySupport::Supported,
                application_keypad: CapabilitySupport::Supported,
                bracketed_paste: CapabilitySupport::Unsupported,
                focus_events: CapabilitySupport::Unsupported,
                sgr_mouse: CapabilitySupport::Unsupported,
            },
            title_setting: CapabilitySupport::Unsupported,
            clipboard: CapabilitySupport::Unsupported,
            save_restore: SaveRestoreCapabilities {
                cursor: CapabilitySupport::Supported,
                modes: CapabilitySupport::Unsupported,
            },
        }
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn vt100_fallback() -> Self {
        Self {
            line_oriented_output: CapabilitySupport::Supported,
            c0_controls: CapabilitySupport::Supported,
            esc_sequences: CapabilitySupport::Supported,
            csi_sequences: CapabilitySupport::Supported,
            osc_string_controls: CapabilitySupport::Unsupported,
            dcs_string_controls: CapabilitySupport::Unsupported,
            sgr: SgrCapabilities {
                attributes: CapabilitySupport::Supported,
                basic_colors: CapabilitySupport::Unsupported,
                bright_colors: CapabilitySupport::Unsupported,
                indexed_256_colors: CapabilitySupport::Unsupported,
                true_color: CapabilitySupport::Unsupported,
            },
            dec_private_modes: DecPrivateModeCapabilities {
                modes: CapabilitySupport::Supported,
                alternate_screen: CapabilitySupport::Unsupported,
                application_cursor: CapabilitySupport::Supported,
                application_keypad: CapabilitySupport::Supported,
                bracketed_paste: CapabilitySupport::Unsupported,
                focus_events: CapabilitySupport::Unsupported,
                sgr_mouse: CapabilitySupport::Unsupported,
            },
            title_setting: CapabilitySupport::Unsupported,
            clipboard: CapabilitySupport::Unsupported,
            save_restore: SaveRestoreCapabilities {
                cursor: CapabilitySupport::Supported,
                modes: CapabilitySupport::Unsupported,
            },
        }
    }

    /// Defines the fn const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    pub const fn dumb() -> Self {
        Self {
            line_oriented_output: CapabilitySupport::Supported,
            c0_controls: CapabilitySupport::Unsupported,
            esc_sequences: CapabilitySupport::Unsupported,
            csi_sequences: CapabilitySupport::Unsupported,
            osc_string_controls: CapabilitySupport::Unsupported,
            dcs_string_controls: CapabilitySupport::Unsupported,
            sgr: SgrCapabilities {
                attributes: CapabilitySupport::Unsupported,
                basic_colors: CapabilitySupport::Unsupported,
                bright_colors: CapabilitySupport::Unsupported,
                indexed_256_colors: CapabilitySupport::Unsupported,
                true_color: CapabilitySupport::Unsupported,
            },
            dec_private_modes: DecPrivateModeCapabilities {
                modes: CapabilitySupport::Unsupported,
                alternate_screen: CapabilitySupport::Unsupported,
                application_cursor: CapabilitySupport::Unsupported,
                application_keypad: CapabilitySupport::Unsupported,
                bracketed_paste: CapabilitySupport::Unsupported,
                focus_events: CapabilitySupport::Unsupported,
                sgr_mouse: CapabilitySupport::Unsupported,
            },
            title_setting: CapabilitySupport::Unsupported,
            clipboard: CapabilitySupport::Unsupported,
            save_restore: SaveRestoreCapabilities {
                cursor: CapabilitySupport::Unsupported,
                modes: CapabilitySupport::Unsupported,
            },
        }
    }
}

/// Resolve a compatibility profile from its stable configuration name.
pub fn terminal_profile_named(name: &str) -> Result<TerminalCompatibilityProfile> {
    match name {
        "xterm-compatible" => Ok(TerminalProfile::XtermCompatible.compatibility_profile()),
        "dumb" => Ok(TerminalProfile::Dumb.compatibility_profile()),
        _ => Err(MezError::invalid_args(format!(
            "unknown terminal compatibility profile {name}"
        ))),
    }
}

/// Where a selected TERM description came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminfoSource {
    /// Represents the Mezzanine case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Mezzanine,
    /// Represents the Installed Fallback case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    InstalledFallback,
    /// Represents the Built In Dumb case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    BuiltInDumb,
}

/// Capability set associated with a TERM name Mezzanine may expose to panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminfoCapabilityProfile {
    /// Stores the term value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub term: &'static str,
    /// Stores the profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub profile: TerminalProfile,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: TerminfoSource,
    /// Stores the capabilities value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub capabilities: TerminalCapabilities,
}

/// Defines the MEZZANINE TERMINFO PROFILES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const MEZZANINE_TERMINFO_PROFILES: &[TerminfoCapabilityProfile] = &[
    TerminfoCapabilityProfile {
        term: "mez-256color",
        profile: TerminalProfile::XtermCompatible,
        source: TerminfoSource::Mezzanine,
        capabilities: TerminalCapabilities::xterm_compatible(),
    },
    TerminfoCapabilityProfile {
        term: "mezzanine-256color",
        profile: TerminalProfile::XtermCompatible,
        source: TerminfoSource::Mezzanine,
        capabilities: TerminalCapabilities::xterm_compatible(),
    },
];

/// Defines the TERMINFO FALLBACK PROFILES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const TERMINFO_FALLBACK_PROFILES: &[TerminfoCapabilityProfile] = &[
    TerminfoCapabilityProfile {
        term: "screen-256color",
        profile: TerminalProfile::XtermCompatible,
        source: TerminfoSource::InstalledFallback,
        capabilities: TerminalCapabilities::screen_256color_fallback(),
    },
    TerminfoCapabilityProfile {
        term: "screen",
        profile: TerminalProfile::XtermCompatible,
        source: TerminfoSource::InstalledFallback,
        capabilities: TerminalCapabilities::screen_fallback(),
    },
    TerminfoCapabilityProfile {
        term: "vt100",
        profile: TerminalProfile::XtermCompatible,
        source: TerminfoSource::InstalledFallback,
        capabilities: TerminalCapabilities::vt100_fallback(),
    },
    TerminfoCapabilityProfile {
        term: "dumb",
        profile: TerminalProfile::Dumb,
        source: TerminfoSource::InstalledFallback,
        capabilities: TerminalCapabilities::dumb(),
    },
];

/// Carries Terminal Diagnostic Severity state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalDiagnosticSeverity {
    /// Represents the Info case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Info,
    /// Represents the Warning case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Warning,
}

/// Carries Terminal Diagnostic state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalDiagnostic {
    /// Stores the severity value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub severity: TerminalDiagnosticSeverity,
    /// Stores the code value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub code: &'static str,
    /// Stores the message value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message: String,
}

/// Carries Terminfo Selection state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminfoSelection {
    /// Stores the term value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub term: String,
    /// Stores the profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub profile: TerminalProfile,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: TerminfoSource,
    /// Stores the degraded value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub degraded: bool,
    /// Stores the advertised capabilities value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub advertised_capabilities: TerminalCapabilities,
    /// Stores the diagnostics value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub diagnostics: Vec<TerminalDiagnostic>,
}

impl TerminfoSelection {
    /// Runs the profile name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn profile_name(&self) -> &'static str {
        self.profile.name()
    }
}

/// Runs the select terminfo operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn select_terminfo<I>(mez_entry_available: bool, installed_terms: I) -> TerminfoSelection
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    if mez_entry_available {
        return build_terminfo_selection(MEZZANINE_TERMINFO_PROFILES[0], false);
    }

    select_installed_terminfo(installed_terms)
}

/// Runs the select installed terminfo operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn select_installed_terminfo<I>(installed_terms: I) -> TerminfoSelection
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let installed = installed_terms
        .into_iter()
        .map(|term| term.as_ref().to_string())
        .collect::<Vec<_>>();

    if let Some(profile) = MEZZANINE_TERMINFO_PROFILES
        .iter()
        .find(|profile| installed.iter().any(|term| term == profile.term))
    {
        return build_terminfo_selection(*profile, false);
    }

    if let Some(profile) = TERMINFO_FALLBACK_PROFILES
        .iter()
        .find(|profile| installed.iter().any(|term| term == profile.term))
    {
        return build_terminfo_selection(*profile, true);
    }

    build_terminfo_selection(
        TerminfoCapabilityProfile {
            term: "dumb",
            profile: TerminalProfile::Dumb,
            source: TerminfoSource::BuiltInDumb,
            capabilities: TerminalCapabilities::dumb(),
        },
        true,
    )
}

/// Runs the build terminfo selection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn build_terminfo_selection(
    profile: TerminfoCapabilityProfile,
    degraded: bool,
) -> TerminfoSelection {
    TerminfoSelection {
        term: profile.term.to_string(),
        profile: profile.profile,
        source: profile.source,
        degraded,
        advertised_capabilities: profile.capabilities,
        diagnostics: terminfo_diagnostics(profile, degraded),
    }
}

/// Runs the terminfo diagnostics operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminfo_diagnostics(
    profile: TerminfoCapabilityProfile,
    degraded: bool,
) -> Vec<TerminalDiagnostic> {
    let mut diagnostics = Vec::new();
    diagnostics.push(TerminalDiagnostic {
        severity: TerminalDiagnosticSeverity::Info,
        code: "terminal.profile_selected",
        message: format!(
            "active terminal profile={} selected TERM={}",
            profile.profile.name(),
            profile.term
        ),
    });

    if degraded {
        let (code, message) = match profile.source {
            TerminfoSource::InstalledFallback => (
                "terminal.terminfo_fallback",
                format!(
                    "Mezzanine terminfo entry not available; selected fallback TERM={} with degraded capabilities",
                    profile.term
                ),
            ),
            TerminfoSource::BuiltInDumb => (
                "terminal.terminfo_builtin_dumb",
                "no listed fallback terminfo entry is installed; using built-in dumb profile with TERM=dumb; install or print mez-256color terminfo to enable xterm-compatible capabilities".to_string(),
            ),
            TerminfoSource::Mezzanine => (
                "terminal.terminfo_degraded",
                format!(
                    "selected TERM={} from Mezzanine entries but marked degraded",
                    profile.term
                ),
            ),
        };
        diagnostics.push(TerminalDiagnostic {
            severity: TerminalDiagnosticSeverity::Warning,
            code,
            message,
        });
    }

    diagnostics
}
