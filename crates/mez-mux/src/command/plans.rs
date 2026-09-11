//! Typed plans for pane-layout command families.
//!
//! This module converts parsed command invocations into dependency-neutral
//! resize, swap, break, join, and split-selection plans. It owns command
//! defaults and argument validation, but does not mutate sessions, spawn pane
//! processes, or project errors into the product error aggregate.

use super::{CommandInvocation, flag_value, positional_args_from_slice};
use crate::layout::{
    PaneNavigationDirection, PaneSizeSpec, ResizeAxis, ResizeDirection, SplitDirection,
};
use crate::{MuxError, Result};

/// Typed session-mutation command parsed from a command invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandPlan {
    /// Create a window in the default group.
    NewWindow(PaneSpawningPlan),
    /// Create a window in a new group.
    NewGroup(PaneSpawningPlan),
    /// Rename a group.
    RenameGroup(NamedTargetPlan),
    /// Select a group.
    SelectGroup(TargetPlan),
    /// Select the next group.
    NextGroup { command: String },
    /// Select the previous group.
    PreviousGroup { command: String },
    /// Select the last group.
    LastGroup { command: String },
    /// Kill a group.
    KillGroup(ForceTargetPlan),
    /// Rename a window.
    RenameWindow(NamedTargetPlan),
    /// Rename a pane.
    RenamePane(NamedTargetPlan),
    /// Select a window.
    SelectWindow(TargetPlan),
    /// Select the next window.
    NextWindow { command: String },
    /// Select the previous window.
    PreviousWindow { command: String },
    /// Select the last window.
    LastWindow { command: String },
    /// Move a window to a target index.
    MoveWindow(MoveWindowPlan),
    /// Kill a window.
    KillWindow(ForceTargetPlan),
    /// Split the active pane.
    SplitWindow(SplitWindowPlan),
    /// Select a pane by target or direction.
    SelectPane(SelectPanePlan),
    /// Select the next pane.
    NextPane { command: String },
    /// Select the previous pane.
    PreviousPane { command: String },
    /// Select the last pane.
    LastPane { command: String },
    /// Rotate panes in the active window.
    RotatePane(RotatePanePlan),
    /// Select a layout.
    SelectLayout(LayoutPlan),
    /// Cycle to the next layout.
    NextLayout { command: String },
    /// Rebalance the active window.
    RebalanceWindow { command: String },
    /// Control pane input synchronization for the active window.
    SynchronizePanes(SynchronizePanesPlan),
    /// Toggle pane zoom.
    ZoomPane { command: String },
    /// Resize a pane or toggle zoom through resize syntax.
    ResizePane(ResizePanePlan),
    /// Kill a pane.
    KillPane(ForceTargetPlan),
    /// Swap panes.
    SwapPane(SwapPanePlan),
    /// Break a pane into a new window.
    BreakPane(BreakPanePlan),
    /// Join a pane into another pane.
    JoinPane(JoinPanePlan),
    /// Rename the session.
    RenameSession(SessionNamePlan),
    /// Kill the session.
    KillSession(ForceOnlyPlan),
    /// Detach a client.
    DetachClient(DetachClientPlan),
    /// The command is not a typed session-mutation plan.
    Fallback,
}

/// Parsed command data for window or pane creation commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSpawningPlan {
    /// Original command name.
    pub command: String,
    /// Effective window name.
    pub name: String,
    /// Optional shell command to start in the new pane.
    pub shell_command: Option<String>,
    /// Optional start directory for the pane process.
    pub start_directory: Option<String>,
    /// Whether the created pane or window should be selected.
    pub select: bool,
}

/// Parsed command data for commands with an optional target and required name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTargetPlan {
    /// Original command name.
    pub command: String,
    /// Optional target selector.
    pub target: Option<String>,
    /// Required new name.
    pub name: String,
}

/// Parsed command data for commands with a required target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPlan {
    /// Original command name.
    pub command: String,
    /// Required target selector.
    pub target: String,
}

/// Parsed command data for commands with an optional target and force flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceTargetPlan {
    /// Original command name.
    pub command: String,
    /// Optional target selector.
    pub target: Option<String>,
    /// Whether force behavior was requested.
    pub force: bool,
}

/// Parsed command data for moving a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveWindowPlan {
    /// Original command name.
    pub command: String,
    /// Optional source window selector.
    pub source: Option<String>,
    /// Destination window index.
    pub target_index: usize,
}

/// Parsed command data for splitting a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitWindowPlan {
    /// Original command name.
    pub command: String,
    /// Split direction.
    pub direction: SplitDirection,
    /// Optional shell command to start in the new pane.
    pub shell_command: Option<String>,
    /// Optional start directory for the new pane process.
    pub start_directory: Option<String>,
    /// Whether the new pane should be selected.
    pub select_new: bool,
}

/// Parsed pane-selection mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneSelectionPlan {
    /// Select by explicit target or alias.
    Target(String),
    /// Select by adjacent-pane direction.
    Direction(PaneNavigationDirection),
}

/// Parsed command data for selecting a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectPanePlan {
    /// Original command name.
    pub command: String,
    /// Selection mode.
    pub selection: PaneSelectionPlan,
}

/// Parsed command data for rotating panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotatePanePlan {
    /// Original command name.
    pub command: String,
    /// Whether the rotation direction is reversed.
    pub reverse: bool,
}

