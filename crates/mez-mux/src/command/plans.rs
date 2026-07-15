//! Typed plans for pane-layout command families.
//!
//! This module converts parsed command invocations into dependency-neutral
//! resize, swap, break, join, and split-selection plans. It owns command
//! defaults and argument validation, but does not mutate sessions, spawn pane
//! processes, or project errors into the product error aggregate.

use super::{CommandInvocation, flag_value};
use crate::layout::{PaneSizeSpec, ResizeAxis, ResizeDirection, SplitDirection};
use crate::{MuxError, Result};

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

    /// Verifies percent resize parsing retains the requested axis and target
    /// before any session mutation occurs.
    #[test]
    fn parses_percent_resize_plan() {
        let plan =
            resize_pane_plan(&invocation("resize-pane -t 1 --percent 50 --axis rows")).unwrap();

        assert_eq!(
            plan,
            ResizePanePlan::Resize {
                command: "resize-pane".to_string(),
                target: Some("1".to_string()),
                spec: PaneSizeSpec::Percent {
                    percent: 50,
                    axis: ResizeAxis::Rows,
                },
            }
        );
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
}
