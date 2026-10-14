//! Actual slash ingress and one-shot human agent browser presentation.
//!
//! Generated trusted directories and owned pane processes qualify provenance;
//! no provider, private home, model discovery, or helper-installed overlay is
//! involved. Tests submit real prompt bytes and inspect the mounted typed source.

use super::*;
use crate::runtime::control::agent_browser::AgentBrowserQuery;
use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
use crate::security::project::{ProjectTrustStore, TrustDecision};
use mez_core::ids::ClientId;

/// Offline two-project fixture with an existing visible prompt in each pane.
struct SlashFixture {
    root: PathBuf,
    first: PathBuf,
    other: PathBuf,
    service: RuntimeSessionService,
    primary: ClientId,
    other_pane: String,
}

impl SlashFixture {
    /// Creates only generated filesystem/process evidence, then registers native
    /// rows via the existing offline prompt-turn seam without executing providers.
    fn new(label: &str) -> Self {
        let root = temp_root(label);
        let first = root.join("first");
        let other = root.join("other");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&other).unwrap();
        let first = fs::canonicalize(first).unwrap();
        let other = fs::canonicalize(other).unwrap();
        let mut service = test_runtime_service();
        let primary = service
            .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
            .unwrap();
        service.start_initial_pane_process(None).unwrap();
        let other_pane = service
            .create_window_with_pane_process(&primary, "other", false, None)
            .unwrap()
            .pane_id
            .to_string();
        service.set_pane_current_working_directory("%1", first.clone());
        service.set_pane_current_working_directory(&other_pane, other.clone());
        let mut trust = ProjectTrustStore::default();
        for root in [&first, &other] {
            trust
                .decide_at(root.clone(), TrustDecision::Trusted, None, 1)
                .unwrap();
        }
        service.set_project_trust_store(trust, None);
        for pane in ["%1", other_pane.as_str()] {
            service
                .agent_shell_store_mut()
                .enter_or_resume(pane)
                .unwrap();
            service
                .start_agent_prompt_turn(pane, "offline fixture")
                .unwrap();
            service.stop_agent_turn_for_pane(pane).unwrap();
        }
        service.session.select_pane_global(&primary, "%1").unwrap();
        Self {
            root,
            first,
            other,
            service,
            primary,
            other_pane,
        }
    }

    /// Routes bytes through the ordinary attached terminal input action.
    fn input(&mut self, bytes: &[u8]) {
        self.service
            .apply_attached_terminal_step_plan(
                &self.primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(bytes.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    /// Returns the real mounted Agents frame, refusing textual pager substitutes.
    fn mounted(&self) -> &crate::runtime::service_state::RuntimeRecordBrowserOverlayState {
        let overlay = self.service.primary_display_overlay().expect("slash pager");
        let state = overlay
            .record_browser
            .as_ref()
            .expect("typed Agents browser");
        assert_eq!(state.command, "list-agents");
        assert!(matches!(
            state.source,
            Some(RuntimeRecordBrowserOverlaySource::Agents { .. })
        ));
        state
    }

    /// Executes with a real renderer-local capture but retains it before
    /// adoption to test the synchronous owner fence and exact snapshot transfer.
    fn capture(
        &mut self,
    ) -> (
        crate::runtime::commands::list_agents::AgentPromptCommandCapture,
        String,
    ) {
        let input = "/list-agents";
        let mut capture =
            self.service
                .prepare_agent_prompt_command_capture(&self.primary, "%1", input, false);
        let response = self
            .service
            .execute_attached_agent_shell_command(&self.primary, "%1", input, &mut capture)
            .unwrap();
        (capture, response)
    }

    /// Requires records, targets and retained query to agree after each actual
    /// mounted interaction; text/neighbor selection cannot supply authority.
    fn assert_scope(&self, query: &AgentBrowserQuery, expected: &[String]) {
        let state = self.mounted();
        let Some(RuntimeRecordBrowserOverlaySource::Agents {
            query: actual,
            targets,
            ..
        }) = &state.source
        else {
            panic!("Agents source");
        };
        assert_eq!(actual, query);
        let mut ids = state
            .browser
            .records()
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(ids, expected);
        assert_eq!(targets.keys().cloned().collect::<Vec<_>>(), expected);
    }

    /// Error chrome belongs to rendered Markdown/actual overlay lines, not raw
    /// copy/save exports. Both visible projections must carry the fixed reason
    /// and explicit All hint; export behavior stays unchanged and nonsensitive.
    fn assert_unavailable_view(&self, reason: &str) {
        let page = self.mounted().browser.render_page();
        assert!(page.markdown.contains(reason), "{}", page.markdown);
        assert!(page.markdown.contains("--all"), "{}", page.markdown);
        assert!(!page.raw_markdown.contains("Current project unavailable"));
        let visible = self
            .service
            .primary_display_overlay()
            .unwrap()
            .lines
            .join(" ");
        assert!(visible.contains(reason), "{visible}");
        assert!(visible.contains("--all"), "{visible}");
    }
}

impl Drop for SlashFixture {
    /// Retires only this fixture's processes and generated data, even on failure.
    fn drop(&mut self) {
        let processes = self.service.terminate_all_pane_processes();
        let directory = fs::remove_dir_all(&self.root);
        if !std::thread::panicking() {
            processes.unwrap();
            directory.unwrap();
        }
    }
}

/// Submitting bare slash through readline must mount the existing typed browser
/// with exact invoking-client/pane scope and filtered targets. Opening does not
/// allocate a conversation or turn, change focus, or schedule provider work.
#[test]
fn list_agents_slash_attached_prompt_mounts_current_project() {
    let mut fixture = SlashFixture::new("list-agents-slash-attached");
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    let before = fixture
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .clone();
    fixture.input(b"/list-agents\r");
    let state = fixture.mounted();
    assert_eq!(
        state
            .browser
            .records()
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["agent-%1"]
    );
    let Some(RuntimeRecordBrowserOverlaySource::Agents {
        client_id,
        query: mounted,
        targets,
        ..
    }) = &state.source
    else {
        panic!("Agents source");
    };
    assert_eq!(client_id, fixture.primary.as_str());
    assert_eq!(mounted, &query);
    assert_eq!(
        targets.keys().map(String::as_str).collect::<Vec<_>>(),
        ["agent-%1"]
    );
    let after = fixture.service.agent_shell_store().get("%1").unwrap();
    assert_eq!(after.session_id, before.session_id);
    assert_eq!(after.running_turn_id, before.running_turn_id);
    assert_eq!(fixture.service.active_pane_id().unwrap().as_str(), "%1");
    assert!(fixture.service.pending_record_browser_overlays_is_empty());
}

/// Literal --all must mount All, including rows above the model discovery cap.
/// Unchanged fixture evidence yields the same records/export as colon after a
/// dismiss/reopen; colon still rejects flags and creates no new conversation.
#[test]
fn list_agents_slash_all_mounts_uncapped_colon_parity() {
    let mut fixture = SlashFixture::new("list-agents-slash-all");
    for index in 0..146 {
        fixture.service.message_service_mut().register_agent(
            None,
            None,
            format!("row-{index}"),
            Vec::new(),
        );
    }
    fixture.input(b"/list-agents --all\r");
    let state = fixture.mounted();
    assert_eq!(state.browser.records().len(), 148);
    assert!(matches!(
        state.source,
        Some(RuntimeRecordBrowserOverlaySource::Agents {
            query: AgentBrowserQuery::All,
            ..
        })
    ));
    let records = state.browser.records().to_vec();
    let markdown = state.browser.render_page().raw_markdown;
    let source = state.source.clone();
    fixture.input(b"\x1b");
    fixture
        .service
        .execute_attached_display_command(&fixture.primary, "list-agents")
        .unwrap();
    let colon = fixture.mounted();
    assert_eq!(colon.browser.records(), records);
    assert_eq!(colon.browser.render_page().raw_markdown, markdown);
    assert_eq!(colon.source, source);
    assert!(
        fixture
            .service
            .execute_terminal_command(&fixture.primary, "list-agents --all")
            .is_err()
    );
}

/// Textual control/direct calls may return the existing display contract but
/// must not stage UI capability. Replaying a textual response into a pager must
/// never pick up old typed targets, even for the same pane/command key.
#[test]
fn list_agents_slash_textual_ingress_leaves_no_typed_browser() {
    let mut fixture = SlashFixture::new("list-agents-slash-textual");
    for control in [true, false] {
        let response = if control {
            fixture
                .service
                .execute_agent_shell_control_command(&fixture.primary, "/list-agents --all")
                .unwrap()
        } else {
            fixture
                .service
                .execute_agent_shell_command(&fixture.primary, "/list-agents --all")
                .unwrap()
        };
        let json: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(
            response.contains("agent-%1"),
            "must return actual display: {json}"
        );
        assert!(fixture.service.pending_record_browser_overlays_is_empty());
        assert!(fixture.service.primary_display_overlay().is_none());
        fixture
            .service
            .set_agent_prompt_response_display_output_for_tests("%1", &response)
            .unwrap();
        assert!(
            fixture
                .service
                .primary_display_overlay()
                .unwrap()
                .record_browser
                .is_none()
        );
        fixture.input(b"\x1b");
    }
    fixture.input(b"/list-agents\r");
    assert_eq!(fixture.mounted().browser.records().len(), 1);
}

/// Invalid options are pane-local usage errors, never a provider prompt or an
/// implicit global query. Quotes and escape spellings cannot synthesize flags;
/// only the raw single literal --all selects the session inventory.
#[test]
fn list_agents_slash_rejects_nonliteral_or_duplicate_options() {
    let mut fixture = SlashFixture::new("list-agents-slash-grammar");
    for args in [
        "--all --all",
        "--unknown",
        "project",
        "--all=true",
        "--all project",
        "\"--all\"",
        "'--all'",
        "\"--all",
        "--all\"",
        "--\\all",
        "--",
        "--ALL",
    ] {
        let input = format!("/list-agents {args}");
        let response = fixture
            .service
            .execute_agent_shell_control_command(&fixture.primary, &input)
            .unwrap();
        assert!(
            response.contains("usage: /list-agents [--all]"),
            "{input}: {response}"
        );
        assert!(
            !response.contains("agent-%1"),
            "invalid options must not query: {response}"
        );
        assert!(fixture.service.pending_record_browser_overlays_is_empty());
        assert!(
            fixture
                .service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .is_none()
        );
    }
}

/// The pane-targeted UI path (also used by mouse paste) must scope to the passed
/// prompt even when global focus names another project. This covers the direct
/// attached path in addition to the adapter-owned terminal-step submission.
#[test]
fn list_agents_slash_uses_actual_nonfocused_invocation_pane() {
    let mut fixture = SlashFixture::new("list-agents-slash-nonfocused");
    fixture
        .service
        .session
        .select_pane_global(&fixture.primary, &fixture.other_pane)
        .unwrap();
    let expected = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture
        .service
        .apply_attached_agent_prompt_input_for_pane(&fixture.primary, "%1", b"/list-agents\r")
        .unwrap();
    let state = fixture.mounted();
    assert_eq!(state.pane_id, "%1");
    assert!(
        matches!(&state.source, Some(RuntimeRecordBrowserOverlaySource::Agents { query, .. }) if query == &expected)
    );
    assert_eq!(
        state
            .browser
            .records()
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["agent-%1"]
    );
    assert_eq!(
        fixture.service.active_pane_id().unwrap().as_str(),
        fixture.other_pane
    );
}

/// A read-only opening must leave the complete task/scheduler and pause fences
/// intact. A live or human-paused task is not an excuse to queue the diagnostic
/// as model input, resume it, consume capacity, or create a new conversation.
#[test]
fn list_agents_slash_opening_preserves_running_and_paused_work() {
    for mode in ["running", "paused", "blocked", "waiting"] {
        let mut fixture = SlashFixture::new("list-agents-slash-active");
        fixture
            .service
            .execute_agent_shell_command(&fixture.primary, "retained fixture task")
            .unwrap();
        let turn = fixture
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .clone()
            .unwrap();
        if mode == "paused" {
            let target = fixture
                .service
                .capture_agent_lifecycle_target(&fixture.primary, "%1")
                .unwrap();
            fixture
                .service
                .pause_agent_lifecycle_target(&fixture.primary, &target)
                .unwrap();
        } else if mode == "blocked" || mode == "waiting" {
            // Use the real provider-capacity reducers, not label-only state.
            // Joined/MMP waiting uses Blocked ledger ownership plus Waiting
            // scheduler ownership; neither permits a new prompt turn.
            if mode == "blocked" {
                fixture
                    .service
                    .agent_scheduler_mut()
                    .block_running(&turn)
                    .unwrap();
            } else {
                fixture
                    .service
                    .agent_scheduler_mut()
                    .wait_running(&turn)
                    .unwrap();
            }
            fixture
                .service
                .agent_turn_ledger_mut()
                .finish_turn(&turn, AgentTurnState::Blocked)
                .unwrap();
        }
        let session = fixture
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .clone();
        let turns = fixture.service.agent_turn_ledger().turns().to_vec();
        let scheduler = fixture.service.agent_scheduler().snapshot();
        let provider_count = fixture.service.pending_agent_provider_tasks().len();
        let pause = fixture
            .service
            .agent_human_pause_status("%1")
            .map(str::to_string);
        fixture.input(b"/list-agents\r");
        fixture.mounted();
        assert_eq!(
            fixture
                .service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .session_id,
            session.session_id
        );
        assert_eq!(
            fixture
                .service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id,
            session.running_turn_id
        );
        assert_eq!(fixture.service.agent_turn_ledger().turns(), turns);
        assert_eq!(fixture.service.agent_scheduler().snapshot(), scheduler);
        assert_eq!(
            fixture.service.pending_agent_provider_tasks().len(),
            provider_count
        );
        assert_eq!(
            fixture.service.agent_human_pause_status("%1"),
            pause.as_deref()
        );
    }
}

/// Mounting adopts exactly one shared-builder snapshot, including columns,
/// metadata, targets and origin. Parsing, pending adoption, reflow and paint do
/// not silently rebuild it or perform another project/trust lookup.
#[test]
fn list_agents_slash_capture_mounts_exact_snapshot_once() {
    let mut fixture = SlashFixture::new("list-agents-slash-capture-once");
    crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
    let (capture, response) = fixture.capture();
    let captured = capture.browser.as_ref().unwrap().clone();
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (1, 2)
    );
    assert!(
        fixture.service.pending_record_browser_overlays_is_empty(),
        "capture stays local before mounting"
    );
    let output = crate::runtime::runtime_agent_shell_display_output(
        &response,
        fixture.service.ui_theme(),
        100,
        100,
    )
    .unwrap();
    fixture
        .service
        .present_captured_agent_prompt_display_output(&fixture.primary, "%1", capture, output)
        .unwrap();
    let mounted = fixture.mounted();
    assert_eq!(mounted.browser.records(), captured.browser.records());
    assert_eq!(
        mounted.browser.render_page().raw_markdown,
        captured.browser.render_page().raw_markdown
    );
    assert_eq!(mounted.source, captured.source);
    assert!(fixture.service.pending_record_browser_overlays_is_empty());
    fixture
        .service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(53, 12).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap();
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (0, 0)
    );
}

