//! Typed command plans.
//!
//! Prefix commands arrive as parsed words and flags, but mutating session code
//! should not also own argument parsing and defaulting. This module converts the
//! state-changing command families into explicit plans before dispatch mutates
//! the session, keeping parsing errors and execution side effects separated.

use super::display::{new_window_name, new_window_shell_command, split_window_shell_command};
use super::shell::{flag_value, positional_args};
use super::{
    CommandInvocation, MezError, PaneNavigationDirection, PaneSizeSpec, ResizeAxis,
    ResizeDirection, Result, SplitDirection,
};

/// Identifies a parsed session-mutation command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CommandPlan {
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
    /// Approve an observer request.
    ApproveObserver(ObserverTargetPlan),
    /// Reject an observer request.
    RejectObserver(ObserverTargetPlan),
    /// Revoke observer access for a client.
    RevokeObserver(ObserverTargetPlan),
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
pub(super) struct PaneSpawningPlan {
    /// Original command name.
    pub(super) command: String,
    /// Effective window name.
    pub(super) name: String,
    /// Optional shell command to start in the new pane.
    pub(super) shell_command: Option<String>,
    /// Optional start directory for the pane process.
    pub(super) start_directory: Option<String>,
    /// Whether the created pane or window should be selected.
    pub(super) select: bool,
}

/// Parsed command data for commands with an optional target and required name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NamedTargetPlan {
    /// Original command name.
    pub(super) command: String,
    /// Optional target selector.
    pub(super) target: Option<String>,
    /// Required new name.
    pub(super) name: String,
}

/// Parsed command data for commands with a required target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TargetPlan {
    /// Original command name.
    pub(super) command: String,
    /// Required target selector.
    pub(super) target: String,
}

/// Parsed command data for commands with an optional target and force flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ForceTargetPlan {
    /// Original command name.
    pub(super) command: String,
    /// Optional target selector.
    pub(super) target: Option<String>,
    /// Whether force behavior was requested.
    pub(super) force: bool,
}

/// Parsed command data for moving a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MoveWindowPlan {
    /// Original command name.
    pub(super) command: String,
    /// Optional source window selector.
    pub(super) source: Option<String>,
    /// Destination window index.
    pub(super) target_index: usize,
}

/// Parsed command data for splitting a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SplitWindowPlan {
    /// Original command name.
    pub(super) command: String,
    /// Split direction.
    pub(super) direction: SplitDirection,
    /// Optional shell command to start in the new pane.
    pub(super) shell_command: Option<String>,
    /// Optional start directory for the new pane process.
    pub(super) start_directory: Option<String>,
    /// Whether the new pane should be selected.
    pub(super) select_new: bool,
}

/// Parsed pane-selection mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PaneSelectionPlan {
    /// Select by explicit target or alias.
    Target(String),
    /// Select by adjacent-pane direction.
    Direction(PaneNavigationDirection),
}

/// Parsed command data for selecting a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SelectPanePlan {
    /// Original command name.
    pub(super) command: String,
    /// Selection mode.
    pub(super) selection: PaneSelectionPlan,
}

/// Parsed command data for rotating panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RotatePanePlan {
    /// Original command name.
    pub(super) command: String,
    /// Whether the rotation direction is reversed.
    pub(super) reverse: bool,
}

/// Parsed command data for selecting a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LayoutPlan {
    /// Original command name.
    pub(super) command: String,
    /// Layout name.
    pub(super) layout_name: String,
}

/// Parsed resize-pane behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResizePanePlan {
    /// Toggle pane zoom instead of resizing.
    Zoom { command: String },
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
pub(crate) enum SwapPaneNeighbor {
    /// Previous pane in window order.
    Previous,
    /// Next pane in window order.
    Next,
}

/// Parsed swap-pane behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SwapPanePlan {
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
pub(crate) struct BreakPanePlan {
    /// Original command name.
    pub(crate) command: String,
    /// Optional source pane target.
    pub(crate) target: Option<String>,
    /// Optional new window name.
    pub(crate) name: Option<String>,
    /// Whether the new window should be selected.
    pub(crate) select: bool,
}

/// Parsed command data for join-pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JoinPanePlan {
    /// Original command name.
    pub(crate) command: String,
    /// Optional source pane selector.
    pub(crate) source: Option<String>,
    /// Destination pane selector.
    pub(crate) target: String,
    /// Join direction.
    pub(crate) direction: SplitDirection,
    /// Whether the joined pane should be selected.
    pub(crate) select: bool,
}

/// Typed runtime-owned pane layout commands.
///
/// These commands mutate session layout and must apply session-authored resize
/// effects through the product runtime rather than through generic dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimePaneLayoutPlan {
    /// Swap panes and apply the resulting resize effects.
    Swap(SwapPanePlan),
    /// Move a pane into a new window and apply the resulting resize effects.
    Break(BreakPanePlan),
    /// Move a pane into another window and apply the resulting resize effects.
    Join(JoinPanePlan),
}

