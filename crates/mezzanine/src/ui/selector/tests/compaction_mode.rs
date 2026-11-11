//! Parser-aligned discovery for compaction selection; completion never submits.

use super::*;

/// Runtime extras may be valid in ordinary prompts, but cannot widen a finite
/// slash-command grammar at first, scope, completed, or already-invalid slots.
#[test]
fn compaction_mode_selector_runtime_extras_cannot_bypass_finite_grammar() {
    let extras = [
        ("$", "$review"),
        ("@", "@github"),
        ("#", "#release"),
        ("compaction-mode", "unrelated"),
    ]
    .map(|(command, value)| {
        SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            command,
            SelectorCandidate::new(value, SelectorCandidateKind::Value, true),
        )
    });
    for preceding in [
        "",
        "summary ",
        "raw_tail ",
        "reset ",
        "status ",
        "summary --global ",
        "reset extra ",
        "--global ",
    ] {
        for prefix in ["$rev", "@git", "#rel", "unrel"] {
            let line = format!("/compaction-mode {preceding}{prefix}");
            assert!(
                plan_selector_with_extra(SelectorSurface::AgentCommand, &line, line.len(), &extras)
                    .is_none(),
                "invalid dynamic operand for {line}"
            );
            assert!(
                shadow_hint_with_extra(SelectorSurface::AgentCommand, &line, line.len(), &extras)
                    .is_none(),
                "invalid shadow for {line}"
            );
        }
    }
    for (line, expected) in [("/compaction-mode ", 4), ("/compaction-mode summary ", 1)] {
        let plan =
            plan_selector_with_extra(SelectorSurface::AgentCommand, line, line.len(), &extras)
                .unwrap();
        assert_eq!(plan.candidates.len(), expected);
    }
    for (prefix, value, suffix) in [
        ("$rev", "$review", "iew"),
        ("@git", "@github", "hub"),
        ("#rel", "#release", "ease"),
    ] {
        let line = format!("inspect with {prefix}");
        let plan =
            plan_selector_with_extra(SelectorSurface::AgentCommand, &line, line.len(), &extras)
                .unwrap();
        assert!(
            plan.candidates
                .iter()
                .any(|candidate| candidate.value == value)
        );
        assert_eq!(
            shadow_hint_with_extra(SelectorSurface::AgentCommand, &line, line.len(), &extras)
                .unwrap()
                .text,
            suffix
        );
    }
    let line = "/compaction-mo";
    assert!(
        plan_selector_with_extra(SelectorSurface::AgentCommand, line, line.len(), &extras)
            .unwrap()
            .candidates
            .iter()
            .any(|candidate| candidate.value == "/compaction-mode")
    );
}

#[test]
fn compaction_mode_selector_candidates_follow_argument_positions() {
    let line = "/compaction-mode ";
    let plan = plan_selector(SelectorSurface::AgentCommand, line, line.len()).unwrap();
    let values = plan
        .candidates
        .iter()
        .map(|candidate| candidate.value.as_str())
        .collect::<Vec<_>>();
    for operation in ["status", "summary", "raw_tail", "reset"] {
        assert!(values.contains(&operation), "{values:?}");
    }
    assert_eq!(values.len(), 4);
    for (prefix, expected) in [
        ("s", "status"),
        ("su", "summary"),
        ("r", "raw_tail"),
        ("raw_", "raw_tail"),
    ] {
        let line = format!("/compaction-mode {prefix}");
        let plan = plan_selector(SelectorSurface::AgentCommand, &line, line.len()).unwrap();
        assert!(
            plan.candidates
                .iter()
                .any(|candidate| candidate.value == expected)
        );
        assert!(
            plan.candidates
                .iter()
                .all(|candidate| candidate.kind == SelectorCandidateKind::Value)
        );
    }
    for operation in ["summary", "raw_tail", "reset"] {
        for prefix in ["", "--g"] {
            let line = format!("/compaction-mode {operation} {prefix}");
            let plan = plan_selector(SelectorSurface::AgentCommand, &line, line.len()).unwrap();
            assert_eq!(plan.candidates.len(), 1);
            assert_eq!(plan.candidates[0].value, "--global");
            assert_eq!(plan.candidates[0].kind, SelectorCandidateKind::Flag);
            let (applied, cursor) = apply_selector_candidate(&line, &plan, &plan.candidates[0]);
            assert_eq!(applied, format!("/compaction-mode {operation} --global "));
            assert_eq!(cursor, applied.len());
        }
    }
    for line in [
        "/compaction-mode status ",
        "/compaction-mode status --g",
        "/compaction-mode summary --global ",
        "/compaction-mode reset --global --g",
        "/compaction-mode --global ",
        "/compaction-mode --global summary ",
        "/compaction-mode summary extra ",
        "/compaction-mode unsupported ",
    ] {
        assert!(
            plan_selector(SelectorSurface::AgentCommand, line, line.len()).is_none(),
            "{line}"
        );
    }
}