/// A capture cannot survive client/pane/generation/conversation/process changes
/// or a suppressed/nonmatching pager result. Nothing is staged before these
/// checks, and replaying the display later cannot recover typed targets.
#[test]
fn list_agents_slash_stale_or_mismatched_capture_is_discarded() {
    for mode in [
        "generation",
        "pane",
        "client",
        "conversation",
        "process",
        "suppressed",
        "command",
    ] {
        let mut fixture = SlashFixture::new("list-agents-slash-capture-rejection");
        let (capture, response) = fixture.capture();
        let mut client = fixture.primary.clone();
        let mut pane = "%1".to_string();
        let mut output = crate::runtime::runtime_agent_shell_display_output(
            &response,
            fixture.service.ui_theme(),
            100,
            100,
        )
        .unwrap();
        match mode {
            "generation" => {
                fixture
                    .service
                    .execute_attached_display_command(&fixture.primary, "list-agents")
                    .unwrap();
                fixture.input(b"\x1b");
            }
            "pane" => pane = fixture.other_pane.clone(),
            "client" => {
                client = fixture
                    .service
                    .attach_primary("replacement", true, Size::new(100, 24).unwrap(), 121)
                    .unwrap()
            }
            "conversation" => {
                fixture
                    .service
                    .agent_shell_store_mut()
                    .start_new_conversation("%1")
                    .unwrap();
            }
            "process" => {
                fixture.service.terminate_all_pane_processes().unwrap();
                fixture.service.start_initial_pane_process(None).unwrap();
            }
            "suppressed" => output = crate::runtime::RuntimeAgentShellDisplayOutput::Suppressed,
            "command" => {
                let mut json: serde_json::Value = serde_json::from_str(&response).unwrap();
                json["command"] = "help".into();
                output = crate::runtime::runtime_agent_shell_display_output(
                    &json.to_string(),
                    fixture.service.ui_theme(),
                    100,
                    100,
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            fixture
                .service
                .present_captured_agent_prompt_display_output(&client, &pane, capture, output)
                .is_err(),
            "{mode}"
        );
        assert!(
            fixture.service.pending_record_browser_overlays_is_empty(),
            "{mode}"
        );
        fixture
            .service
            .set_agent_prompt_response_display_output_for_tests(&pane, &response)
            .unwrap();
        assert!(
            fixture
                .service
                .primary_display_overlay()
                .unwrap()
                .record_browser
                .is_none(),
            "{mode}"
        );
    }
}

/// Actual slash mounting retains scope through search typing, resize, selection,
/// manual refresh, cwd/trust changes and origin replacement. Search/copy/save
/// operate only on the retained filtered rows, with zero paint/export lookups.
#[test]
fn list_agents_slash_mounted_refresh_search_resize_exports_keep_origin() {
    use mez_mux::record_browser::{RecordBrowserAction, RecordBrowserOutcome};
    let mut fixture = SlashFixture::new("list-agents-slash-refresh");
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.input(b"/list-agents\r");
    fixture
        .service
        .session
        .select_pane_global(&fixture.primary, &fixture.other_pane)
        .unwrap();
    crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
    fixture.input(b"/ipdr");
    assert!(fixture.service.agent_human_pause_status("%1").is_none());
    assert!(matches!(
        fixture.mounted().source,
        Some(RuntimeRecordBrowserOverlaySource::Agents {
            confirmation: None,
            ..
        })
    ));
    fixture.input(b"\x1b");
    fixture.input(b"/agent-%1\r");
    fixture
        .service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(52, 12).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap();
    let mut browser = fixture.mounted().browser.clone();
    assert!(!browser.set_active_record_id(&format!("agent-{}", fixture.other_pane)));
    assert!(browser.set_active_record_id("agent-%1"));
    let RecordBrowserOutcome::CopyRequested { markdown } = browser
        .apply_action(RecordBrowserAction::CopyActive)
        .unwrap()
    else {
        panic!("copy source");
    };
    assert!(!markdown.contains(&format!("agent-{}", fixture.other_pane)));
    browser
        .apply_action(RecordBrowserAction::StartSave)
        .unwrap();
    browser
        .apply_action(RecordBrowserAction::EditPrompt("fixture.md".into()))
        .unwrap();
    let RecordBrowserOutcome::SaveSubmitted { markdown, .. } = browser
        .apply_action(RecordBrowserAction::SubmitPrompt)
        .unwrap()
    else {
        panic!("save source");
    };
    assert!(!markdown.contains(&format!("agent-{}", fixture.other_pane)));
    assert!(!markdown.contains(&fixture.other.to_string_lossy().to_string()));
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (0, 0)
    );
    fixture.input(b"r");
    fixture.assert_scope(&query, &["agent-%1".into()]);
    assert_eq!(
        fixture
            .service
            .primary_display_overlay()
            .unwrap()
            .search_query
            .as_deref(),
        Some("agent-%1")
    );
    assert_eq!(
        fixture.mounted().browser.active_record_id(),
        Some("agent-%1")
    );
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (1, 2)
    );
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, fixture.first.clone());
    fixture.input(b"r");
    fixture.assert_scope(
        &query,
        &["agent-%1".into(), format!("agent-{}", fixture.other_pane)],
    );
    fixture.service.integration.set_project_trust_store(None);
    fixture.input(b"r");
    fixture.assert_scope(&query, &[]);
    fixture.assert_unavailable_view("Current project unavailable");
    fixture
        .service
        .agent_shell_store_mut()
        .start_new_conversation("%1")
        .unwrap();
    let mut trust = ProjectTrustStore::default();
    trust
        .decide_at(fixture.first.clone(), TrustDecision::Trusted, None, 1)
        .unwrap();
    fixture.service.set_project_trust_store(trust, None);
    fixture.input(b"r");
    fixture.assert_scope(&query, &[]);
    assert_eq!(
        fixture.service.active_pane_id().unwrap().as_str(),
        fixture.other_pane
    );
}

