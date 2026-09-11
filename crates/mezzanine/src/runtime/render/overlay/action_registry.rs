//! Product-owned registry of typed overlay action targets.
//!
//! Rendered Markdown is untrusted presentation text: record bodies, record
//! metadata, prompts, approval summaries, and MCP metadata can all be shaped
//! by data the product does not author. The mux therefore carries only opaque
//! [`OverlayActionId`] values, and this registry is the single place that maps
//! one identity back to a typed, validated target.
//!
//! Every identity embeds the overlay generation that created it, so an
//! identity captured from an earlier render can never resolve against the
//! current generation. Registration is the only way to make a rendered range
//! executable, which means display text alone can never create a control.

use super::record_adapter::runtime_display_is_known_command;
use mez_mux::command::parse_command_sequence;
use mez_mux::overlay::{OverlayActionId, OverlaySelection, OverlaySelectionKind};
use std::collections::BTreeMap;

/// Record-browser commands whose rows may open a record by stable id.
///
/// The list mirrors the record-browser producers; a row whose open command is
/// outside this set registers nothing and renders as inert text.
const RECORD_BROWSER_OPEN_COMMANDS: &[&str] = &[
    "list-personalities",
    "personality",
    "resume",
    "show-approvals",
    "show-context",
    "show-issues",
    "show-memories",
];

/// Typed target behind one registered overlay action.
///
/// Each variant carries only validated components, so the dispatcher can
/// rebuild the exact command line it executes without ever trusting rendered
/// text or a caller-supplied command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverlayActionTarget {
    /// Terminal command with a validated baseline command name.
    TerminalCommand {
        /// Terminal command name without any leading slash.
        name: String,
        /// Unparsed argument text following the command name.
        args: String,
    },
    /// Terminal theme selection with a validated theme name.
    SetTheme {
        /// Theme name selected by this action.
        name: String,
    },
    /// Terminal key-preset selection with a validated preset name.
    SetKeyPreset {
        /// Key-preset name selected by this action.
        name: String,
    },
    /// Record-browser row opening one record by stable id.
    RecordBrowserOpen {
        /// Validated record-browser open command name.
        command_name: String,
        /// Stable record id, guaranteed to be one plain token.
        record_id: String,
    },
    /// Record-browser prompt option selected by visible index.
    RecordBrowserPromptSelect {
        /// Zero-based prompt option index in the rendered selector.
        index: usize,
    },
}

impl OverlayActionTarget {
    /// Returns the agent slash-command line for agent-owned targets.
    pub(crate) fn agent_command_line(&self) -> Option<String> {
        match self {
            Self::RecordBrowserOpen {
                command_name,
                record_id,
            } => Some(format!("/{command_name} {record_id}")),
            Self::TerminalCommand { .. }
            | Self::SetTheme { .. }
            | Self::SetKeyPreset { .. }
            | Self::RecordBrowserPromptSelect { .. } => None,
        }
    }

    /// Returns the terminal command line for terminal-owned targets.
    pub(crate) fn terminal_command_line(&self) -> Option<String> {
        match self {
            Self::TerminalCommand { name, args } => Some(join_command_line("", name, args)),
            Self::SetTheme { name } => Some(format!("set-theme {name}")),
            Self::SetKeyPreset { name } => Some(format!("set-key-preset {name}")),
            Self::RecordBrowserOpen { .. } | Self::RecordBrowserPromptSelect { .. } => None,
        }
    }
}

/// Joins one validated command name with its retained argument text.
fn join_command_line(prefix: &str, name: &str, args: &str) -> String {
    if args.is_empty() {
        format!("{prefix}{name}")
    } else {
        format!("{prefix}{name} {args}")
    }
}

/// One product-composed selectable range before it receives an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeOverlayAction {
    /// Logical action identity shared by physical wrapped-row fragments.
    pub(crate) logical_id: usize,
    /// Zero-based content line containing the action range.
    pub(crate) line_index: usize,
    /// Display column where the interactive range begins.
    pub(crate) start_column: usize,
    /// Display-cell width of the interactive range.
    pub(crate) width: usize,
    /// Typed target this range invokes.
    pub(crate) target: OverlayActionTarget,
    /// Visual importance of this action.
    pub(crate) kind: OverlaySelectionKind,
}

/// Per-generation registry of typed overlay action targets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OverlayActionRegistry {
    generation: u32,
    next_serial: u32,
    actions: BTreeMap<OverlayActionId, OverlayActionTarget>,
}