/// Parsed command data for selecting a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutPlan {
    /// Original command name.
    pub command: String,
    /// Layout name.
    pub layout_name: String,
}

/// Parsed command data for observer target mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserverTargetPlan {
    /// Original command name.
    pub command: String,
    /// Observer request or client target.
    pub target: String,
}

/// Parsed command data for rename-session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionNamePlan {
    /// Original command name.
    pub command: String,
    /// New session name.
    pub name: String,
}

/// Parsed command data for force-only commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceOnlyPlan {
    /// Original command name.
    pub command: String,
    /// Whether force behavior was requested.
    pub force: bool,
}

/// Parsed command data for detach-client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachClientPlan {
    /// Original command name.
    pub command: String,
    /// Optional client target. Execution supplies the primary client default.
    pub target: Option<String>,
}

/// Requested synchronization mode for the active window's panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynchronizePanesMode {
    /// Enable synchronized pane input.
    On,
    /// Disable synchronized pane input.
    Off,
    /// Toggle synchronized pane input.
    Toggle,
    /// Report synchronized pane input state.
    Status,
}

/// Parsed command data for synchronize-panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynchronizePanesPlan {
    /// Original command name.
    pub command: String,
    /// Requested synchronization mode.
    pub mode: SynchronizePanesMode,
}

/// Parsed resize-pane behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResizePanePlan {
    /// Toggle pane zoom instead of resizing.
    Zoom {
        /// Original command name.
        command: String,
    },
    /// Resize a target pane.
    Resize {
        /// Original command name.
        command: String,
        /// Optional pane target.
        target: Option<String>,
        /// Parsed pane-size specification.
        spec: PaneSizeSpec,
    },
}

/// Directional neighbor used by swap-pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapPaneNeighbor {
    /// Previous pane in window order.
    Previous,
    /// Next pane in window order.
    Next,
}

/// Parsed swap-pane behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapPanePlan {
    /// Swap source with an explicit target.
    Target {
        /// Original command name.
        command: String,
        /// Optional source pane selector.
        source: Option<String>,
        /// Destination pane selector.
        target: String,
    },
    /// Swap the active pane with a neighbor.
    Neighbor {
        /// Original command name.
        command: String,
        /// Neighbor selection.
        neighbor: SwapPaneNeighbor,
    },
}

/// Parsed command data for break-pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakPanePlan {
    /// Original command name.
    pub command: String,
    /// Optional source pane target.
    pub target: Option<String>,
    /// Optional new window name.
    pub name: Option<String>,
    /// Whether the new window should be selected.
    pub select: bool,
}

/// Parsed command data for join-pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinPanePlan {
    /// Original command name.
    pub command: String,
    /// Optional source pane selector.
    pub source: Option<String>,
    /// Destination pane selector.
    pub target: String,
    /// Join direction.
    pub direction: SplitDirection,
    /// Whether the joined pane should be selected.
    pub select: bool,
}

/// Pane-layout commands whose resize effects are applied by the product runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimePaneLayoutPlan {
    /// Swap panes and apply the resulting resize effects.
    Swap(SwapPanePlan),
    /// Move a pane into a new window and apply the resulting resize effects.
    Break(BreakPanePlan),
    /// Move a pane into another window and apply the resulting resize effects.
    Join(JoinPanePlan),
}

