//! Product selector adapter tests.

use super::{
    SelectorCandidate, SelectorCandidateKind, SelectorExtraCandidate, SelectorSurface,
    plan_selector, plan_selector_with_extra, plan_selector_with_extra_in_working_directory,
    record_browser_save_path_candidates, shadow_hint, shadow_hint_with_extra,
    start_active_selector,
};
use mez_mux::selector::apply_selector_candidate;
use std::fs;
use std::sync::Mutex;

static CWD_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Verifies selector plans mezzanine command candidates from prefix.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn selector_plans_mezzanine_command_candidates_from_prefix() {
    let plan = plan_selector(SelectorSurface::MezzanineCommand, "new", 3).unwrap();

    assert_eq!(plan.replacement_start, 0);
    assert_eq!(plan.replacement_end, 3);
    assert_eq!(plan.candidates[0].value, "new-window");
    assert_eq!(plan.candidates[0].kind, SelectorCandidateKind::Command);
    assert!(
        plan.candidates
            .iter()
            .any(|candidate| candidate.value == "new-group"
                && candidate.kind == SelectorCandidateKind::Command)
    );
}

/// Verifies selector plans agent slash candidates from empty prompt.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn selector_plans_agent_slash_candidates_from_empty_prompt() {
    let plan = plan_selector(SelectorSurface::AgentCommand, "", 0).unwrap();

    assert!(plan.candidates.iter().any(|candidate| {
        candidate.value == "/help" && candidate.kind == SelectorCandidateKind::Command
    }));
}

/// Verifies selector plans mezzanine command argument candidates.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn selector_plans_mezzanine_command_argument_candidates() {
    let theme_plan = plan_selector(SelectorSurface::MezzanineCommand, "set-theme to", 12).unwrap();
    assert_eq!(theme_plan.replacement_start, "set-theme ".len());
    assert_eq!(theme_plan.candidates[0].value, "tokyo_night");
    assert_eq!(theme_plan.candidates[0].kind, SelectorCandidateKind::Value);
}

/// Verifies selector plans agent argument candidates.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn selector_plans_agent_argument_candidates() {
    let plan = plan_selector(SelectorSurface::AgentCommand, "/log-level de", 13).unwrap();

    assert_eq!(plan.candidates[0].value, "debug");

    let routing_plan = plan_selector(SelectorSurface::AgentCommand, "/routing t", 18).unwrap();
    assert_eq!(routing_plan.candidates[0].value, "toggle");

    let policy_plan = plan_selector(
        SelectorSurface::AgentCommand,
        "/routing policy s",
        "/routing policy s".len(),
    )
    .unwrap();
    assert_eq!(policy_plan.candidates[0].value, "subagent");

    let copy_plan = plan_selector(SelectorSurface::AgentCommand, "/copy c", 7).unwrap();
    assert_eq!(copy_plan.candidates[0].value, "clipboard");
}

