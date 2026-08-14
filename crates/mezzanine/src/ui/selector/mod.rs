//! Product selector candidate providers for command prompt surfaces.
//!
//! This module supplies Mezzanine and agent command catalogs, runtime values,
//! parameter hints, and filesystem candidates. Product-independent token
//! parsing, ranking, replacement, and active selection live in `mez-mux`.

use crate::ui::command::baseline_commands;
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
    SelectorExtraCandidate, SelectorSurface, shadow_hint_with_extra_and_filesystem_candidates,
    start_active_selector_with_extra_and_filesystem_candidates,
};
#[cfg(test)]
pub use api::{
    plan_selector, plan_selector_with_extra, plan_selector_with_extra_in_working_directory,
    shadow_hint, shadow_hint_with_extra, start_active_selector,
};
#[cfg(test)]
use command_catalog::selector_candidates;
use command_catalog::{canonical_agent_command, selector_candidates_with_filesystem};
#[cfg(test)]
use filesystem::path_candidates;
pub use filesystem::record_browser_save_path_candidates;
pub use filesystem::{AsyncFilesystemSelectorCandidates, AsyncFilesystemSelectorSnapshot};
use parameters::{
    agent_parameter_hint, flag_candidates, mezzanine_parameter_hint, value_candidates,
};

#[cfg(test)]
mod tests;