/// Explicit native controls in a slash-mounted view use original scope and
/// stable target ownership. Removed membership retires armed confirmation and
/// old clickable IDs; cancellation or stale y never closes the neighbor.
#[test]
fn list_agents_slash_native_controls_and_stale_confirmation_keep_query() {
    let mut fixture = SlashFixture::new("list-agents-slash-admin");
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, fixture.first.clone());
    fixture
        .service
        .start_agent_prompt_turn(&fixture.other_pane, "retained remote")
        .unwrap();
    let native = format!("agent-{}", fixture.other_pane);
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.input(b"/list-agents\r");
    fixture.input(format!("/{native}\r").as_bytes());
    for key in [b"p".as_slice(), b"p".as_slice(), b"i".as_slice()] {
        fixture.input(key);
        fixture.assert_scope(&query, &["agent-%1".into(), native.clone()]);
        assert_eq!(fixture.service.active_pane_id().unwrap().as_str(), "%1");
    }
    assert!(
        !fixture
            .service
            .agent_shell_pane_has_active_turn(&fixture.other_pane)
    );
    for cancel in [b"n".as_slice(), b"\x1b".as_slice()] {
        fixture.input(b"d");
        assert!(matches!(
            fixture.mounted().source,
            Some(RuntimeRecordBrowserOverlaySource::Agents {
                confirmation: Some(_),
                ..
            })
        ));
        fixture.input(cancel);
        fixture.assert_scope(&query, &["agent-%1".into(), native.clone()]);
        assert!(
            fixture
                .service
                .find_pane_descriptor(&fixture.other_pane)
                .is_some()
        );
    }
    let old_action = fixture
        .service
        .primary_display_overlay()
        .unwrap()
        .selections[0]
        .action_id;
    fixture.input(b"d");
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, fixture.other.clone());
    // Armed confirmation deliberately swallows r. An owner refresh still
    // invalidates disappeared targets; do not broaden native modal keys.
    fixture
        .service
        .refresh_agent_management_overlay(&fixture.primary)
        .unwrap();
    fixture.assert_scope(&query, &["agent-%1".into()]);
    assert!(matches!(
        fixture.mounted().source,
        Some(RuntimeRecordBrowserOverlaySource::Agents {
            confirmation: None,
            ..
        })
    ));
    fixture.input(b"/\r");
    fixture.input(b"y");
    fixture
        .service
        .execute_primary_display_overlay_action(&fixture.primary, old_action)
        .unwrap();
    assert!(fixture.service.find_pane_descriptor("%1").is_some());
    assert!(
        fixture
            .service
            .find_pane_descriptor(&fixture.other_pane)
            .is_some()
    );
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, fixture.first.clone());
    fixture.input(b"r");
    fixture.input(format!("/{native}\r").as_bytes());
    fixture.input(b"d");
    fixture.input(b"y");
    assert!(
        fixture
            .service
            .find_pane_descriptor(&fixture.other_pane)
            .is_none()
    );
    fixture.assert_scope(&query, &["agent-%1".into()]);
}

