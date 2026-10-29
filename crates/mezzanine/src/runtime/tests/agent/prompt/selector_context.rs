//! Runtime completion-context regressions at render and input ownership seams.
//!
//! Synthetic pane cwd observations and prompt-local fixtures exercise actual
//! frame projections without shells, providers, trust changes, or submissions.

use super::*;

/// Idle pane cwd changes must reach both agent frame clones and primary command
/// overlays before another input byte arrives. Rendering cannot mutate the
/// retained draft, and first prefilled command display must use the pane root.
#[test]
fn runtime_selector_context_idle_render_uses_current_pane_cwd() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    let root = temp_root("selector-context-render");
    let a = root.join("a");
    let b = root.join("b");
    for cwd in [&a, &b] {
        fs::create_dir_all(cwd).unwrap();
    }
    fs::write(a.join("entry-alpha"), "a").unwrap();
    fs::write(b.join("entry-beta"), "b").unwrap();
    service.set_pane_current_working_directory("%1", a.clone());
    service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap()
        .prompt
        .buffer
        .set_line("inspect ./entry-");
    for cwd in [&a, &b] {
        service.set_pane_current_working_directory("%1", cwd.clone());
        let frame = service.terminal_frame_context();
        let prompt = frame.panes["%1"].agent_prompt.as_ref().unwrap();
        assert_eq!(prompt.selector_working_directory.as_ref(), Some(cwd));
        assert_eq!(prompt.buffer.line(), "inspect ./entry-");
        prompt.complete_filesystem_selector_for_tests();
        let expected = if cwd == &a {
            "entry-alpha"
        } else {
            "entry-beta"
        };
        assert_eq!(
            prompt.render_with_shadow_hint(),
            format!("⟩ inspect ./{expected}")
        );
        assert_eq!(
            prompt.rendered_cursor_column(),
            "⟩ inspect ./entry-".chars().count()
        );
    }
    service
        .enter_primary_command_prompt("source-file entry-")
        .unwrap();
    assert_eq!(
        service
            .primary_prompt_input()
            .unwrap()
            .prompt
            .selector_working_directory
            .as_ref(),
        Some(&b)
    );
    // Complete A's previously displayed hint, then change authoritative cwd to B.
    service.set_pane_current_working_directory("%1", a.clone());
    service
        .apply_primary_prompt_input(&primary, b"", false)
        .unwrap();
    let prompt = &service.primary_prompt_input().unwrap().prompt;
    prompt.complete_filesystem_selector_for_tests();
    assert!(prompt.render_with_shadow_hint().contains("entry-alpha"));
    let draft = prompt.buffer.draft_snapshot();
    service.set_pane_current_working_directory("%1", b);
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(!view.lines.iter().any(|line| line.contains("entry-alpha")));
    let frame = service.terminal_frame_context();
    let mut current = frame.panes["%1"].agent_prompt.clone().unwrap();
    current.kind = crate::ui::readline::ReadlinePromptKind::Command;
    current.buffer.set_line("source-file entry-");
    current.complete_filesystem_selector_for_tests();
    assert_eq!(current.render_with_shadow_hint(), ":source-file entry-beta");
    // The overlay clone shares the same worker as the retained primary prompt.
    let mut overlay = service.primary_prompt_input().unwrap().prompt.clone();
    overlay.set_selector_working_directory(current.selector_working_directory.clone());
    overlay.complete_filesystem_selector_for_tests();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(view.lines.iter().any(|line| line.contains("entry-beta")));
    assert_eq!(
        service
            .primary_prompt_input()
            .unwrap()
            .prompt
            .buffer
            .draft_snapshot(),
        draft
    );
    let _ = fs::remove_dir_all(root);
}

/// Idle Ctrl+C replaces the buffer directly in the runtime. A later cycle key
/// must not resurrect the retained selector's immutable base draft, and prompt
/// teardown/recreation must not carry the old selection or worker ownership.
#[test]
fn runtime_selector_context_ctrl_c_and_recreated_prompt_cannot_restore_selection() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"/log\t")
        .unwrap();
    assert!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .selector
            .is_some()
    );
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"\x03")
        .unwrap();
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        ""
    );
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"\t")
        .unwrap();
    let mut fresh =
        crate::ui::readline::ReadlinePrompt::new(crate::ui::readline::ReadlinePromptKind::Agent);
    fresh.apply_terminal_input(b"\t").unwrap();
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        fresh.buffer.line()
    );
    service.remove_agent_prompt_input("%1");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"\x1b[Z")
        .unwrap();
    let mut fresh =
        crate::ui::readline::ReadlinePrompt::new(crate::ui::readline::ReadlinePromptKind::Agent);
    fresh.apply_terminal_input(b"\x1b[Z").unwrap();
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        fresh.buffer.line()
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// Native agent completion must use owned workspace intent even when a later
/// pane observation changes. The primary command prompt keeps physical pane
/// context, so navigation cannot silently retarget ordinary pane commands.
/// Actual filesystem hints must agree with each prompt's captured directory.
#[test]
fn runtime_selector_context_native_directory_keeps_prompt_roles_separate() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_native_shell_mode_for_tests("%1");
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    let root = temp_root("native-directory-selector-roles");
    let target = root.join("agent");
    let observed = root.join("pane");
    for path in [&target, &observed] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(target.join("entry-agent"), "agent").unwrap();
    fs::write(observed.join("entry-pane"), "pane").unwrap();
    let target = fs::canonicalize(target).unwrap();
    let observed = fs::canonicalize(observed).unwrap();
    service.set_pane_current_working_directory("%1", observed.clone());
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .record_native_working_directory(target.clone(), 0)
        .unwrap();
    service
        .agent_prompt_inputs_mut_for_tests()
        .get_mut("%1")
        .unwrap()
        .prompt
        .buffer
        .set_line("inspect ./entry-");
    let frame = service.terminal_frame_context();
    let prompt = frame.panes["%1"].agent_prompt.as_ref().unwrap();
    assert_eq!(prompt.selector_working_directory.as_ref(), Some(&target));
    prompt.complete_filesystem_selector_for_tests();
    assert_eq!(prompt.render_with_shadow_hint(), "⟩ inspect ./entry-agent");
    assert_eq!(
        service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "inspect ./entry-"
    );
    service
        .enter_primary_command_prompt("source-file entry-")
        .unwrap();
    let command = &service.primary_prompt_input().unwrap().prompt;
    assert_eq!(command.selector_working_directory.as_ref(), Some(&observed));
    command.complete_filesystem_selector_for_tests();
    assert_eq!(command.render_with_shadow_hint(), ":source-file entry-pane");
    let _ = fs::remove_dir_all(root);
}