/// Verifies selector plans filesystem path candidates for command
/// arguments in the Mezzanine and agent prompt surfaces.
#[test]
fn selector_plans_path_candidates_for_prompt_arguments() {
    let _guard = CWD_TEST_LOCK.lock().unwrap();
    let original = std::env::current_dir().unwrap();
    let root = std::env::temp_dir().join(format!("mez-selector-paths-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("fixtures")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("fixture.toml"), "value = true\n").unwrap();
    fs::write(root.join("src").join("selector.rs"), "// fixture\n").unwrap();
    std::env::set_current_dir(&root).unwrap();

    let command_plan = plan_selector(
        SelectorSurface::MezzanineCommand,
        "source-file fi",
        "source-file fi".len(),
    )
    .unwrap();
    let agent_plan = plan_selector(
        SelectorSurface::AgentCommand,
        "/list-mcp ./fi",
        "/list-mcp ./fi".len(),
    )
    .unwrap();
    let relative_agent_plan = plan_selector(
        SelectorSurface::AgentCommand,
        "inspect src/sel",
        "inspect src/sel".len(),
    )
    .unwrap();

    std::env::set_current_dir(original).unwrap();
    let _ = fs::remove_dir_all(&root);

    assert!(
        command_plan
            .candidates
            .iter()
            .any(|candidate| candidate.value == "fixture.toml")
    );
    assert!(
        command_plan
            .candidates
            .iter()
            .any(|candidate| candidate.value == "fixtures/")
    );
    assert!(
        agent_plan
            .candidates
            .iter()
            .any(|candidate| candidate.value == "./fixture.toml")
    );
    assert!(
        relative_agent_plan
            .candidates
            .iter()
            .any(|candidate| candidate.value == "src/selector.rs")
    );
}

/// Verifies prompt path completion can resolve relative paths from an
/// explicit pane working directory instead of the launcher process cwd.
#[test]
fn selector_plans_path_candidates_from_explicit_working_directory() {
    let _guard = CWD_TEST_LOCK.lock().unwrap();
    let original = std::env::current_dir().unwrap();
    let launch_root =
        std::env::temp_dir().join(format!("mez-selector-launch-{}", std::process::id()));
    let pane_root = std::env::temp_dir().join(format!("mez-selector-pane-{}", std::process::id()));
    let _ = fs::remove_dir_all(&launch_root);
    let _ = fs::remove_dir_all(&pane_root);
    fs::create_dir_all(&launch_root).unwrap();
    fs::create_dir_all(pane_root.join("src")).unwrap();
    fs::write(pane_root.join("fixture.toml"), "value = true\n").unwrap();
    fs::write(pane_root.join("src").join("selector.rs"), "// fixture\n").unwrap();
    std::env::set_current_dir(&launch_root).unwrap();

    let command_plan = plan_selector_with_extra_in_working_directory(
        SelectorSurface::MezzanineCommand,
        "source-file fi",
        "source-file fi".len(),
        &[],
        Some(pane_root.as_path()),
    )
    .unwrap();
    let agent_plan = plan_selector_with_extra_in_working_directory(
        SelectorSurface::AgentCommand,
        "inspect src/sel",
        "inspect src/sel".len(),
        &[],
        Some(pane_root.as_path()),
    )
    .unwrap();

    std::env::set_current_dir(original).unwrap();
    let _ = fs::remove_dir_all(&launch_root);
    let _ = fs::remove_dir_all(&pane_root);

    assert!(
        command_plan
            .candidates
            .iter()
            .any(|candidate| candidate.value == "fixture.toml")
    );
    assert!(
        agent_plan
            .candidates
            .iter()
            .any(|candidate| candidate.value == "src/selector.rs")
    );
}

/// Verifies record-browser Save completion uses the supplied pane directory
/// and keeps literal filesystem names intact.
///
/// Save paths go straight to filesystem APIs rather than a shell, so paths
/// containing spaces must remain unescaped. Hidden entries remain opt-in, and
/// directory candidates retain their continuation slash.
#[test]
fn record_browser_save_path_candidates_are_literal_and_pane_relative() {
    let root = std::env::temp_dir().join(format!(
        "mez-record-browser-save-paths-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("dir with spaces")).unwrap();
    fs::write(root.join("report file.md"), "report").unwrap();
    fs::write(root.join(".hidden.md"), "hidden").unwrap();

    let visible = record_browser_save_path_candidates("", Some(root.as_path()));
    let hidden = record_browser_save_path_candidates(".", Some(root.as_path()));
    let _ = fs::remove_dir_all(&root);

    let visible = visible
        .into_iter()
        .map(|candidate| candidate.value)
        .collect::<Vec<_>>();
    let hidden = hidden
        .into_iter()
        .map(|candidate| candidate.value)
        .collect::<Vec<_>>();
    assert_eq!(visible, vec!["dir with spaces/", "report file.md"]);
    assert_eq!(hidden, vec![".hidden.md"]);
}

/// Verifies first-token agent shell input still plans filesystem
/// completions when the user starts with a likely relative path instead of
/// a slash command.
#[test]
fn selector_plans_agent_root_path_candidates_for_first_token() {
    let _guard = CWD_TEST_LOCK.lock().unwrap();
    let original = std::env::current_dir().unwrap();
    let root = std::env::temp_dir().join(format!("mez-selector-root-paths-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    std::env::set_current_dir(&root).unwrap();

    let plan = plan_selector(SelectorSurface::AgentCommand, "sr", 2).unwrap();

    std::env::set_current_dir(original).unwrap();
    let _ = fs::remove_dir_all(&root);

    assert!(
        plan.candidates
            .iter()
            .any(|candidate| candidate.value == "src/")
    );
}

/// Verifies incomplete directory components stay breadth-first so a stray
/// slash after a partial directory name still suggests that directory
/// instead of trying to recurse into a non-existent path.
#[test]
fn selector_plans_breadth_first_candidates_for_incomplete_directory_components() {
    let _guard = CWD_TEST_LOCK.lock().unwrap();
    let original = std::env::current_dir().unwrap();
    let root =
        std::env::temp_dir().join(format!("mez-selector-breadth-first-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    std::env::set_current_dir(&root).unwrap();

    let plan = plan_selector(SelectorSurface::AgentCommand, "sr/", 3).unwrap();

    std::env::set_current_dir(original).unwrap();
    let _ = fs::remove_dir_all(&root);

    assert!(
        plan.candidates
            .iter()
            .any(|candidate| candidate.value == "src/")
    );
}

/// Verifies continued agent path completion still works after selecting a
/// directory whose name contains spaces.
///
/// The selector must escape inserted spaced path components and map those
/// escaped components back to real filesystem names for subsequent lookup,
/// or the next completion splits the path into multiple tokens and stops.
#[test]
fn selector_continues_agent_path_completion_inside_directory_with_spaces() {
    let _guard = CWD_TEST_LOCK.lock().unwrap();
    let original = std::env::current_dir().unwrap();
    let root =
        std::env::temp_dir().join(format!("mez-selector-spaced-paths-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("dir with spaces").join("subdir")).unwrap();
    std::env::set_current_dir(&root).unwrap();

    let first_plan = plan_selector(
        SelectorSurface::AgentCommand,
        "inspect ./dir",
        "inspect ./dir".len(),
    )
    .unwrap();
    let directory_candidate = first_plan
        .candidates
        .iter()
        .find(|candidate| candidate.value == "./dir\\ with\\ spaces/")
        .unwrap()
        .clone();
    let (selected_line, selected_cursor) =
        apply_selector_candidate("inspect ./dir", &first_plan, &directory_candidate);
    let second_plan = plan_selector(
        SelectorSurface::AgentCommand,
        &selected_line,
        selected_cursor,
    )
    .unwrap();

    std::env::set_current_dir(original).unwrap();
    let _ = fs::remove_dir_all(&root);

    assert_eq!(selected_line, "inspect ./dir\\ with\\ spaces/");
    assert_eq!(selected_cursor, selected_line.len());
    assert!(
        second_plan
            .candidates
            .iter()
            .any(|candidate| candidate.value == "./dir\\ with\\ spaces/subdir/")
    );
}

/// Verifies bare-tilde agent path queries expand against the caller home
/// directory instead of trying to match a literal `~` filename.
#[test]
fn selector_plans_agent_path_candidates_for_bare_tilde() {
    let _guard = CWD_TEST_LOCK.lock().unwrap();
    let home_root = std::env::temp_dir().join(format!("mez-selector-home-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home_root);
    fs::create_dir_all(home_root.join("notes")).unwrap();
    fs::write(home_root.join("notes.txt"), "remember me\n").unwrap();
    let original_home = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("HOME", &home_root);
    }

    let plan = plan_selector(
        SelectorSurface::AgentCommand,
        "inspect ~",
        "inspect ~".len(),
    )
    .unwrap();

    match original_home {
        Some(home) => unsafe {
            std::env::set_var("HOME", home);
        },
        None => unsafe {
            std::env::remove_var("HOME");
        },
    }
    let _ = fs::remove_dir_all(&home_root);

    assert!(
        plan.candidates
            .iter()
            .any(|candidate| candidate.value == "~/notes/")
    );
    assert!(
        plan.candidates
            .iter()
            .any(|candidate| candidate.value == "~/notes.txt")
    );
}

/// Verifies dynamic agent argument candidates are scoped to their command.
#[test]
fn selector_plans_dynamic_agent_resume_candidates() {
    let extra = vec![SelectorExtraCandidate::new(
        SelectorSurface::AgentCommand,
        "resume",
        SelectorCandidate::new(
            "018f6b3a-1b2c-7000-9000-cafebabefeed",
            SelectorCandidateKind::Value,
            true,
        ),
    )];

    let plan = plan_selector_with_extra(
        SelectorSurface::AgentCommand,
        "/resume 018f",
        "/resume 018f".len(),
        &extra,
    )
    .unwrap();
    let model_plan = plan_selector_with_extra(
        SelectorSurface::AgentCommand,
        "/model 018f",
        "/model 018f".len(),
        &extra,
    );

    assert_eq!(
        plan.candidates[0].value,
        "018f6b3a-1b2c-7000-9000-cafebabefeed"
    );
    assert!(model_plan.is_none());
}

/// Verifies explicit skill syntax can use runtime-provided `$skill`
/// candidates at the agent prompt root.
#[test]
fn selector_plans_dynamic_agent_skill_candidates() {
    let extra = vec![SelectorExtraCandidate::new(
        SelectorSurface::AgentCommand,
        "$",
        SelectorCandidate::new("$openai-docs", SelectorCandidateKind::Value, true),
    )];

    let plan = plan_selector_with_extra(
        SelectorSurface::AgentCommand,
        "$open",
        "$open".len(),
        &extra,
    )
    .unwrap();

    assert_eq!(plan.candidates[0].value, "$openai-docs");
}

/// Verifies explicit macro syntax uses runtime-provided `#macro` candidates
/// at the agent prompt root without mixing with skill or MCP namespaces.
#[test]
fn selector_plans_dynamic_agent_macro_candidates() {
    let extra = vec![
        SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "#",
            SelectorCandidate::new("#release-check", SelectorCandidateKind::Value, true),
        ),
        SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "$",
            SelectorCandidate::new("$release-check", SelectorCandidateKind::Value, true),
        ),
    ];

    let plan =
        plan_selector_with_extra(SelectorSurface::AgentCommand, "#rel", "#rel".len(), &extra)
            .unwrap();

    assert_eq!(plan.candidates[0].value, "#release-check");
    assert!(
        plan.candidates
            .iter()
            .all(|candidate| !candidate.value.starts_with("$"))
    );
}

/// Verifies explicit skill syntax can complete at any prompt position and
/// after earlier skill tokens.
#[test]
fn selector_plans_dynamic_agent_skill_candidates_anywhere_in_prompt() {
    let extra = vec![SelectorExtraCandidate::new(
        SelectorSurface::AgentCommand,
        "$",
        SelectorCandidate::new("$openai-docs", SelectorCandidateKind::Value, true),
    )];

    let middle_plan = plan_selector_with_extra(
        SelectorSurface::AgentCommand,
        "please use $open",
        "please use $open".len(),
        &extra,
    )
    .unwrap();
    let repeated_plan = plan_selector_with_extra(
        SelectorSurface::AgentCommand,
        "$review first then $open",
        "$review first then $open".len(),
        &extra,
    )
    .unwrap();

    assert_eq!(middle_plan.candidates[0].value, "$openai-docs");
    assert_eq!(repeated_plan.candidates[0].value, "$openai-docs");

    let (line, cursor) =
        apply_selector_candidate("please use $open", &middle_plan, &middle_plan.candidates[0]);
    assert_eq!(line, "please use $openai-docs ");
    assert_eq!(cursor, line.len());
}

/// Verifies explicit MCP server syntax can use runtime-provided `@server`
/// candidates without entering the slash-command or skill completion domains.
///
/// This keeps prompt-local MCP discovery aligned with submitted `@server`
/// invocation syntax while preserving `$skill` completion as a separate
/// selector namespace.
#[test]
fn selector_plans_dynamic_agent_mcp_server_candidates() {
    let extra = vec![
        SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "@",
            SelectorCandidate::new("@github", SelectorCandidateKind::Value, true),
        ),
        SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "$",
            SelectorCandidate::new("$github", SelectorCandidateKind::Value, true),
        ),
    ];

    let plan = plan_selector_with_extra(
        SelectorSurface::AgentCommand,
        "please ask @git",
        "please ask @git".len(),
        &extra,
    )
    .unwrap();
    let skill_plan = plan_selector_with_extra(
        SelectorSurface::AgentCommand,
        "please ask $git",
        "please ask $git".len(),
        &extra,
    )
    .unwrap();

    assert_eq!(plan.candidates[0].value, "@github");
    assert!(
        !plan
            .candidates
            .iter()
            .any(|candidate| candidate.value == "$github")
    );
    assert_eq!(skill_plan.candidates[0].value, "$github");
    assert!(
        !skill_plan
            .candidates
            .iter()
            .any(|candidate| candidate.value == "@github")
    );

    let (line, cursor) = apply_selector_candidate("please ask @git", &plan, &plan.candidates[0]);
    assert_eq!(line, "please ask @github ");
    assert_eq!(cursor, line.len());
}

/// Verifies auto-completed directory candidates keep cycling sibling
/// matches until the user explicitly types more path input.
#[test]
fn active_selector_keeps_cycling_after_implicit_directory_selection() {
    let _guard = CWD_TEST_LOCK.lock().unwrap();
    let original = std::env::current_dir().unwrap();
    let root = std::env::temp_dir().join(format!("mez-selector-refresh-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    std::env::set_current_dir(&root).unwrap();

    let selector = start_active_selector(
        SelectorSurface::AgentCommand,
        "/list-mcp ./sr",
        "/list-mcp ./sr".len(),
        false,
    )
    .unwrap();
    let (line, cursor) = selector.selected_line().unwrap();

    std::env::set_current_dir(original).unwrap();
    let _ = fs::remove_dir_all(&root);

    assert_eq!(line, "/list-mcp ./src/");
    assert!(!selector.should_refresh_from_selected_directory(&line, cursor));
}

/// Verifies an explicit trailing slash on the typed query refreshes into
/// the selected directory on the next Tab press.
#[test]
fn active_selector_refreshes_after_explicit_directory_selection() {
    let _guard = CWD_TEST_LOCK.lock().unwrap();
    let original = std::env::current_dir().unwrap();
    let root = std::env::temp_dir().join(format!("mez-selector-refresh-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    std::env::set_current_dir(&root).unwrap();

    let selector = start_active_selector(
        SelectorSurface::AgentCommand,
        "/list-mcp ./sr/",
        "/list-mcp ./sr/".len(),
        false,
    )
    .unwrap();
    let (line, cursor) = selector.selected_line().unwrap();

    std::env::set_current_dir(original).unwrap();
    let _ = fs::remove_dir_all(&root);

    assert_eq!(line, "/list-mcp ./src/");
    assert!(selector.should_refresh_from_selected_directory(&line, cursor));
}

/// Verifies non-directory selections continue cycling within the active
/// candidate set instead of forcing a fresh selector.
#[test]
fn active_selector_keeps_argument_candidate_selection_active() {
    let selector = start_active_selector(
        SelectorSurface::MezzanineCommand,
        "set-theme to",
        "set-theme to".len(),
        false,
    )
    .unwrap();
    let (line, cursor) = selector.selected_line().unwrap();

    assert_eq!(line, "set-theme tokyo_night ");
    assert!(!selector.should_refresh_from_selected_directory(&line, cursor));
}

/// Verifies that non-mutating command-name shadow hints reuse selector
/// candidates without inserting text into the prompt buffer.
#[test]
fn selector_shadow_hint_completes_mezzanine_command_prefix() {
    let hint = shadow_hint(SelectorSurface::MezzanineCommand, "new", 3).unwrap();

    assert_eq!(hint.insert_at, 3);
    assert_eq!(hint.text, "-window");
    assert_eq!(hint.kind, SelectorCandidateKind::Command);
}

/// Verifies that commands with known arguments show a placeholder only until
/// the user starts typing an argument value.
#[test]
fn selector_shadow_hint_hides_placeholder_after_param_input() {
    let placeholder = shadow_hint(
        SelectorSurface::MezzanineCommand,
        "save-layout ",
        "save-layout ".len(),
    )
    .unwrap();
    let value_suffix = shadow_hint(
        SelectorSurface::MezzanineCommand,
        "set-theme to",
        "set-theme to".len(),
    )
    .unwrap();

    assert_eq!(placeholder.text, " [--name name]");
    assert_eq!(value_suffix.text, "kyo_night");

    let theme_placeholder = shadow_hint(
        SelectorSurface::MezzanineCommand,
        "set-theme ",
        "set-theme ".len(),
    )
    .unwrap();
    assert_eq!(theme_placeholder.text, " <theme>");

    let rename_session_placeholder = shadow_hint(
        SelectorSurface::MezzanineCommand,
        "rename-session ",
        "rename-session ".len(),
    )
    .unwrap();
    assert_eq!(rename_session_placeholder.text, " <name>");
}

/// Verifies that agent slash commands expose the same prefix-completion
/// shadow hints as the Mezzanine command prompt.
#[test]
fn selector_shadow_hint_completes_agent_slash_prefix() {
    let hint = shadow_hint(SelectorSurface::AgentCommand, "/log", 4).unwrap();

    assert_eq!(hint.insert_at, 4);
    assert_eq!(hint.text, "-level");
    assert_eq!(hint.kind, SelectorCandidateKind::Command);
}

/// Verifies `/tool` completes the canonical typed-toolchain command before
/// argument-specific completion takes over.
#[test]
fn selector_shadow_hint_completes_toolchain_command_name() {
    let hint = shadow_hint(SelectorSurface::AgentCommand, "/tool", 5).unwrap();

    assert_eq!(hint.text, "chain");
    assert_eq!(hint.kind, SelectorCandidateKind::Command);
}

/// Verifies `/toolchain` completion follows its strict grammar from operation
/// through typed Rust, Zig, Go, Deno, Bun, Node.js, Python, or JDK selection and confirmation.
#[test]
fn selector_shadow_hint_completes_toolchain_grammar() {
    let cases = [
        ("/toolchain sta", "tus", SelectorCandidateKind::Value),
        ("/toolchain detect r", "ust", SelectorCandidateKind::Value),
        ("/toolchain detect z", "ig", SelectorCandidateKind::Value),
        ("/toolchain detect g", "o", SelectorCandidateKind::Value),
        ("/toolchain detect d", "eno", SelectorCandidateKind::Value),
        ("/toolchain detect b", "un", SelectorCandidateKind::Value),
        ("/toolchain detect n", "ode", SelectorCandidateKind::Value),
        ("/toolchain detect p", "ython", SelectorCandidateKind::Value),
        ("/toolchain detect j", "dk", SelectorCandidateKind::Value),
        ("/toolchain enable r", "ust", SelectorCandidateKind::Value),
        ("/toolchain enable z", "ig", SelectorCandidateKind::Value),
        ("/toolchain enable g", "o", SelectorCandidateKind::Value),
        ("/toolchain enable d", "eno", SelectorCandidateKind::Value),
        ("/toolchain enable b", "un", SelectorCandidateKind::Value),
        ("/toolchain enable n", "ode", SelectorCandidateKind::Value),
        ("/toolchain enable p", "ython", SelectorCandidateKind::Value),
        ("/toolchain enable j", "dk", SelectorCandidateKind::Value),
        ("/toolchain disable r", "ust", SelectorCandidateKind::Value),
        (
            "/toolchain enable rust --y",
            "es",
            SelectorCandidateKind::Flag,
        ),
        (
            "/toolchain disable rust --y",
            "es",
            SelectorCandidateKind::Flag,
        ),
        (
            "/toolchain enable zig --y",
            "es",
            SelectorCandidateKind::Flag,
        ),
        (
            "/toolchain enable go --y",
            "es",
            SelectorCandidateKind::Flag,
        ),
        (
            "/toolchain enable deno --y",
            "es",
            SelectorCandidateKind::Flag,
        ),
        (
            "/toolchain enable bun --y",
            "es",
            SelectorCandidateKind::Flag,
        ),
        (
            "/toolchain enable node --y",
            "es",
            SelectorCandidateKind::Flag,
        ),
        (
            "/toolchain enable python --y",
            "es",
            SelectorCandidateKind::Flag,
        ),
        (
            "/toolchain enable jdk --y",
            "es",
            SelectorCandidateKind::Flag,
        ),
    ];

    for (line, expected_text, expected_kind) in cases {
        let hint = shadow_hint(SelectorSurface::AgentCommand, line, line.len()).unwrap();
        assert_eq!(hint.text, expected_text, "completion for {line}");
        assert_eq!(hint.kind, expected_kind, "candidate kind for {line}");
    }
}

/// Verifies custom identities are offered only in parser-supported selector
/// positions and never expose their configured host roots.
#[test]
fn selector_completes_dynamic_custom_toolchains_in_valid_positions() {
    let extra = ["status", "detect", "enable", "disable", "remove"]
        .into_iter()
        .map(|subcommand| {
            SelectorExtraCandidate::after_subcommand(
                SelectorSurface::AgentCommand,
                "toolchain",
                subcommand,
                2,
                matches!(subcommand, "status" | "detect" | "remove").then_some(2),
                matches!(subcommand, "enable" | "disable").then_some("--yes"),
                SelectorCandidate::new("custom:acme", SelectorCandidateKind::Value, true),
            )
        })
        .collect::<Vec<_>>();

    for line in [
        "/toolchain status custom:a",
        "/toolchain detect custom:a",
        "/toolchain enable custom:a",
        "/toolchain disable custom:a",
        "/toolchain remove custom:a",
    ] {
        let plan =
            plan_selector_with_extra(SelectorSurface::AgentCommand, line, line.len(), &extra)
                .unwrap();
        assert_eq!(plan.candidates[0].value, "custom:acme", "{line}");
        assert_eq!(plan.candidates[0].detail, None);
        assert!(
            plan.candidates
                .iter()
                .all(|candidate| !candidate.value.contains("/private")),
            "{line}"
        );
    }

    assert!(
        plan_selector_with_extra(
            SelectorSurface::AgentCommand,
            "/toolchain list custom:a",
            "/toolchain list custom:a".len(),
            &extra,
        )
        .is_none()
    );
    assert!(
        plan_selector_with_extra(
            SelectorSurface::AgentCommand,
            "/toolchain enable custom:acme custom:a",
            "/toolchain enable custom:acme custom:a".len(),
            &extra,
        )
        .is_none()
    );
    assert!(
        plan_selector_with_extra(
            SelectorSurface::AgentCommand,
            "/toolchain enable custom:acme --yes custom:a",
            "/toolchain enable custom:acme --yes custom:a".len(),
            &extra,
        )
        .is_none()
    );
}

/// Verifies toolchain completion never inspects explicit host paths, including
/// custom-definition root value slots that intentionally accept path text.
#[test]
fn selector_does_not_probe_filesystem_for_toolchain_arguments() {
    let directory = std::env::temp_dir().join(format!(
        "mez-toolchain-selector-no-probe-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("private-sdk"), b"secret").unwrap();
    let query = format!("/toolchain define acme --root {}/pri", directory.display());

    let plan = plan_selector_with_extra_in_working_directory(
        SelectorSurface::AgentCommand,
        &query,
        query.len(),
        &[],
        Some(&directory),
    );

    assert!(plan.is_none());
    fs::remove_dir_all(directory).unwrap();
}

/// Verifies definition and removal completion mirrors repeatable and
/// singleton flags from the strict parser without suggesting values as flags.
#[test]
fn selector_completes_custom_toolchain_flags_by_position() {
    let define = plan_selector(
        SelectorSurface::AgentCommand,
        "/toolchain define acme --",
        "/toolchain define acme --".len(),
    )
    .unwrap();
    for flag in [
        "--root",
        "--path",
        "--require",
        "--env-root",
        "--description",
        "--yes",
    ] {
        assert!(
            define
                .candidates
                .iter()
                .any(|candidate| candidate.value == flag),
            "missing {flag}"
        );
    }

    let after_singletons = plan_selector(
        SelectorSurface::AgentCommand,
        "/toolchain define acme --description SDK --yes --",
        "/toolchain define acme --description SDK --yes --".len(),
    )
    .unwrap();
    assert!(
        after_singletons
            .candidates
            .iter()
            .all(|candidate| !matches!(candidate.value.as_str(), "--description" | "--yes"))
    );
    assert!(
        plan_selector(
            SelectorSurface::AgentCommand,
            "/toolchain define acme --root /",
            "/toolchain define acme --root /".len(),
        )
        .is_none()
    );

    let remove = plan_selector(
        SelectorSurface::AgentCommand,
        "/toolchain remove custom:acme --",
        "/toolchain remove custom:acme --".len(),
    )
    .unwrap();
    let remove_flags = remove
        .candidates
        .iter()
        .map(|candidate| candidate.value.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        remove_flags,
        std::collections::BTreeSet::from(["--disable", "--yes"])
    );
}

/// Verifies empty-token shadow text follows every parser-supported custom
/// definition value slot and suppresses hints after terminal confirmation.
#[test]
fn selector_hints_custom_toolchain_value_positions() {
    for (line, expected) in [
        ("/toolchain status ", " [SELECTOR]"),
        ("/toolchain enable ", " <SELECTOR...> --yes"),
        (
            "/toolchain define ",
            " <NAME> --root <PATH> --path <REF> [--require <REF>] [--env-root <NAME=REF>] [--description <TEXT>] --yes",
        ),
        ("/toolchain define acme --root ", " <absolute-path>"),
        (
            "/toolchain define acme --path ",
            " <root-index:relative-path>",
        ),
        (
            "/toolchain define acme --env-root ",
            " <NAME=root-index:relative-path>",
        ),
        ("/toolchain define acme --description ", " <text>"),
        ("/toolchain remove ", " <custom:NAME> [--disable] --yes"),
        ("/toolchain remove custom:acme --disable ", " --yes"),
    ] {
        let hint = shadow_hint(SelectorSurface::AgentCommand, line, line.len()).unwrap();
        assert_eq!(hint.text, expected, "{line}");
    }

    for line in [
        "/toolchain list ",
        "/toolchain reload ",
        "/toolchain remove custom:acme --disable --yes ",
        "/toolchain define acme --root /opt/acme --path 0:bin --yes ",
    ] {
        assert!(
            shadow_hint(SelectorSurface::AgentCommand, line, line.len()).is_none(),
            "unexpected hint for {line}"
        );
    }
}

/// Verifies dynamic argument candidates can provide shadow completion text.
#[test]
fn selector_shadow_hint_completes_dynamic_resume_candidate() {
    let extra = vec![SelectorExtraCandidate::new(
        SelectorSurface::AgentCommand,
        "resume",
        SelectorCandidate::new(
            "018f6b3a-1b2c-7000-9000-cafebabefeed",
            SelectorCandidateKind::Value,
            true,
        ),
    )];

    let hint = shadow_hint_with_extra(
        SelectorSurface::AgentCommand,
        "/resume 018f",
        "/resume 018f".len(),
        &extra,
    )
    .unwrap();

    assert_eq!(hint.insert_at, "/resume 018f".len());
    assert_eq!(hint.text, "6b3a-1b2c-7000-9000-cafebabefeed");
}

/// Verifies agent slash-command placeholders enumerate the documented
/// first-slot options for commands with static selector candidates.
///
/// These hints are maintained separately from candidate lists, so this
/// regression coverage keeps shadow text aligned with the first argument
/// values users can discover through completion.
#[test]
fn selector_shadow_hint_covers_static_agent_first_slot_options() {
    let loop_hint = shadow_hint(SelectorSurface::AgentCommand, "/loop ", "/loop ".len()).unwrap();
    let latency_hint = shadow_hint(
        SelectorSurface::AgentCommand,
        "/latency ",
        "/latency ".len(),
    )
    .unwrap();
    let trust_hint =
        shadow_hint(SelectorSurface::AgentCommand, "/trust ", "/trust ".len()).unwrap();
    let personality_hint = shadow_hint(
        SelectorSurface::AgentCommand,
        "/personality ",
        "/personality ".len(),
    )
    .unwrap();
    let routing_hint = shadow_hint(
        SelectorSurface::AgentCommand,
        "/routing ",
        "/routing ".len(),
    )
    .unwrap();
    let toolchain_hint = shadow_hint(
        SelectorSurface::AgentCommand,
        "/toolchain ",
        "/toolchain ".len(),
    )
    .unwrap();

    assert_eq!(loop_hint.text, " [--fork|--new] [--limit <int>] <prompt>");
    assert_eq!(latency_hint.text, " <slow|default|fast>");
    assert_eq!(trust_hint.text, " <project-root|latest|list|pending>");
    assert_eq!(routing_hint.text, " <on|off|toggle|status|policy>");
    assert_eq!(
        toolchain_hint.text,
        " <status|list|detect|define|enable|disable|remove|reload>"
    );
    assert_eq!(
        personality_hint.text,
        " <profile|style|list|status|show|clear|default>"
    );
}

/// Verifies `/loop` flag completions surface the documented iteration-mode
/// and limit options as transient shadow text before users accept a
/// selector candidate.
#[test]
fn selector_shadow_hint_completes_loop_flags() {
    let fork_hint = shadow_hint(
        SelectorSurface::AgentCommand,
        "/loop --f",
        "/loop --f".len(),
    )
    .unwrap();
    let new_hint = shadow_hint(
        SelectorSurface::AgentCommand,
        "/loop --n",
        "/loop --n".len(),
    )
    .unwrap();
    let limit_hint = shadow_hint(
        SelectorSurface::AgentCommand,
        "/loop --l",
        "/loop --l".len(),
    )
    .unwrap();

    assert_eq!(fork_hint.insert_at, "/loop --f".len());
    assert_eq!(fork_hint.text, "ork");
    assert_eq!(fork_hint.kind, SelectorCandidateKind::Flag);
    assert_eq!(new_hint.insert_at, "/loop --n".len());
    assert_eq!(new_hint.text, "ew");
    assert_eq!(new_hint.kind, SelectorCandidateKind::Flag);
    assert_eq!(limit_hint.insert_at, "/loop --l".len());
    assert_eq!(limit_hint.text, "imit");
    assert_eq!(limit_hint.kind, SelectorCandidateKind::Flag);
}

/// Verifies argument-bearing slash commands expose parameter shadow hints
/// so users can discover their accepted values without opening help.
#[test]
fn selector_shadow_hint_covers_argument_bearing_agent_commands() {
    let cases = [
        ("/directive ", " <status|show|clear|default|none|text>"),
        ("/memory ", " <on|off|toggle|status|show>"),
        ("/remember ", " [statement]"),
        ("/fork ", " [conversation-id]"),
        ("/debug-config ", " [filter]"),
    ];

    for (line, expected) in cases {
        let hint = shadow_hint(SelectorSurface::AgentCommand, line, line.len()).unwrap();
        assert_eq!(hint.text, expected, "hint for {line}");
    }
}

/// Verifies static slash-command argument completions cover commands whose
/// first slot is constrained by their parser or documented mode set.
#[test]
fn selector_shadow_hint_completes_additional_agent_command_values() {
    let cases = [
        ("/directive cl", "ear", SelectorCandidateKind::Value),
        ("/memory to", "ggle", SelectorCandidateKind::Value),
        (
            "/debug-config mc",
            "p_servers",
            SelectorCandidateKind::Value,
        ),
    ];

    for (line, expected_text, expected_kind) in cases {
        let hint = shadow_hint(SelectorSurface::AgentCommand, line, line.len()).unwrap();
        assert_eq!(hint.text, expected_text, "completion for {line}");
        assert_eq!(hint.kind, expected_kind, "candidate kind for {line}");
    }
}

/// Verifies the persistent host-execution candidate carries conspicuous
/// warning copy that distinguishes it from sandboxed full access.
#[test]
fn selector_host_access_candidate_warns_about_host_execution() {
    let plan = plan_selector(
        SelectorSurface::AgentCommand,
        "/approval host",
        "/approval host".len(),
    )
    .unwrap();
    let candidate = plan
        .candidates
        .iter()
        .find(|candidate| candidate.value == "host-access")
        .unwrap();

    assert_eq!(
        candidate.detail.as_deref(),
        Some("No prompts; execute on host outside configured sandbox.")
    );
}

/// Verifies `/routing policy` exposes nested policy values through Tab
/// completion and transient prompt shadow hints.
#[test]
fn selector_shadow_hint_completes_routing_policy_values() {
    let policy_hint = shadow_hint(
        SelectorSurface::AgentCommand,
        "/routing policy ",
        "/routing policy ".len(),
    )
    .unwrap();
    let subagent_hint = shadow_hint(
        SelectorSurface::AgentCommand,
        "/routing policy s",
        "/routing policy s".len(),
    )
    .unwrap();
    let in_place_hint = shadow_hint(
        SelectorSurface::AgentCommand,
        "/routing policy i",
        "/routing policy i".len(),
    )
    .unwrap();

    assert_eq!(policy_hint.text, " <subagent|in-place>");
    assert_eq!(subagent_hint.text, "ubagent");
    assert_eq!(in_place_hint.text, "n-place");
    assert_eq!(subagent_hint.kind, SelectorCandidateKind::Value);
    assert_eq!(in_place_hint.kind, SelectorCandidateKind::Value);
}

/// Verifies commands without first-slot enumerated arguments do not expose
/// stale selector candidates from neighboring command metadata.
///
/// `rename-session` accepts a free-form name and `list-themes` takes no
/// argument, so neither prompt should inherit static value completions that
/// imply a constrained first-slot value set.
#[test]
fn selector_omits_stale_first_slot_candidates_for_free_form_or_argless_commands() {
    assert!(
        plan_selector(
            SelectorSurface::MezzanineCommand,
            "rename-session ne",
            "rename-session ne".len(),
        )
        .is_none()
    );
    assert!(
        plan_selector(
            SelectorSurface::MezzanineCommand,
            "list-themes to",
            "list-themes to".len(),
        )
        .is_none()
    );
}

/// Verifies skill-name shadow hints do not insert completion text in the
/// middle of an existing token.
///
/// Cursor navigation inside multi-line prompts should not cause the
/// completion renderer to duplicate part of a `$skill` token or shift the
/// visible cursor row while the user edits surrounding text.
#[test]
fn selector_shadow_hint_suppresses_dynamic_skill_suffix_inside_token() {
    let extra = vec![SelectorExtraCandidate::new(
        SelectorSurface::AgentCommand,
        "$",
        SelectorCandidate::new("$review-codebase", SelectorCandidateKind::Value, true),
    )];
    let line = "$rev-codebase produce a report";

    let hint = shadow_hint_with_extra(SelectorSurface::AgentCommand, line, "$rev".len(), &extra);

    assert!(hint.is_none());
}

/// Verifies MCP server-name shadow hints use the same dynamic selector path
/// as skill-name hints while remaining scoped to `@server` tokens.
///
/// The hint must be transient prompt text only: it completes the visible
/// suffix for the current token without mutating the editable buffer or
/// mixing with `$skill` candidates.
#[test]
fn selector_shadow_hint_completes_dynamic_mcp_server_suffix() {
    let extra = vec![
        SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "@",
            SelectorCandidate::new("@github", SelectorCandidateKind::Value, true),
        ),
        SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "$",
            SelectorCandidate::new("$github", SelectorCandidateKind::Value, true),
        ),
    ];

    let hint = shadow_hint_with_extra(
        SelectorSurface::AgentCommand,
        "ask @git",
        "ask @git".len(),
        &extra,
    )
    .unwrap();

    assert_eq!(hint.insert_at, "ask @git".len());
    assert_eq!(hint.text, "hub");
    assert_eq!(hint.kind, SelectorCandidateKind::Value);
}

/// Verifies known issue-project candidates complete only the value after a
/// supported `/show-issues` project option and remain available to hints.
///
/// Project paths are dynamic values rather than general command arguments, so
/// exposing them for an id, a flag, or another command would produce incorrect
/// Tab replacements and misleading shadow text.
#[test]
fn selector_scopes_issue_project_candidates_to_project_option_values() {
    let extra = vec![SelectorExtraCandidate::after_option(
        SelectorSurface::AgentCommand,
        "show-issues",
        "--project",
        SelectorCandidate::new("/repo/example", SelectorCandidateKind::Value, true),
    )];

    let project_plan = plan_selector_with_extra(
        SelectorSurface::AgentCommand,
        "/show-issues --project /repo/ex",
        "/show-issues --project /repo/ex".len(),
        &extra,
    )
    .unwrap();
    assert_eq!(project_plan.candidates[0].value, "/repo/example");

    let hint = shadow_hint_with_extra(
        SelectorSurface::AgentCommand,
        "/show-issues --project /repo/ex",
        "/show-issues --project /repo/ex".len(),
        &extra,
    )
    .unwrap();
    assert_eq!(hint.text, "ample");

    assert!(
        plan_selector_with_extra(
            SelectorSurface::AgentCommand,
            "/show-issues issue",
            "/show-issues issue".len(),
            &extra,
        )
        .is_none()
    );
    assert!(
        plan_selector_with_extra(
            SelectorSurface::AgentCommand,
            "/show-memories /repo/ex",
            "/show-memories /repo/ex".len(),
            &extra,
        )
        .is_none()
    );
}

/// Verifies the `/show-issues` project-glob alias receives the same dynamic
/// project candidates as the canonical `--project` option.
#[test]
fn selector_scopes_issue_project_candidates_to_project_glob_values() {
    let extra = vec![SelectorExtraCandidate::after_option(
        SelectorSurface::AgentCommand,
        "show-issues",
        "--project-glob",
        SelectorCandidate::new("/repo/example", SelectorCandidateKind::Value, true),
    )];

    let plan = plan_selector_with_extra(
        SelectorSurface::AgentCommand,
        "/show-issues --project-glob /repo/ex",
        "/show-issues --project-glob /repo/ex".len(),
        &extra,
    )
    .unwrap();

    assert_eq!(plan.candidates[0].value, "/repo/example");
}