/// Converts one parsed command invocation into a typed session-mutation plan.
pub fn command_plan_from_invocation(invocation: &CommandInvocation) -> Result<CommandPlan> {
    let command = invocation.name.clone();
    match invocation.name.as_str() {
        "new-window" | "neww" => Ok(CommandPlan::NewWindow(pane_spawning_plan(invocation)?)),
        "new-group" | "newg" => Ok(CommandPlan::NewGroup(pane_spawning_plan(invocation)?)),
        "rename-group" | "renameg" => Ok(CommandPlan::RenameGroup(named_target_plan(
            invocation,
            "rename-group requires a name",
        )?)),
        "select-group" | "selectg" => Ok(CommandPlan::SelectGroup(required_target_plan(
            invocation,
            "select-group requires a target",
        )?)),
        "next-group" | "nextg" => Ok(CommandPlan::NextGroup { command }),
        "previous-group" | "prevg" => Ok(CommandPlan::PreviousGroup { command }),
        "last-group" | "lastg" => Ok(CommandPlan::LastGroup { command }),
        "kill-group" | "killg" => Ok(CommandPlan::KillGroup(force_target_plan(invocation))),
        "rename-window" | "renamew" => Ok(CommandPlan::RenameWindow(named_target_plan(
            invocation,
            "rename-window requires a name",
        )?)),
        "rename-pane" => Ok(CommandPlan::RenamePane(rename_pane_plan(invocation)?)),
        "select-window" | "selectw" => Ok(CommandPlan::SelectWindow(required_target_plan(
            invocation,
            "select-window requires a target",
        )?)),
        "next-window" | "next" | "nextw" => Ok(CommandPlan::NextWindow { command }),
        "previous-window" | "previous" | "prev" | "prevw" => {
            Ok(CommandPlan::PreviousWindow { command })
        }
        "last-window" | "lastw" => Ok(CommandPlan::LastWindow { command }),
        "move-window" | "movew" => Ok(CommandPlan::MoveWindow(move_window_plan(invocation)?)),
        "kill-window" | "killw" => Ok(CommandPlan::KillWindow(force_target_plan(invocation))),
        "split-window" | "splitw" => Ok(CommandPlan::SplitWindow(split_window_plan(invocation)?)),
        "select-pane" | "selectp" => Ok(CommandPlan::SelectPane(select_pane_plan(invocation)?)),
        "next-pane" | "nextp" => Ok(CommandPlan::NextPane { command }),
        "previous-pane" | "prev-pane" | "prevp" => Ok(CommandPlan::PreviousPane { command }),
        "last-pane" | "lastp" => Ok(CommandPlan::LastPane { command }),
        "rotate-pane" | "rotatep" => Ok(CommandPlan::RotatePane(RotatePanePlan {
            command,
            reverse: invocation.has_flag("-D", "--reverse"),
        })),
        "select-layout" => Ok(CommandPlan::SelectLayout(layout_plan(invocation)?)),
        "next-layout" => Ok(CommandPlan::NextLayout { command }),
        "rebalance-window" => Ok(CommandPlan::RebalanceWindow { command }),
        "synchronize-panes" | "sync-panes" => Ok(CommandPlan::SynchronizePanes(
            synchronize_panes_plan(invocation)?,
        )),
        "zoom-pane" => Ok(CommandPlan::ZoomPane { command }),
        "resize-pane" | "resizep" => Ok(CommandPlan::ResizePane(resize_pane_plan(invocation)?)),
        "kill-pane" | "killp" => Ok(CommandPlan::KillPane(force_target_plan(invocation))),
        "swap-pane" | "swapp" => Ok(CommandPlan::SwapPane(swap_pane_plan(invocation)?)),
        "break-pane" | "breakp" => Ok(CommandPlan::BreakPane(break_pane_plan(invocation))),
        "join-pane" | "joinp" => Ok(CommandPlan::JoinPane(join_pane_plan(invocation)?)),
        "rename-session" | "renames" => Ok(CommandPlan::RenameSession(session_name_plan(
            invocation,
            "rename-session requires a name",
        )?)),
        "kill-session" => Ok(CommandPlan::KillSession(ForceOnlyPlan {
            command,
            force: invocation.has_flag("-f", "--force"),
        })),
        "detach-client" | "detach" => Ok(CommandPlan::DetachClient(DetachClientPlan {
            command,
            target: invocation.target_arg().map(ToOwned::to_owned),
        })),
        _ => Ok(CommandPlan::Fallback),
    }
}

/// Returns the effective window name parsed from a new-window invocation.
pub fn new_window_name(invocation: &CommandInvocation) -> String {
    flag_value(&invocation.args, "-n")
        .or_else(|| flag_value(&invocation.args, "--name"))
        .map(ToOwned::to_owned)
        .or_else(|| {
            if double_dash_tail_start(&invocation.args).is_some()
                || explicit_shell_command_value(&invocation.args).is_some()
            {
                None
            } else {
                positional_args_before_double_dash(invocation)
                    .first()
                    .map(|value| (*value).to_string())
            }
        })
        .unwrap_or_else(|| "shell".to_string())
}

/// Accepted spellings for an explicit pane shell command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellCommandFlag {
    /// `--shell-command`
    ShellCommand,
    /// `--command`
    Command,
}

impl ShellCommandFlag {
    /// Returns the accepted spelling of this flag.
    pub fn spelling(self) -> &'static str {
        match self {
            Self::ShellCommand => "--shell-command",
            Self::Command => "--command",
        }
    }

    /// Returns the index of this flag's value when the flag has one.
    fn value_index(self, args: &[String]) -> Option<usize> {
        args.iter()
            .position(|argument| argument == self.spelling())
            .and_then(|index| index.checked_add(1))
            .filter(|index| *index < args.len())
    }
}

/// Where a pane command's shell source comes from.
///
/// Plan readers and prompt completion share this one precedence rule: an
/// explicit `--shell-command`/`--command` value wins, then words after the
/// first `--`, then positional words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellCommandSource {
    /// The value at this argument index is user-authored shell source.
    ExplicitFlagValue {
        /// Flag spelling that supplied the value.
        flag: ShellCommandFlag,
        /// Argument index of the value.
        value_index: usize,
    },
    /// Words after the first `--` are shell source, starting at this index.
    DoubleDashTail {
        /// Argument index of the first word after `--`.
        start: usize,
    },
    /// Positional words before any `--` are shell source.
    PositionalWords,
    /// The invocation supplies no shell source.
    None,
}

impl ShellCommandSource {
    /// Classifies shell source for a `new-window` argument list.
    pub fn for_new_window(args: &[String]) -> Self {
        if let Some((flag, value_index)) = explicit_shell_command_value(args) {
            return Self::ExplicitFlagValue { flag, value_index };
        }
        if let Some(start) = double_dash_tail_start(args) {
            return Self::DoubleDashTail { start };
        }
        if has_name_flag_value(args) {
            return Self::PositionalWords;
        }
        Self::None
    }

    /// Classifies shell source for a `split-window` argument list.
    pub fn for_split_window(args: &[String]) -> Self {
        if let Some((flag, value_index)) = explicit_shell_command_value(args) {
            return Self::ExplicitFlagValue { flag, value_index };
        }
        if let Some(start) = double_dash_tail_start(args) {
            return Self::DoubleDashTail { start };
        }
        Self::PositionalWords
    }
}