/// Real queued manual compaction must remain exactly the same auxiliary task.
/// Opening slash is allowlisted diagnostic work, not a mutation, extra provider
/// request, compaction cancellation, conversation creation or capacity claim.
#[test]
fn list_agents_slash_opens_during_real_compaction_without_state_change() {
    let mut fixture = SlashFixture::new("list-agents-slash-compacting");
    fixture.service.replace_config_layers(vec![ConfigLayer {
        name: "offline-compaction".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"offline-compaction\"\n[providers.runtime-batch]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.offline-compaction]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=40000\nmax_input_tokens=20000\n".into(),
    }]).unwrap();
    let conversation = fixture
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let store = AgentTranscriptStore::new(fixture.root.join("transcripts"));
    for sequence in 1..=8 {
        store
            .append(&TranscriptEntry {
                conversation_id: conversation.clone(),
                sequence,
                created_at_unix_seconds: sequence,
                role: TranscriptRole::Assistant,
                turn_id: format!("old-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "generated compaction source ".repeat(400),
            })
            .unwrap();
    }
    fixture.service.set_agent_transcript_store(store.clone());
    fixture
        .service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 8)
        .unwrap();
    let response = fixture
        .service
        .execute_agent_shell_command(&fixture.primary, "/compact")
        .unwrap();
    assert!(
        fixture.service.agent_is_compacting("%1"),
        "real queued compaction required: {response}"
    );
    let before = fixture
        .service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let turns = fixture.service.agent_turn_ledger().turns().to_vec();
    let scheduler = fixture.service.agent_scheduler().snapshot();
    let provider_count = fixture.service.pending_agent_provider_tasks().len();
    let rows = store.inspect(&conversation).unwrap();
    fixture.input(b"/list-agents\r");
    fixture.mounted();
    assert!(fixture.service.agent_is_compacting("%1"));
    let after = fixture
        .service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(after.task_generation, before.task_generation);
    assert_eq!(after.compaction_epoch, before.compaction_epoch);
    assert_eq!(after.conversation_id, conversation);
    assert_eq!(after.request, before.request);
    assert_eq!(fixture.service.agent_turn_ledger().turns(), turns);
    assert_eq!(fixture.service.agent_scheduler().snapshot(), scheduler);
    assert_eq!(
        fixture.service.pending_agent_provider_tasks().len(),
        provider_count
    );
    assert_eq!(store.inspect(&conversation).unwrap(), rows);
}