impl OverlayActionRegistry {
    /// Starts a new overlay generation and invalidates every earlier action.
    ///
    /// Every overlay build calls this before registering its own ranges, so
    /// identities from a previous render can never dispatch.
    pub(crate) fn begin_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.next_serial = 0;
        self.actions.clear();
    }

    /// Returns the generation whose positions are currently valid.
    #[cfg(test)]
    pub(crate) fn current_generation(&self) -> u32 {
        self.generation
    }

    /// Registers one typed target and returns its opaque identity.
    pub(crate) fn register(&mut self, target: OverlayActionTarget) -> OverlayActionId {
        let serial = self.next_serial;
        self.next_serial = self.next_serial.wrapping_add(1);
        let action_id = OverlayActionId(u64::from(self.generation) << 32 | u64::from(serial));
        self.actions.insert(action_id, target);
        action_id
    }

    /// Returns the registered target for one opaque action identity.
    ///
    /// An identity from an earlier generation, an identity that was never
    /// registered, and an identity from another service instance all report
    /// no target.
    pub(crate) fn resolve(&self, action_id: OverlayActionId) -> Option<&OverlayActionTarget> {
        (action_id.0 >> 32 == u64::from(self.generation))
            .then(|| self.actions.get(&action_id))
            .flatten()
    }

    /// Registers every composed range and returns the matching mux selections.
    pub(crate) fn register_all(
        &mut self,
        actions: impl IntoIterator<Item = RuntimeOverlayAction>,
    ) -> Vec<OverlaySelection> {
        actions
            .into_iter()
            .map(|action| {
                let action_id = self.register(action.target);
                OverlaySelection {
                    logical_id: action.logical_id,
                    line_index: action.line_index,
                    start_column: action.start_column,
                    width: action.width,
                    action_id,
                    kind: action.kind,
                }
            })
            .collect()
    }
}

/// Returns true when a rendered command line is safe to hand to an executor.
fn command_line_is_plain(command: &str) -> bool {
    !command.is_empty() && !command.chars().any(char::is_control)
}

/// Returns true when a single-token command name is well formed.
fn command_name_is_plain(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// Splits one single-invocation command line into its name and raw arguments.
///
/// The argument text is retained verbatim after the name token so quoting and
/// flag grouping survive the round trip exactly as authored.
fn split_single_command(command: &str) -> Option<(String, String)> {
    if !command_line_is_plain(command) {
        return None;
    }
    let invocations = parse_command_sequence(command).ok()?;
    if invocations.len() != 1 {
        return None;
    }
    let name = invocations.first()?.name.clone();
    if !command_name_is_plain(&name) {
        return None;
    }
    let trimmed = command.trim();
    let name_start = trimmed.find(name.as_str())?;
    let args = trimmed
        .get(name_start.saturating_add(name.len())..)
        .unwrap_or_default()
        .trim()
        .to_string();
    Some((name, args))
}

/// Returns a validated terminal-command target for one command line.
///
/// The command name must be part of the runtime baseline command set, which is
/// the same allowlist that previously gated selectable terminal choices.
pub(crate) fn overlay_terminal_command_target(command: &str) -> Option<OverlayActionTarget> {
    let (name, args) = split_single_command(command)?;
    if !runtime_display_is_known_command(&name) {
        return None;
    }
    Some(OverlayActionTarget::TerminalCommand { name, args })
}

/// Returns a validated theme-selection target for one action cell label.
pub(crate) fn overlay_set_theme_target(label: &str) -> Option<OverlayActionTarget> {
    let target = overlay_terminal_command_target(label)?;
    match target {
        OverlayActionTarget::TerminalCommand { name, args } if name == "set-theme" => {
            plain_identifier_token(&args).map(|name| OverlayActionTarget::SetTheme { name })
        }
        _ => None,
    }
}

/// Returns a validated key-preset selection target for one action cell label.
pub(crate) fn overlay_set_key_preset_target(label: &str) -> Option<OverlayActionTarget> {
    let target = overlay_terminal_command_target(label)?;
    match target {
        OverlayActionTarget::TerminalCommand { name, args } if name == "set-key-preset" => {
            plain_identifier_token(&args).map(|name| OverlayActionTarget::SetKeyPreset { name })
        }
        _ => None,
    }
}

/// Returns one plain identifier token with no separators or control characters.
///
/// Theme and key-preset names are operator-visible slugs, so anything else is
/// treated as shaped text and registers nothing.
fn plain_identifier_token(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        }))
    .then(|| value.to_string())
}

/// Returns one plain single-token argument that cannot start a second command.
fn plain_command_argument(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.split_whitespace().count() == 1
        && !value
            .chars()
            .any(|character| character.is_control() || character == ';'))
    .then(|| value.to_string())
}

/// Returns a validated record-browser row target for one open command.
///
/// The command name must be one of the record-browser producers and the record
/// id must be a single plain token, so a hostile record id cannot add a second
/// argument or a second invocation to the reconstructed command line.
pub(crate) fn overlay_record_browser_open_target(
    open_command: &str,
) -> Option<OverlayActionTarget> {
    let body = open_command
        .trim()
        .strip_prefix('/')
        .unwrap_or(open_command.trim());
    let (command_name, record_id) = body.split_once(char::is_whitespace)?;
    if !RECORD_BROWSER_OPEN_COMMANDS.contains(&command_name) {
        return None;
    }
    let record_id = plain_command_argument(record_id)?;
    Some(OverlayActionTarget::RecordBrowserOpen {
        command_name: command_name.to_string(),
        record_id,
    })
}