/// Returns the first explicit shell-command value and the flag that supplied it.
fn explicit_shell_command_value(args: &[String]) -> Option<(ShellCommandFlag, usize)> {
    for flag in [ShellCommandFlag::ShellCommand, ShellCommandFlag::Command] {
        if let Some(value_index) = flag.value_index(args) {
            return Some((flag, value_index));
        }
    }
    None
}

/// Returns the index of the first word after `--`.
fn double_dash_tail_start(args: &[String]) -> Option<usize> {
    args.iter()
        .position(|argument| argument == "--")
        .map(|index| index.saturating_add(1))
}

/// Returns whether `-n`/`--name` has a following value argument.
fn has_name_flag_value(args: &[String]) -> bool {
    args.windows(2)
        .any(|window| window[0] == "-n" || window[0] == "--name")
}

/// Returns the optional shell command parsed from a new-window invocation.
pub fn new_window_shell_command(invocation: &CommandInvocation) -> Result<Option<String>> {
    shell_command_for_source(
        invocation,
        ShellCommandSource::for_new_window(&invocation.args),
    )
}

/// Returns the optional shell command parsed from a split-window invocation.
pub fn split_window_shell_command(invocation: &CommandInvocation) -> Result<Option<String>> {
    shell_command_for_source(
        invocation,
        ShellCommandSource::for_split_window(&invocation.args),
    )
}

/// Resolves one classified shell source into the effective command string.
fn shell_command_for_source(
    invocation: &CommandInvocation,
    source: ShellCommandSource,
) -> Result<Option<String>> {
    match source {
        ShellCommandSource::ExplicitFlagValue { value_index, .. } => {
            let command = invocation
                .args
                .get(value_index)
                .map(String::as_str)
                .unwrap_or_default();
            if command.trim().is_empty() {
                return Err(MuxError::invalid_args(
                    "pane shell command must not be empty",
                ));
            }
            Ok(Some(command.to_string()))
        }
        ShellCommandSource::DoubleDashTail { start } => shell_command_from_words(
            invocation
                .args
                .get(start..)
                .unwrap_or_default()
                .iter()
                .map(String::as_str)
                .collect(),
        ),
        ShellCommandSource::PositionalWords => {
            shell_command_from_words(positional_args_before_double_dash(invocation))
        }
        ShellCommandSource::None => Ok(None),
    }
}

/// Parses a resize-pane invocation into a typed plan.
pub fn resize_pane_plan(invocation: &CommandInvocation) -> Result<ResizePanePlan> {
    let command = invocation.name.clone();
    if invocation.has_flag("-Z", "--zoom") {
        return Ok(ResizePanePlan::Zoom { command });
    }
    Ok(ResizePanePlan::Resize {
        command,
        target: invocation.target_arg().map(ToOwned::to_owned),
        spec: resize_spec_from_invocation(invocation)?,
    })
}

/// Parses a layout command that requires runtime-owned resize synchronization.
pub fn runtime_pane_layout_plan_from_invocation(
    invocation: &CommandInvocation,
) -> Result<Option<RuntimePaneLayoutPlan>> {
    let plan = match invocation.name.as_str() {
        "swap-pane" | "swapp" => RuntimePaneLayoutPlan::Swap(swap_pane_plan(invocation)?),
        "break-pane" | "breakp" => RuntimePaneLayoutPlan::Break(break_pane_plan(invocation)),
        "join-pane" | "joinp" => RuntimePaneLayoutPlan::Join(join_pane_plan(invocation)?),
        _ => return Ok(None),
    };
    Ok(Some(plan))
}

/// Builds the pane-size specification requested by `resize-pane`.
pub fn resize_spec_from_invocation(invocation: &CommandInvocation) -> Result<PaneSizeSpec> {
    if let Some(percent) = flag_value(&invocation.args, "--percent") {
        let percent = parse_resize_amount(percent, "resize-pane percent is invalid")?;
        let axis = match flag_value(&invocation.args, "--axis").unwrap_or("both") {
            "columns" | "horizontal" => ResizeAxis::Columns,
            "rows" | "vertical" => ResizeAxis::Rows,
            "both" => ResizeAxis::Both,
            _ => return Err(MuxError::invalid_args("resize-pane axis is invalid")),
        };
        return Ok(PaneSizeSpec::Percent { percent, axis });
    }
    if let Some(direction) = flag_value(&invocation.args, "--delta") {
        let direction = ResizeDirection::from_name(direction)
            .ok_or_else(|| MuxError::invalid_args("resize-pane delta direction is invalid"))?;
        return Ok(PaneSizeSpec::Delta {
            direction,
            amount: resize_amount_flag(invocation)?,
        });
    }
    if let Some(edge) = flag_value(&invocation.args, "--edge") {
        let edge = ResizeDirection::from_name(edge)
            .ok_or_else(|| MuxError::invalid_args("resize-pane edge is invalid"))?;
        return Ok(PaneSizeSpec::Edge {
            edge,
            amount: resize_amount_flag(invocation)?,
        });
    }
    for (flag, direction) in [
        ("-L", ResizeDirection::Left),
        ("-R", ResizeDirection::Right),
        ("-U", ResizeDirection::Up),
        ("-D", ResizeDirection::Down),
    ] {
        if invocation.args.iter().any(|argument| argument == flag) {
            return Ok(PaneSizeSpec::Delta {
                direction,
                amount: optional_flag_amount(&invocation.args, flag)?,
            });
        }
    }

    let columns = flag_value(&invocation.args, "-x")
        .or_else(|| flag_value(&invocation.args, "--columns"))
        .map(|value| parse_resize_amount(value, "resize-pane columns are invalid"))
        .transpose()?;
    let rows = flag_value(&invocation.args, "-y")
        .or_else(|| flag_value(&invocation.args, "--rows"))
        .map(|value| parse_resize_amount(value, "resize-pane rows are invalid"))
        .transpose()?;
    if columns.is_none() && rows.is_none() {
        return Err(MuxError::invalid_args(
            "resize-pane requires a size, percent, delta, or edge",
        ));
    }
    Ok(PaneSizeSpec::Cells { columns, rows })
}