/// A collapsed bracketed-paste command takes SubmittedWithDisplay through the
/// actual readline renderer, in both adapter and direct attached paths. Literal
/// whitespace does not change scope, and display metadata cannot replace source.
#[test]
fn list_agents_slash_collapsed_paste_mounts_both_attached_paths() {
    for adapter in [false, true] {
        let mut fixture = SlashFixture::new("list-agents-slash-paste");
        let source = format!("/list-agents --all{}", "\n".repeat(20));
        let bytes = format!("\x1b[200~{source}\x1b[201~\r");
        if adapter {
            fixture.input(bytes.as_bytes());
        } else {
            fixture
                .service
                .apply_attached_agent_prompt_input_for_pane(
                    &fixture.primary,
                    "%1",
                    bytes.as_bytes(),
                )
                .unwrap();
        }
        fixture.assert_scope(
            &AgentBrowserQuery::All,
            &["agent-%1".into(), format!("agent-{}", fixture.other_pane)],
        );
        assert!(fixture.service.pending_record_browser_overlays_is_empty());
    }
}

/// Missing/withheld origin evidence opens an explicit empty typed scoped view,
/// not All or the focused pane's project. A following explicit --all is the only
/// way to request session inventory. Attached malformed input remains a notice.
#[test]
fn list_agents_slash_untrusted_origin_empty_and_invalid_input_is_notice() {
    let mut fixture = SlashFixture::new("list-agents-slash-untrusted");
    fixture.service.integration.set_project_trust_store(None);
    fixture.input(b"/list-agents\r");
    assert!(fixture.mounted().browser.records().is_empty());
    fixture.assert_unavailable_view("Current project unavailable");
    fixture.input(b"\x1b");
    fixture.input(b"/list-agents --all\r");
    fixture.assert_scope(
        &AgentBrowserQuery::All,
        &["agent-%1".into(), format!("agent-{}", fixture.other_pane)],
    );
    fixture.input(b"\x1b");
    fixture.input(b"/list-agents \"--all\"\r");
    assert!(fixture.service.primary_display_overlay().is_none());
    assert!(fixture.service.pending_record_browser_overlays_is_empty());
    assert!(
        fixture
            .service
            .primary_error_status_overlay()
            .unwrap()
            .contains("usage: /list-agents [--all]"),
        "attached usage must be visible"
    );
    let response = fixture
        .service
        .execute_agent_shell_control_command(&fixture.primary, "/list-agents \"--all\"")
        .unwrap();
    assert!(response.contains("usage: /list-agents [--all]"));
    assert!(
        fixture
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .is_none()
    );
}

