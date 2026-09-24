//! Typed classification of configured agent actions.
//!
//! Validation reports every issue, including duplicates; runtime readers use
//! the first issue and accept duplicates. Shape, omission, and error wording
//! remain owned by each caller rather than this pure classifier.

use mez_agent::{AllowedAction, AllowedActionSet};
use std::collections::BTreeSet;

/// One invalid action-list element, retaining its source spelling for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionListIssue<'a> {
    /// An element is not an action name string.
    NonString,
    /// The name is absent from the typed action catalog.
    Unknown(&'a str),
    /// The action is reserved for controller use.
    ControllerOnly(&'a str),
    /// A valid configurable action appeared earlier in this list.
    Duplicate(&'a str),
}

/// Classifies ordered names against the provider-visible action catalog.
///
/// `diagnose_duplicates` is true for aggregate config validation and false
/// for direct runtime readers, which historically accept repeated names.
/// Invalid elements do not prevent later elements from being classified.
pub(crate) fn classify_action_list<'a>(
    names: impl IntoIterator<Item = Option<&'a str>>,
    diagnose_duplicates: bool,
) -> (Vec<AllowedAction>, Vec<ActionListIssue<'a>>) {
    let configurable = AllowedActionSet::all_enabled();
    let mut actions = Vec::new();
    let mut issues = Vec::new();
    let mut seen = BTreeSet::new();
    for name in names {
        let Some(name) = name else {
            issues.push(ActionListIssue::NonString);
            continue;
        };
        let Some(action) = AllowedAction::from_action_type(name) else {
            issues.push(ActionListIssue::Unknown(name));
            continue;
        };
        if !configurable.contains(action) {
            issues.push(ActionListIssue::ControllerOnly(name));
        } else if diagnose_duplicates && !seen.insert(action) {
            issues.push(ActionListIssue::Duplicate(name));
        } else {
            actions.push(action);
        }
    }
    (actions, issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Validation diagnoses every malformed element in source order, while a
    /// runtime reader accepts duplicates and retains first-error ordering.
    #[test]
    fn action_lists_distinguish_validation_and_runtime_duplicates() {
        let names = [
            Some("say"),
            Some("say"),
            None,
            Some("unknown"),
            Some("request_capability"),
        ];
        let (validated, issues) = classify_action_list(names, true);
        assert_eq!(validated, [AllowedAction::Say]);
        assert_eq!(
            issues,
            [
                ActionListIssue::Duplicate("say"),
                ActionListIssue::NonString,
                ActionListIssue::Unknown("unknown"),
                ActionListIssue::ControllerOnly("request_capability"),
            ]
        );
        let (runtime, issues) = classify_action_list(names, false);
        assert_eq!(runtime, [AllowedAction::Say, AllowedAction::Say]);
        assert_eq!(
            issues,
            [
                ActionListIssue::NonString,
                ActionListIssue::Unknown("unknown"),
                ActionListIssue::ControllerOnly("request_capability")
            ]
        );
    }
}