/// Returns whether `split-window` should select the newly created pane.
pub fn split_window_selects_new_pane(invocation: &CommandInvocation) -> Result<bool> {
    let explicit_select = invocation
        .args
        .iter()
        .any(|argument| argument == "--select");
    let detached = invocation.has_flag("-d", "--detached")
        || invocation
            .args
            .iter()
            .any(|argument| argument == "--no-select");
    if explicit_select && detached {
        return Err(MuxError::invalid_args(
            "split-window cannot combine --select with -d/--no-select",
        ));
    }
    Ok(!detached)
}

/// Parses an explicit-target or adjacent-neighbor swap-pane invocation.
pub fn swap_pane_plan(invocation: &CommandInvocation) -> Result<SwapPanePlan> {
    let command = invocation.name.clone();
    if let Some(target) = invocation
        .target_arg()
        .or_else(|| invocation.positional_args().first().copied())
    {
        return Ok(SwapPanePlan::Target {
            command,
            source: invocation.source_arg().map(ToOwned::to_owned),
            target: target.to_string(),
        });
    }
    if let Some(neighbor) = swap_pane_neighbor(invocation)? {
        if invocation.source_arg().is_some() {
            return Err(MuxError::invalid_args(
                "swap-pane direction flags operate on the active pane",
            ));
        }
        return Ok(SwapPanePlan::Neighbor { command, neighbor });
    }
    Err(MuxError::invalid_args("swap-pane requires a target"))
}

/// Parses a break-pane invocation into a pane-move plan.
pub fn break_pane_plan(invocation: &CommandInvocation) -> BreakPanePlan {
    BreakPanePlan {
        command: invocation.name.clone(),
        target: invocation
            .target_arg()
            .or_else(|| invocation.positional_args().first().copied())
            .map(ToOwned::to_owned),
        name: flag_value(&invocation.args, "-n")
            .or_else(|| flag_value(&invocation.args, "--name"))
            .map(ToOwned::to_owned),
        select: !invocation.has_flag("-d", "--detached"),
    }
}

/// Parses a join-pane invocation into a pane-move plan.
pub fn join_pane_plan(invocation: &CommandInvocation) -> Result<JoinPanePlan> {
    let target = invocation
        .target_arg()
        .or_else(|| invocation.positional_args().first().copied())
        .ok_or_else(|| MuxError::invalid_args("join-pane requires a target"))?;
    let direction = if invocation.has_flag("-h", "--horizontal") {
        SplitDirection::Horizontal
    } else {
        SplitDirection::Vertical
    };
    Ok(JoinPanePlan {
        command: invocation.name.clone(),
        source: invocation.source_arg().map(ToOwned::to_owned),
        target: target.to_string(),
        direction,
        select: invocation
            .args
            .iter()
            .any(|argument| argument == "--select"),
    })
}

fn synchronize_panes_plan(invocation: &CommandInvocation) -> Result<SynchronizePanesPlan> {
    let mode = match invocation
        .positional_args()
        .first()
        .copied()
        .unwrap_or("toggle")
    {
        "on" => SynchronizePanesMode::On,
        "off" => SynchronizePanesMode::Off,
        "toggle" => SynchronizePanesMode::Toggle,
        "status" => SynchronizePanesMode::Status,
        _ => {
            return Err(MuxError::invalid_args(
                "synchronize-panes accepts on, off, toggle, or status",
            ));
        }
    };
    Ok(SynchronizePanesPlan {
        command: invocation.name.clone(),
        mode,
    })
}

fn pane_spawning_plan(invocation: &CommandInvocation) -> Result<PaneSpawningPlan> {
    Ok(PaneSpawningPlan {
        command: invocation.name.clone(),
        name: new_window_name(invocation),
        shell_command: new_window_shell_command(invocation)?,
        start_directory: invocation.start_directory_arg().map(ToOwned::to_owned),
        select: !invocation.has_flag("-d", "--detached"),
    })
}

fn named_target_plan(
    invocation: &CommandInvocation,
    missing: &'static str,
) -> Result<NamedTargetPlan> {
    let name = invocation.positional_args().join(" ");
    if name.is_empty() {
        return Err(MuxError::invalid_args(missing));
    }
    Ok(NamedTargetPlan {
        command: invocation.name.clone(),
        target: invocation.target_arg().map(ToOwned::to_owned),
        name,
    })
}

