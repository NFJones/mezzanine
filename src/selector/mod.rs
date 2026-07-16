//! Product selector candidate providers for command prompt surfaces.
//!
//! This module supplies Mezzanine and agent command catalogs, runtime values,
//! parameter hints, and filesystem candidates. Product-independent token
//! parsing, ranking, replacement, and active selection live in `mez-mux`.

use crate::command::baseline_commands;
use mez_agent::baseline_slash_commands;
use mez_mux::selector::{
    ActiveSelector, SelectorCandidate, SelectorCandidateKind, SelectorPlan, SelectorShadowHint,
    SelectorTokenContext, dedupe_selector_candidates, filter_and_sort_selector_candidates,
    selector_candidate_prefix_suffix, selector_token_context, unescape_selector_shell_token,
};
use std::fs;
use std::path::{Path, PathBuf};

mod api;
mod command_catalog;
mod filesystem;
mod parameters;

pub use api::{
    SelectorExtraCandidate, SelectorSurface, plan_selector, plan_selector_with_extra,
    plan_selector_with_extra_in_working_directory, shadow_hint, shadow_hint_with_extra,
    shadow_hint_with_extra_in_working_directory, start_active_selector,
    start_active_selector_with_extra_in_working_directory,
};
use command_catalog::{canonical_agent_command, selector_candidates};
use filesystem::path_candidates;
use parameters::{
    agent_parameter_hint, flag_candidates, mezzanine_parameter_hint, value_candidates,
};

#[cfg(test)]
mod tests;