/// Native, internal, subagent and authenticated external rows sharing one pane
/// retain distinct IDs and one project cache entry. Scope adds no lifecycle
/// rights to external rows; native focus uses explicit stable-ID activation and
/// another attached primary cannot borrow the mounted administrative frame.
#[test]
fn list_agents_slash_all_kinds_external_controls_and_client_isolation() {
    let mut fixture = SlashFixture::new("list-agents-slash-kinds");
    let started = fixture
        .service
        .start_agent_prompt_turn("%1", "fixture retained work")
        .unwrap();
    let internal = fixture
        .service
        .message_service_mut()
        .register_agent(
            mez_core::ids::PaneId::opaque("%1"),
            None,
            "internal",
            Vec::new(),
        )
        .agent_id
        .to_string();
    fixture.service.register_macro_managed_subagent(
        &internal,
        &started.turn_id,
        "agent-%1",
        "review",
    );
    let child = fixture
        .service
        .message_service_mut()
        .register_agent(
            mez_core::ids::PaneId::opaque("%1"),
            None,
            "child",
            Vec::new(),
        )
        .agent_id
        .to_string();
    fixture.service.set_subagent_lineage(
        &child,
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%1".into(),
            root_agent_id: "agent-%1".into(),
            depth: 1,
            display_name: "fixture child".into(),
            terminal: true,
        },
    );
    let launch: serde_json::Value = serde_json::from_str(&fixture.service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#,
        &fixture.primary,
    )).unwrap();
    assert!(launch.get("error").is_none(), "{launch}");
    let mut connection = ControlConnectionState::new(true, false);
    connection
        .bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
            crate::runtime::current_effective_uid(),
        ))
        .unwrap();
    let request = serde_json::json!({"jsonrpc":"2.0","id":"register","method":"agent/external/register","params":{
        "launch_token": launch["result"]["launch_token"], "generation": launch["result"]["generation"],
        "external_session_id": "fixture-external", "display_name": "External fixture"
    }}).to_string();
    let registration: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .dispatch_runtime_control_body_for_connection(&request, &mut connection),
    )
    .unwrap();
    assert!(registration.get("error").is_none(), "{registration}");
    crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
    fixture.input(b"/list-agents\r");
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (1, 2)
    );
    let state = fixture.mounted();
    assert_eq!(state.browser.records().len(), 4);
    assert!(state.browser.records().iter().any(
        |row| row.id == internal && row.metadata.contains(&("Kind".into(), "internal".into()))
    ));
    assert!(
        state.browser.records().iter().any(
            |row| row.id == child && row.metadata.contains(&("Kind".into(), "subagent".into()))
        )
    );
    let external = state
        .browser
        .records()
        .iter()
        .find(|row| row.title == "External fixture")
        .unwrap()
        .id
        .clone();
    assert_ne!(external, "agent-%1");
    let query = match state.source.as_ref().unwrap() {
        RuntimeRecordBrowserOverlaySource::Agents { query, targets, .. } => {
            assert!(targets["agent-%1"].lifecycle.is_some());
            assert!(targets[&external].lifecycle.is_none());
            query.clone()
        }
        _ => panic!("Agents source"),
    };
    let expected = vec!["agent-%1".into(), internal, child, external.clone()];
    fixture.input(format!("/{external}\r").as_bytes());
    for key in [b"i".as_slice(), b"p".as_slice(), b"d".as_slice()] {
        fixture.input(key);
        fixture.assert_scope(&query, &expected);
        assert!(fixture.service.agent_shell_pane_has_active_turn("%1"));
        assert!(fixture.service.agent_human_pause_status("%1").is_none());
        assert!(matches!(
            fixture.mounted().source,
            Some(RuntimeRecordBrowserOverlaySource::Agents {
                confirmation: None,
                ..
            })
        ));
    }
    let old_action = fixture
        .service
        .primary_display_overlay()
        .unwrap()
        .selections[0]
        .action_id;
    let other = fixture
        .service
        .attach_primary("other-primary", true, Size::new(100, 24).unwrap(), 121)
        .unwrap();
    assert!(fixture.service.primary_display_overlay().is_none());
    assert!(
        !fixture
            .service
            .execute_primary_display_overlay_action(&other, old_action)
            .unwrap()
    );
    assert!(fixture.service.agent_shell_pane_has_active_turn("%1"));
    assert!(fixture.service.agent_human_pause_status("%1").is_none());
}