/// Parses a pane rename while rejecting names that contain only whitespace.
fn rename_pane_plan(invocation: &CommandInvocation) -> Result<NamedTargetPlan> {
    let plan = named_target_plan(invocation, "rename-pane requires a name")?;
    if plan.name.trim().is_empty() {
        return Err(MuxError::invalid_args("rename-pane requires a name"));
    }
    Ok(plan)
}

fn required_target_plan(
    invocation: &CommandInvocation,
    missing: &'static str,
) -> Result<TargetPlan> {
    let target = invocation
        .target_arg()
        .or_else(|| invocation.positional_args().first().copied())
        .ok_or_else(|| MuxError::invalid_args(missing))?;
    Ok(TargetPlan {
        command: invocation.name.clone(),
        target: target.to_string(),
    })
}

fn force_target_plan(invocation: &CommandInvocation) -> ForceTargetPlan {
    ForceTargetPlan {
        command: invocation.name.clone(),
        target: invocation.target_arg().map(ToOwned::to_owned),
        force: invocation.has_flag("-f", "--force"),
    }
}

fn move_window_plan(invocation: &CommandInvocation) -> Result<MoveWindowPlan> {
    Ok(MoveWindowPlan {
        command: invocation.name.clone(),
        source: invocation.source_arg().map(ToOwned::to_owned),
        target_index: move_window_target_index(invocation)?,
    })
}

fn split_window_plan(invocation: &CommandInvocation) -> Result<SplitWindowPlan> {
    let direction = if invocation.has_flag("-h", "--horizontal") {
        SplitDirection::Horizontal
    } else {
        SplitDirection::Vertical
    };
    Ok(SplitWindowPlan {
        command: invocation.name.clone(),
        direction,
        shell_command: split_window_shell_command(invocation)?,
        start_directory: invocation.start_directory_arg().map(ToOwned::to_owned),
        select_new: split_window_selects_new_pane(invocation)?,
    })
}

fn select_pane_plan(invocation: &CommandInvocation) -> Result<SelectPanePlan> {
    let selection = if let Some(target) = invocation
        .target_arg()
        .or_else(|| invocation.positional_args().first().copied())
    {
        PaneSelectionPlan::Target(target.to_string())
    } else if let Some(direction) = select_pane_direction(invocation)? {
        PaneSelectionPlan::Direction(direction)
    } else {
        return Err(MuxError::invalid_args("select-pane requires a target"));
    };
    Ok(SelectPanePlan {
        command: invocation.name.clone(),
        selection,
    })
}

fn layout_plan(invocation: &CommandInvocation) -> Result<LayoutPlan> {
    let layout_name = invocation
        .positional_args()
        .first()
        .copied()
        .ok_or_else(|| MuxError::invalid_args("select-layout requires a layout"))?;
    Ok(LayoutPlan {
        command: invocation.name.clone(),
        layout_name: layout_name.to_string(),
    })
}

fn session_name_plan(
    invocation: &CommandInvocation,
    missing: &'static str,
) -> Result<SessionNamePlan> {
    let name = invocation.positional_args().join(" ");
    if name.is_empty() {
        return Err(MuxError::invalid_args(missing));
    }
    Ok(SessionNamePlan {
        command: invocation.name.clone(),
        name,
    })
}

fn select_pane_direction(
    invocation: &CommandInvocation,
) -> Result<Option<PaneNavigationDirection>> {
    let mut matched = [
        ("-U", PaneNavigationDirection::Up),
        ("--up", PaneNavigationDirection::Up),
        ("-D", PaneNavigationDirection::Down),
        ("--down", PaneNavigationDirection::Down),
        ("-L", PaneNavigationDirection::Left),
        ("--left", PaneNavigationDirection::Left),
        ("-R", PaneNavigationDirection::Right),
        ("--right", PaneNavigationDirection::Right),
    ]
    .into_iter()
    .filter_map(|(flag, direction)| {
        invocation
            .args
            .iter()
            .any(|argument| argument == flag)
            .then_some(direction)
    });
    let direction = matched.next();
    if matched.next().is_some() {
        return Err(MuxError::invalid_args(
            "select-pane accepts only one direction flag",
        ));
    }
    Ok(direction)
}

fn move_window_target_index(invocation: &CommandInvocation) -> Result<usize> {
    let target = invocation
        .target_arg()
        .or_else(|| invocation.positional_args().first().copied())
        .ok_or_else(|| MuxError::invalid_args("move-window requires a target index"))?;
    target
        .parse::<usize>()
        .map_err(|_| MuxError::invalid_args("move-window target must be a window index"))
}

fn positional_args_before_double_dash(invocation: &CommandInvocation) -> Vec<&str> {
    let end = invocation
        .args
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(invocation.args.len());
    positional_args_from_slice(&invocation.args[..end])
}

fn shell_command_from_words(words: Vec<&str>) -> Result<Option<String>> {
    if words.is_empty() {
        return Ok(None);
    }
    let command = shell_join_words(&words);
    if command.trim().is_empty() {
        return Err(MuxError::invalid_args(
            "pane shell command must not be empty",
        ));
    }
    Ok(Some(command))
}