/// Returns one validated record-browser prompt option target.
pub(crate) fn overlay_record_browser_prompt_select_target(index: usize) -> OverlayActionTarget {
    OverlayActionTarget::RecordBrowserPromptSelect { index }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies a new generation invalidates every earlier action identity.
    #[test]
    fn begin_generation_invalidates_earlier_identities() {
        let mut registry = OverlayActionRegistry::default();
        registry.begin_generation();
        let first_generation = registry.current_generation();
        let action_id = registry.register(OverlayActionTarget::SetTheme {
            name: "kanagawa".to_string(),
        });
        assert!(registry.resolve(action_id).is_some());

        registry.begin_generation();

        assert_ne!(registry.current_generation(), first_generation);
        assert_eq!(registry.resolve(action_id), None);
        let fresh = registry.register(OverlayActionTarget::SetTheme {
            name: "kanagawa".to_string(),
        });
        assert_ne!(fresh, action_id);
        assert!(registry.resolve(fresh).is_some());
    }

    /// Verifies an identity that was never registered resolves to no target.
    #[test]
    fn unknown_identity_resolves_to_no_target() {
        let mut registry = OverlayActionRegistry::default();
        registry.begin_generation();
        let registered = registry.register(OverlayActionTarget::SetKeyPreset {
            name: "simple".to_string(),
        });

        assert_eq!(registry.resolve(OverlayActionId(0)), None);
        assert_eq!(registry.resolve(OverlayActionId(registered.0 + 1)), None);
    }

    /// Verifies an identity minted by another generation never resolves here.
    #[test]
    fn foreign_generation_identity_resolves_to_no_target() {
        let mut current = OverlayActionRegistry::default();
        current.begin_generation();
        let mut other = OverlayActionRegistry::default();
        other.begin_generation();
        other.begin_generation();
        let foreign = other.register(OverlayActionTarget::SetTheme {
            name: "kanagawa".to_string(),
        });

        assert_eq!(current.resolve(foreign), None);
    }

    /// Verifies target construction rejects shaped or unknown command text.
    #[test]
    fn target_validation_rejects_shaped_command_text() {
        assert!(overlay_terminal_command_target("set-theme kanagawa").is_some());
        assert!(overlay_terminal_command_target("set-theme kanagawa; list-panes").is_none());
        assert!(overlay_terminal_command_target("set-theme kanagawa\nkill-pane").is_none());
        assert!(overlay_terminal_command_target("unknown-command now").is_none());

        assert_eq!(
            overlay_set_theme_target("set-theme kanagawa"),
            Some(OverlayActionTarget::SetTheme {
                name: "kanagawa".to_string()
            })
        );
        assert!(overlay_set_theme_target("set-theme `kanagawa`").is_none());
        assert!(overlay_set_theme_target("set-theme kanagawa extra").is_none());
        assert!(overlay_set_theme_target("set-key-preset simple").is_none());
        assert_eq!(
            overlay_set_key_preset_target("set-key-preset simple"),
            Some(OverlayActionTarget::SetKeyPreset {
                name: "simple".to_string()
            })
        );

        assert_eq!(
            overlay_record_browser_open_target("/resume 018f"),
            Some(OverlayActionTarget::RecordBrowserOpen {
                command_name: "resume".to_string(),
                record_id: "018f".to_string()
            })
        );
        assert!(overlay_record_browser_open_target("/show-issues issue-1 /approve").is_none());
        assert!(overlay_record_browser_open_target("/show-issues issue-1; /approve").is_none());
        assert!(overlay_record_browser_open_target("/kill-session %1").is_none());
        assert!(overlay_record_browser_open_target("/show-issues").is_none());
    }

    /// Verifies every registered target rebuilds the command line it executes.
    #[test]
    fn registered_targets_rebuild_validated_command_lines() {
        let record = OverlayActionTarget::RecordBrowserOpen {
            command_name: "show-issues".to_string(),
            record_id: "issue-1".to_string(),
        };
        assert_eq!(
            record.agent_command_line().as_deref(),
            Some("/show-issues issue-1")
        );
        assert_eq!(record.terminal_command_line(), None);

        let terminal = overlay_terminal_command_target("select-window -t @2")
            .expect("baseline terminal command should register");
        assert_eq!(
            terminal.terminal_command_line().as_deref(),
            Some("select-window -t @2")
        );
        assert_eq!(terminal.agent_command_line(), None);

        let theme = overlay_set_theme_target("set-theme kanagawa").unwrap();
        assert_eq!(
            theme.terminal_command_line().as_deref(),
            Some("set-theme kanagawa")
        );
        assert_eq!(
            overlay_set_key_preset_target("set-key-preset simple")
                .unwrap()
                .terminal_command_line()
                .as_deref(),
            Some("set-key-preset simple")
        );
    }
}