/// More than 64 current-project rows mount through actual slash ingress while
/// interleaved foreign-project registrations are excluded from both records
/// and targets, not merely hidden by search or the model discovery cap.
#[test]
fn list_agents_slash_current_project_inventory_is_uncapped_and_filtered() {
    let mut fixture = SlashFixture::new("list-agents-slash-large-project");
    let mut expected = vec!["agent-%1".into()];
    for index in 0..70 {
        expected.push(
            fixture
                .service
                .message_service_mut()
                .register_agent(
                    mez_core::ids::PaneId::opaque("%1"),
                    None,
                    format!("included-{index}"),
                    Vec::new(),
                )
                .agent_id
                .to_string(),
        );
        fixture.service.message_service_mut().register_agent(
            mez_core::ids::PaneId::opaque(&fixture.other_pane),
            None,
            format!("excluded-{index}"),
            Vec::new(),
        );
    }
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.input(b"/list-agents\r");
    fixture.assert_scope(&query, &expected);
    assert_eq!(fixture.mounted().browser.records().len(), 71);
    assert!(
        !fixture
            .mounted()
            .browser
            .render_page()
            .raw_markdown
            .contains("excluded-")
    );
}

/// The actual authenticated JSON-RPC mutation owner returns the display body
/// but leaves every pending typed map empty. A later real prompt submission
/// captures fresh project scope rather than adopting the earlier All response.
#[test]
fn list_agents_slash_rpc_display_does_not_stage_later_ui() {
    let mut fixture = SlashFixture::new("list-agents-slash-rpc");
    let request = r#"{"jsonrpc":"2.0","id":"list-agents","method":"agent/shell/command","params":{"idempotency_key":"list-agents-once","input":"/list-agents --all"}}"#;
    let response: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .dispatch_runtime_control_body(request, &fixture.primary),
    )
    .unwrap();
    assert!(response.get("error").is_none(), "{response}");
    assert!(
        response["result"]["body"]
            .as_str()
            .unwrap()
            .contains(&format!("agent-{}", fixture.other_pane)),
        "{response}"
    );
    assert!(fixture.service.primary_display_overlay().is_none());
    assert!(fixture.service.pending_record_browser_overlays_is_empty());
    fixture.input(b"/list-agents\r");
    assert_eq!(fixture.mounted().browser.records().len(), 1);
}