fn shell_join_words(words: &[&str]) -> String {
    words
        .iter()
        .map(|word| shell_quote_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote_word(word: &str) -> String {
    if !word.is_empty()
        && word.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
                )
        })
    {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

fn resize_amount_flag(invocation: &CommandInvocation) -> Result<u16> {
    flag_value(&invocation.args, "--amount")
        .map(|value| parse_resize_amount(value, "resize-pane amount is invalid"))
        .transpose()?
        .ok_or_else(|| MuxError::invalid_args("resize-pane requires --amount"))
}

fn optional_flag_amount(args: &[String], flag: &str) -> Result<u16> {
    let Some(index) = args.iter().position(|argument| argument == flag) else {
        return Ok(1);
    };
    let Some(value) = args.get(index.saturating_add(1)) else {
        return Ok(1);
    };
    if value.starts_with('-') {
        return Ok(1);
    }
    parse_resize_amount(value, "resize-pane amount is invalid")
}

fn parse_resize_amount(value: &str, message: &'static str) -> Result<u16> {
    value
        .parse::<u16>()
        .map_err(|_| MuxError::invalid_args(message))
}

fn swap_pane_neighbor(invocation: &CommandInvocation) -> Result<Option<SwapPaneNeighbor>> {
    let mut matched = [
        ("-U", SwapPaneNeighbor::Previous),
        ("--up", SwapPaneNeighbor::Previous),
        ("-D", SwapPaneNeighbor::Next),
        ("--down", SwapPaneNeighbor::Next),
    ]
    .into_iter()
    .filter_map(|(flag, neighbor)| {
        invocation
            .args
            .iter()
            .any(|argument| argument == flag)
            .then_some(neighbor)
    });
    let neighbor = matched.next();
    if matched.next().is_some() {
        return Err(MuxError::invalid_args(
            "swap-pane accepts only one direction flag",
        ));
    }
    Ok(neighbor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::parse_command_sequence;

    fn invocation(input: &str) -> CommandInvocation {
        parse_command_sequence(input).unwrap().remove(0)
    }

    /// Verifies mutating pane commands are parsed into typed plans before
    /// execution so flag and default handling stays separate from mutation.
    #[test]
    fn typed_command_plan_parses_resize_pane_before_execution() {
        let plan =
            command_plan_from_invocation(&invocation("resize-pane -t 1 --percent 50 --axis rows"))
                .unwrap();

        assert_eq!(
            plan,
            CommandPlan::ResizePane(ResizePanePlan::Resize {
                command: "resize-pane".to_string(),
                target: Some("1".to_string()),
                spec: PaneSizeSpec::Percent {
                    percent: 50,
                    axis: ResizeAxis::Rows,
                },
            })
        );
    }

    /// Verifies window and split plans retain names, directories, focus
    /// defaults, and safely reconstructed shell commands for runtime spawning.
    #[test]
    fn parses_pane_spawning_command_details() {
        let new_window = command_plan_from_invocation(&invocation(
            "new-window -n build -c /tmp -- echo 'hello world'",
        ))
        .unwrap();
        let split =
            command_plan_from_invocation(&invocation("split-window -h -d -- make test")).unwrap();

        assert!(matches!(
            new_window,
            CommandPlan::NewWindow(PaneSpawningPlan {
                name,
                shell_command: Some(shell_command),
                start_directory: Some(start_directory),
                select: true,
                ..
            }) if name == "build"
                && shell_command == "echo 'hello world'"
                && start_directory == "/tmp"
        ));
        assert!(matches!(
            split,
            CommandPlan::SplitWindow(SplitWindowPlan {
                direction: SplitDirection::Horizontal,
                shell_command: Some(shell_command),
                select_new: false,
                ..
            }) if shell_command == "make test"
        ));
    }

    /// Verifies group, pane, observer, and synchronization command families
    /// produce lower-owned plans without requiring a product session instance.
    #[test]
    fn parses_session_command_families_without_mutation() {
        assert!(matches!(
            command_plan_from_invocation(&invocation("rename-group -t 2 work tree")).unwrap(),
            CommandPlan::RenameGroup(NamedTargetPlan { target: Some(target), name, .. })
                if target == "2" && name == "work tree"
        ));
        assert!(matches!(
            command_plan_from_invocation(&invocation("rename-pane -t %2 build logs")).unwrap(),
            CommandPlan::RenamePane(NamedTargetPlan { target: Some(target), name, .. })
                if target == "%2" && name == "build logs"
        ));
        assert!(matches!(
            command_plan_from_invocation(&invocation("select-pane -L")).unwrap(),
            CommandPlan::SelectPane(SelectPanePlan {
                selection: PaneSelectionPlan::Direction(PaneNavigationDirection::Left),
                ..
            })
        ));
        assert!(matches!(
            command_plan_from_invocation(&invocation("synchronize-panes status")).unwrap(),
            CommandPlan::SynchronizePanes(SynchronizePanesPlan {
                mode: SynchronizePanesMode::Status,
                ..
            })
        ));
    }

    /// Verifies invalid lower-owned session command arguments retain mux error
    /// categories and stable diagnostics before product error projection.
    #[test]
    fn rejects_invalid_session_command_plan_arguments() {
        let error = command_plan_from_invocation(&invocation("move-window nowhere")).unwrap_err();

        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
        assert!(error.message().contains("window index"));

        let error = command_plan_from_invocation(&invocation("rename-pane")).unwrap_err();
        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
        assert!(error.message().contains("rename-pane requires a name"));

        let error = command_plan_from_invocation(&invocation("rename-pane '   '")).unwrap_err();
        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
        assert!(error.message().contains("rename-pane requires a name"));
    }

    /// Verifies directional resize shorthand defaults to one cell when no
    /// explicit amount follows the direction flag.
    #[test]
    fn directional_resize_defaults_to_one_cell() {
        let spec = resize_spec_from_invocation(&invocation("resize-pane -R")).unwrap();

        assert_eq!(
            spec,
            PaneSizeSpec::Delta {
                direction: ResizeDirection::Right,
                amount: 1,
            }
        );
    }

    /// Verifies runtime pane-layout parsing produces typed swap, break, and
    /// join plans while unrelated commands remain outside this boundary.
    #[test]
    fn parses_runtime_pane_layout_command_family() {
        assert!(matches!(
            runtime_pane_layout_plan_from_invocation(&invocation("swap-pane -t 2")).unwrap(),
            Some(RuntimePaneLayoutPlan::Swap(SwapPanePlan::Target { target, .. }))
                if target == "2"
        ));
        assert!(matches!(
            runtime_pane_layout_plan_from_invocation(&invocation("break-pane -n build")).unwrap(),
            Some(RuntimePaneLayoutPlan::Break(BreakPanePlan { name: Some(name), .. }))
                if name == "build"
        ));
        assert!(matches!(
            runtime_pane_layout_plan_from_invocation(&invocation("join-pane -h -t 3")).unwrap(),
            Some(RuntimePaneLayoutPlan::Join(JoinPanePlan {
                target,
                direction: SplitDirection::Horizontal,
                ..
            })) if target == "3"
        ));
        assert_eq!(
            runtime_pane_layout_plan_from_invocation(&invocation("select-pane -t 1")).unwrap(),
            None
        );
    }

    /// Verifies contradictory swap directions are rejected before the runtime
    /// attempts to resolve an adjacent pane.
    #[test]
    fn rejects_multiple_swap_neighbor_directions() {
        let error =
            runtime_pane_layout_plan_from_invocation(&invocation("swap-pane -U -D")).unwrap_err();

        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
        assert!(error.message().contains("only one direction flag"));
    }

    /// Verifies explicit selection cannot be combined with detached split
    /// syntax because the two flags request incompatible focus outcomes.
    #[test]
    fn rejects_conflicting_split_selection_flags() {
        let error =
            split_window_selects_new_pane(&invocation("split-window --select -d")).unwrap_err();

        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
        assert!(error.message().contains("cannot combine"));
    }

    /// Verifies the shared shell-source classifier keeps the documented
    /// precedence and that both plan readers resolve each source identically.
    #[test]
    fn classifies_pane_shell_command_sources_with_shared_precedence() {
        let explicit = invocation("split-window --shell-command 'echo hi' -- tail");
        assert_eq!(
            ShellCommandSource::for_split_window(&explicit.args),
            ShellCommandSource::ExplicitFlagValue {
                flag: ShellCommandFlag::ShellCommand,
                value_index: 1,
            }
        );
        assert_eq!(
            split_window_shell_command(&explicit).unwrap(),
            Some("echo hi".to_string())
        );

        let command_flag = invocation("new-window --command make -n build -- ignored");
        assert_eq!(
            ShellCommandSource::for_new_window(&command_flag.args),
            ShellCommandSource::ExplicitFlagValue {
                flag: ShellCommandFlag::Command,
                value_index: 1,
            }
        );
        assert_eq!(
            new_window_shell_command(&command_flag).unwrap(),
            Some("make".to_string())
        );

        let tail = invocation("split-window -h -- make test");
        assert_eq!(
            ShellCommandSource::for_split_window(&tail.args),
            ShellCommandSource::DoubleDashTail { start: 2 }
        );
        assert_eq!(
            split_window_shell_command(&tail).unwrap(),
            Some("make test".to_string())
        );

        let named = invocation("new-window -n build echo hi");
        assert_eq!(
            ShellCommandSource::for_new_window(&named.args),
            ShellCommandSource::PositionalWords
        );
        assert_eq!(
            new_window_shell_command(&named).unwrap(),
            Some("echo hi".to_string())
        );

        let unnamed = invocation("new-window work");
        assert_eq!(
            ShellCommandSource::for_new_window(&unnamed.args),
            ShellCommandSource::None
        );
        assert_eq!(new_window_shell_command(&unnamed).unwrap(), None);
        assert_eq!(new_window_name(&unnamed), "work");

        // A trailing `--shell-command` without a value is not the explicit
        // source, so the later rules still decide the command string.
        let dangling = invocation("new-window -n build --shell-command");
        assert_eq!(
            ShellCommandSource::for_new_window(&dangling.args),
            ShellCommandSource::PositionalWords
        );
        assert_eq!(new_window_shell_command(&dangling).unwrap(), None);
        assert_eq!(
            split_window_shell_command(&invocation("split-window --shell-command")).unwrap(),
            None
        );
        assert_eq!(
            new_window_name(&invocation("new-window --shell-command a b")),
            "shell"
        );

        let empty = invocation("split-window --command ''");
        assert!(split_window_shell_command(&empty).is_err());
    }
}