#[test]
fn compaction_mode_selector_preserves_token_ranges_and_cycles() {
    let line = "/compaction-mode suZZ --global";
    let cursor = "/compaction-mode su".len();
    let plan = plan_selector(SelectorSurface::AgentCommand, line, cursor).unwrap();
    assert_eq!(
        (plan.replacement_start, plan.replacement_end),
        ("/compaction-mode ".len(), "/compaction-mode suZZ".len())
    );
    let candidate = plan
        .candidates
        .iter()
        .find(|candidate| candidate.value == "summary")
        .unwrap();
    let (applied, cursor) = apply_selector_candidate(line, &plan, candidate);
    assert_eq!(applied, "/compaction-mode summary --global");
    assert_eq!(cursor, "/compaction-mode summary".len());
    assert!(
        shadow_hint(
            SelectorSurface::AgentCommand,
            line,
            "/compaction-mode su".len()
        )
        .is_none()
    );
    let line = "/compaction-mode ";
    let mut selector =
        start_active_selector(SelectorSurface::AgentCommand, line, line.len(), false).unwrap();
    let first = selector.selected_line().unwrap();
    selector.select_next();
    assert_ne!(selector.selected_line().unwrap(), first);
    selector.select_previous();
    assert_eq!(selector.selected_line().unwrap(), first);
    selector.select_previous();
    let reverse =
        start_active_selector(SelectorSurface::AgentCommand, line, line.len(), true).unwrap();
    assert_eq!(selector.selected_line(), reverse.selected_line());
}

#[test]
fn compaction_mode_shadow_hints_are_optional_and_position_sensitive() {
    for line in [
        "/compaction-mode ",
        "/compaction-mode   ",
        "/compaction-mode\t",
    ] {
        let hint = shadow_hint(SelectorSurface::AgentCommand, line, line.len()).unwrap();
        assert_eq!(hint.text, " [status|summary|raw_tail|reset]");
        assert_eq!(hint.insert_at, line.len());
    }
    for operation in ["summary", "raw_tail", "reset"] {
        let line = format!("/compaction-mode {operation} ");
        assert_eq!(
            shadow_hint(SelectorSurface::AgentCommand, &line, line.len())
                .unwrap()
                .text,
            " [--global]"
        );
    }
    for (line, expected) in [
        ("/compaction-mode su", "mmary"),
        ("/compaction-mode raw_", "tail"),
        ("/compaction-mode summary --g", "lobal"),
    ] {
        assert_eq!(
            shadow_hint(SelectorSurface::AgentCommand, line, line.len())
                .unwrap()
                .text,
            expected
        );
    }
    for line in [
        "/compaction-mode status",
        "/compaction-mode status ",
        "/compaction-mode status --g",
        "/compaction-mode summary --global",
        "/compaction-mode summary --global ",
        "/compaction-mode --global ",
        "/compaction-mode reset extra ",
    ] {
        assert!(
            shadow_hint(SelectorSurface::AgentCommand, line, line.len()).is_none(),
            "{line}"
        );
    }
}

#[test]
fn compaction_mode_selector_never_treats_operands_as_paths() {
    // This module directory is real, but its entries are not legal operands.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui/selector");
    for line in [
        "/compaction-mode ./",
        "/compaction-mode summary ./",
        "/compaction-mode status ./",
    ] {
        assert!(
            plan_selector_with_extra_in_working_directory(
                SelectorSurface::AgentCommand,
                line,
                line.len(),
                &[],
                Some(&root)
            )
            .is_none(),
            "{line}"
        );
    }
    let stale_paths = [SelectorCandidate::new(
        "./not-an-operation",
        SelectorCandidateKind::Value,
        true,
    )];
    for line in ["/compaction-mode ./", "/compaction-mode status "] {
        assert!(
            crate::ui::selector::start_active_selector_with_extra_and_filesystem_candidates(
                SelectorSurface::AgentCommand,
                line,
                line.len(),
                false,
                &[],
                &stale_paths,
            )
            .is_none(),
            "stale snapshot must not supply operands for {line}"
        );
    }
}