/// Native selection-style activation uses one stable record ID for keyboard
/// and real identifier-cell mouse coordinates. Navigation/search/opening never
/// focus a pane; explicit activation alone focuses the selected remote owner.
#[test]
fn list_agents_slash_keyboard_and_mouse_focus_exact_stable_id() {
    for mouse in [false, true] {
        let mut fixture = SlashFixture::new("list-agents-slash-focus");
        let remote = format!("agent-{}", fixture.other_pane);
        fixture.input(b"/list-agents --all\r");
        assert_eq!(fixture.service.active_pane_id().unwrap().as_str(), "%1");
        fixture.input(format!("/{remote}\r").as_bytes());
        assert_eq!(fixture.service.active_pane_id().unwrap().as_str(), "%1");
        if mouse {
            let index = fixture.service.primary_display_overlay_action_targets().iter()
                .position(|target| matches!(target, crate::runtime::render::OverlayActionTarget::RecordBrowserSelect { record_id } if record_id == &remote)).unwrap();
            let selection = &fixture
                .service
                .primary_display_overlay()
                .unwrap()
                .selections[index];
            let position = CopyPosition {
                line: selection.line_index.saturating_add(1),
                column: selection.start_column.saturating_add(2),
            };
            fixture
                .service
                .apply_attached_terminal_step_plan(
                    &fixture.primary,
                    &AttachedTerminalClientStepPlan {
                        actions: vec![TerminalClientLoopAction::HandleMouse(
                            MouseAction::SelectDisplayOverlay { position },
                        )],
                        output_lines: Vec::new(),
                        output_line_style_spans: Vec::new(),
                        input_hangup: false,
                        output_hangup: false,
                        error_roles: Vec::new(),
                    },
                )
                .unwrap();
        } else {
            fixture.input(b"\r");
        }
        assert!(fixture.service.primary_display_overlay().is_none());
        assert_eq!(
            fixture.service.active_pane_id().unwrap().as_str(),
            fixture.other_pane
        );
    }
}

/// Destroying the slash invocation pane while another matching pane is focused
/// must leave the retained CurrentProject query empty, not pivot to live focus
/// or recreate the origin. Fresh All remains a separate explicit invocation.
#[test]
fn list_agents_slash_closed_origin_stays_unavailable_after_refresh() {
    let mut fixture = SlashFixture::new("list-agents-slash-closed-origin");
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, fixture.first.clone());
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.input(b"/list-agents\r");
    fixture
        .service
        .session
        .select_pane_global(&fixture.primary, &fixture.other_pane)
        .unwrap();
    fixture
        .service
        .dispatch_runtime_pane_close(&fixture.primary, r#"{"pane_id":"%1","force":true}"#)
        .unwrap();
    fixture.input(b"r");
    fixture.assert_scope(&query, &[]);
    fixture.assert_unavailable_view("invoking pane incarnation or owner unavailable");
    assert_eq!(
        fixture.service.active_pane_id().unwrap().as_str(),
        fixture.other_pane
    );
}

/// Actual textual RPC diagnostics must explain withheld or failed origin
/// provenance and the explicit --all escape hatch without leaving typed UI
/// state. Raw saved exports remain distinct from the command's display body.
#[test]
fn list_agents_slash_rpc_unavailable_origin_includes_reason_and_all_hint() {
    for failed_reload in [false, true] {
        let mut fixture = SlashFixture::new("list-agents-slash-rpc-unavailable");
        if failed_reload {
            let database = fixture.root.join("trust.json");
            let mut trust = ProjectTrustStore::default();
            trust
                .decide_at(fixture.first.clone(), TrustDecision::Trusted, None, 1)
                .unwrap();
            trust.save_to_file(&database).unwrap();
            fixture
                .service
                .set_project_trust_store(trust, Some(database.clone()));
            fs::write(database, "invalid generated trust fixture").unwrap();
        } else {
            fixture.service.integration.set_project_trust_store(None);
        }
        let request = r#"{"jsonrpc":"2.0","id":"unavailable","method":"agent/shell/command","params":{"idempotency_key":"unavailable-once","input":"/list-agents"}}"#;
        let response: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .dispatch_runtime_control_body(request, &fixture.primary),
        )
        .unwrap();
        assert!(response.get("error").is_none(), "{response}");
        let body = response["result"]["body"].as_str().unwrap();
        assert!(body.contains("Current project unavailable"), "{body}");
        assert!(body.contains("--all"), "{body}");
        assert!(body.contains("No records found."), "{body}");
        if failed_reload {
            assert!(body.contains("trust refresh failed"), "{body}");
        }
        assert!(!body.contains("agent-%1"));
        assert!(!body.contains(&fixture.first.to_string_lossy().to_string()));
        assert!(fixture.service.pending_record_browser_overlays_is_empty());
        assert!(fixture.service.primary_display_overlay().is_none());
    }
}