/// Parsed command data for observer target mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ObserverTargetPlan {
    /// Original command name.
    pub(super) command: String,
    /// Observer request or client target.
    pub(super) target: String,
}

/// Parsed command data for rename-session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionNamePlan {
    /// Original command name.
    pub(super) command: String,
    /// New session name.
    pub(super) name: String,
}

/// Parsed command data for force-only commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ForceOnlyPlan {
    /// Original command name.
    pub(super) command: String,
    /// Whether force behavior was requested.
    pub(super) force: bool,
}

/// Parsed command data for detach-client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DetachClientPlan {
    /// Original command name.
    pub(super) command: String,
    /// Optional client target. Execution supplies the primary client default.
    pub(super) target: Option<String>,
}

/// Requested synchronization mode for the active window's panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SynchronizePanesMode {
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
pub(super) struct SynchronizePanesPlan {
    /// Original command name.
    pub(super) command: String,
    /// Requested synchronization mode.
    pub(super) mode: SynchronizePanesMode,
}

/// Converts one parsed command invocation into a typed session-mutation plan.
pub(super) fn command_plan_from_invocation(invocation: &CommandInvocation) -> Result<CommandPlan> {
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
        "approve-observer" => Ok(CommandPlan::ApproveObserver(observer_target_plan(
            invocation,
            "approve-observer requires a target",
        )?)),
        "reject-observer" => Ok(CommandPlan::RejectObserver(observer_target_plan(
            invocation,
            "reject-observer requires a target",
        )?)),
        "revoke-observer" => Ok(CommandPlan::RevokeObserver(observer_target_plan(
            invocation,
            "revoke-observer requires a client id",
        )?)),
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

/// Parses a layout command that requires runtime-owned resize synchronization.
pub(crate) fn runtime_pane_layout_plan_from_invocation(
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

fn observer_target_plan(
    invocation: &CommandInvocation,
    missing: &'static str,
) -> Result<ObserverTargetPlan> {
    let target = invocation
        .target_arg()
        .or_else(|| positional_args(invocation).first().copied())
        .ok_or_else(|| MezError::invalid_args(missing))?;
    Ok(ObserverTargetPlan {
        command: invocation.name.clone(),
        target: target.to_string(),
    })
}

fn synchronize_panes_plan(invocation: &CommandInvocation) -> Result<SynchronizePanesPlan> {
    let mode = match positional_args(invocation)
        .first()
        .copied()
        .unwrap_or("toggle")
    {
        "on" => SynchronizePanesMode::On,
        "off" => SynchronizePanesMode::Off,
        "toggle" => SynchronizePanesMode::Toggle,
        "status" => SynchronizePanesMode::Status,
        _ => {
            return Err(MezError::invalid_args(
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
    let name = positional_args(invocation).join(" ");
    if name.is_empty() {
        return Err(MezError::invalid_args(missing));
    }
    Ok(NamedTargetPlan {
        command: invocation.name.clone(),
        target: invocation.target_arg().map(ToOwned::to_owned),
        name,
    })
}

fn required_target_plan(
    invocation: &CommandInvocation,
    missing: &'static str,
) -> Result<TargetPlan> {
    let target = invocation
        .target_arg()
        .or_else(|| positional_args(invocation).first().copied())
        .ok_or_else(|| MezError::invalid_args(missing))?;
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
        .or_else(|| positional_args(invocation).first().copied())
    {
        PaneSelectionPlan::Target(target.to_string())
    } else if let Some(direction) = select_pane_direction(invocation)? {
        PaneSelectionPlan::Direction(direction)
    } else {
        return Err(MezError::invalid_args("select-pane requires a target"));
    };
    Ok(SelectPanePlan {
        command: invocation.name.clone(),
        selection,
    })
}

fn layout_plan(invocation: &CommandInvocation) -> Result<LayoutPlan> {
    let layout_name = positional_args(invocation)
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("select-layout requires a layout"))?;
    Ok(LayoutPlan {
        command: invocation.name.clone(),
        layout_name: layout_name.to_string(),
    })
}

fn resize_pane_plan(invocation: &CommandInvocation) -> Result<ResizePanePlan> {
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

fn swap_pane_plan(invocation: &CommandInvocation) -> Result<SwapPanePlan> {
    let command = invocation.name.clone();
    if let Some(target) = invocation
        .target_arg()
        .or_else(|| positional_args(invocation).first().copied())
    {
        return Ok(SwapPanePlan::Target {
            command,
            source: invocation.source_arg().map(ToOwned::to_owned),
            target: target.to_string(),
        });
    }
    if let Some(neighbor) = swap_pane_neighbor(invocation)? {
        if invocation.source_arg().is_some() {
            return Err(MezError::invalid_args(
                "swap-pane direction flags operate on the active pane",
            ));
        }
        return Ok(SwapPanePlan::Neighbor { command, neighbor });
    }
    Err(MezError::invalid_args("swap-pane requires a target"))
}

fn break_pane_plan(invocation: &CommandInvocation) -> BreakPanePlan {
    BreakPanePlan {
        command: invocation.name.clone(),
        target: invocation
            .target_arg()
            .or_else(|| positional_args(invocation).first().copied())
            .map(ToOwned::to_owned),
        name: flag_value(&invocation.args, "-n")
            .or_else(|| flag_value(&invocation.args, "--name"))
            .map(ToOwned::to_owned),
        select: !invocation.has_flag("-d", "--detached"),
    }
}

fn join_pane_plan(invocation: &CommandInvocation) -> Result<JoinPanePlan> {
    let target = invocation
        .target_arg()
        .or_else(|| positional_args(invocation).first().copied())
        .ok_or_else(|| MezError::invalid_args("join-pane requires a target"))?;
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
        select: invocation.args.iter().any(|arg| arg == "--select"),
    })
}

fn session_name_plan(
    invocation: &CommandInvocation,
    missing: &'static str,
) -> Result<SessionNamePlan> {
    let name = positional_args(invocation).join(" ");
    if name.is_empty() {
        return Err(MezError::invalid_args(missing));
    }
    Ok(SessionNamePlan {
        command: invocation.name.clone(),
        name,
    })
}

/// Builds the pane-size specification requested by `resize-pane`.
pub(crate) fn resize_spec_from_invocation(invocation: &CommandInvocation) -> Result<PaneSizeSpec> {
    if let Some(percent) = flag_value(&invocation.args, "--percent") {
        let percent = parse_resize_amount(percent, "resize-pane percent is invalid")?;
        let axis = match flag_value(&invocation.args, "--axis").unwrap_or("both") {
            "columns" | "horizontal" => ResizeAxis::Columns,
            "rows" | "vertical" => ResizeAxis::Rows,
            "both" => ResizeAxis::Both,
            _ => return Err(MezError::invalid_args("resize-pane axis is invalid")),
        };
        return Ok(PaneSizeSpec::Percent { percent, axis });
    }
    if let Some(direction) = flag_value(&invocation.args, "--delta") {
        let direction = ResizeDirection::from_name(direction)
            .ok_or_else(|| MezError::invalid_args("resize-pane delta direction is invalid"))?;
        return Ok(PaneSizeSpec::Delta {
            direction,
            amount: resize_amount_flag(invocation)?,
        });
    }
    if let Some(edge) = flag_value(&invocation.args, "--edge") {
        let edge = ResizeDirection::from_name(edge)
            .ok_or_else(|| MezError::invalid_args("resize-pane edge is invalid"))?;
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
        if invocation.args.iter().any(|arg| arg == flag) {
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
        return Err(MezError::invalid_args(
            "resize-pane requires a size, percent, delta, or edge",
        ));
    }
    Ok(PaneSizeSpec::Cells { columns, rows })
}

fn resize_amount_flag(invocation: &CommandInvocation) -> Result<u16> {
    flag_value(&invocation.args, "--amount")
        .map(|value| parse_resize_amount(value, "resize-pane amount is invalid"))
        .transpose()?
        .ok_or_else(|| MezError::invalid_args("resize-pane requires --amount"))
}

fn optional_flag_amount(args: &[String], flag: &str) -> Result<u16> {
    let Some(index) = args.iter().position(|arg| arg == flag) else {
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
        .map_err(|_| MezError::invalid_args(message))
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
            .any(|arg| arg == flag)
            .then_some(direction)
    });
    let direction = matched.next();
    if matched.next().is_some() {
        return Err(MezError::invalid_args(
            "select-pane accepts only one direction flag",
        ));
    }
    Ok(direction)
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
            .any(|arg| arg == flag)
            .then_some(neighbor)
    });
    let neighbor = matched.next();
    if matched.next().is_some() {
        return Err(MezError::invalid_args(
            "swap-pane accepts only one direction flag",
        ));
    }
    Ok(neighbor)
}

fn move_window_target_index(invocation: &CommandInvocation) -> Result<usize> {
    let target = invocation
        .target_arg()
        .or_else(|| positional_args(invocation).first().copied())
        .ok_or_else(|| MezError::invalid_args("move-window requires a target index"))?;
    target
        .parse::<usize>()
        .map_err(|_| MezError::invalid_args("move-window target must be a window index"))
}

/// Returns whether `split-window` should select the newly created pane.
pub(crate) fn split_window_selects_new_pane(invocation: &CommandInvocation) -> Result<bool> {
    let explicit_select = invocation.args.iter().any(|arg| arg == "--select");
    let detached = invocation.has_flag("-d", "--detached")
        || invocation.args.iter().any(|arg| arg == "--no-select");
    if explicit_select && detached {
        return Err(MezError::invalid_args(
            "split-window cannot combine --select with -d/--no-select",
        ));
    }
    Ok(!detached)
}
