//! Exact current-trusted-project membership for the human agent browser.
//!
//! These offline fixtures use generated directories and real pane processes,
//! never provider calls or private session data. Membership and retained action
//! targets must be filtered together; display/search/export cannot recover rows
//! outside the captured origin's project. Administrative authority is unchanged.

use super::*;
use crate::runtime::control::agent_browser::AgentBrowserTarget;
use crate::security::project::{ProjectTrustStore, TrustDecision};
use mez_core::ids::ClientId;
use mez_mux::record_browser::RecordBrowser;
use std::collections::BTreeMap;

/// Captures authenticated CurrentProject for a direct snapshot. Tests of retained
/// refresh ownership separately hold one query instead of recapturing this seam.
fn scoped_snapshot(
    service: &mut RuntimeSessionService,
    client: &ClientId,
    origin: &str,
) -> (RecordBrowser, BTreeMap<String, AgentBrowserTarget>) {
    let query = service
        .current_project_agent_browser_query(client, origin)
        .unwrap();
    service
        .agent_management_browser_for_query(client, &query)
        .unwrap()
}

/// Authors current-version generated trust evidence for both withholding tests.
/// Pending is not an explicit decision supported by decide_at; use the store's
/// existing test-record hook, retaining canonical root and policy/schema fields.
fn fixture_trust_decision(
    store: &mut ProjectTrustStore,
    root: std::path::PathBuf,
    decision: TrustDecision,
) {
    if decision == TrustDecision::Pending {
        store
            .decide_at(root.clone(), TrustDecision::Trusted, None, 1)
            .unwrap();
        let mut record = store.get(&root).unwrap().clone();
        record.state = TrustDecision::Pending;
        store.insert_record_for_tests(record);
    } else {
        store.decide_at(root, decision, None, 1).unwrap();
    }
}

/// An invoking terminal need not itself have an agent registration. Resolve its
/// project anyway, include every separate registration on a matching pane, and
/// omit both unrelated records and their retained executable targets. The human
/// snapshot must remain uncapped above the model-facing 64-row limit.
#[test]
fn scoped_browser_filters_records_targets_and_exports_without_model_cap() {
    let mut fixture = ScopeFixture::new("scoped-browser-membership");
    let origin = fixture
        .service
        .create_window_with_pane_process(&fixture.primary, "unregistered-origin", false, None)
        .unwrap()
        .pane_id
        .to_string();
    fixture
        .service
        .set_pane_current_working_directory(&origin, fixture.first.clone());
    let mut included_ids = vec![fixture.first_id.clone()];
    let mut excluded_ids = vec![fixture.other_id.clone()];
    for index in 0..69 {
        // Interleave >64 excluded rows with >64 matching rows. Discovery order
        // must be preserved as an All subsequence, not re-sorted by project.
        let excluded = fixture.service.message_service_mut().register_agent(
            mez_core::ids::PaneId::opaque(&fixture.other_pane),
            None,
            format!("excluded-marker-{index}"),
            Vec::new(),
        );
        excluded_ids.push(excluded.agent_id.to_string());
        let identity = fixture.service.message_service_mut().register_agent(
            mez_core::ids::PaneId::opaque("%1"),
            None,
            format!("matched-{index}"),
            Vec::new(),
        );
        included_ids.push(identity.agent_id.to_string());
    }
    crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
    let (browser, targets) = scoped_snapshot(&mut fixture.service, &fixture.primary, &origin);
    assert_eq!(browser.records().len(), 70, "unrelated rows must not leak");
    assert_eq!(targets.len(), 70, "unrelated actions must not be retained");
    assert!(included_ids.iter().all(|id| targets.contains_key(id)));
    assert!(excluded_ids.iter().all(|id| !targets.contains_key(id)));
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (1, 3),
        "resolve unregistered origin once and each candidate pane once"
    );
    for query in ["", "excluded-marker", "other-worktree"] {
        let exported = browser.render_page_matching(query).raw_markdown;
        assert!(
            excluded_ids
                .iter()
                .all(|id| !exported.contains(&format!("`{id}`")))
        );
        assert!(!exported.contains("excluded-marker"));
        assert!(!exported.contains("other-worktree"));
    }
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (0, 0),
        "search/export are pure over the already-filtered snapshot"
    );
    let (all, all_targets) = fixture
        .service
        .agent_management_browser(&fixture.primary)
        .unwrap();
    assert_eq!(all.records().len(), 140);
    assert!(excluded_ids.iter().all(|id| all_targets.contains_key(id)));
    let expected: Vec<_> = all
        .records()
        .iter()
        .filter(|row| included_ids.contains(&row.id))
        .cloned()
        .collect();
    assert_eq!(browser.records(), expected);
    let mut ids: Vec<_> = browser.records().iter().map(|row| row.id.clone()).collect();
    ids.sort();
    assert_eq!(ids, targets.keys().cloned().collect::<Vec<_>>());
    assert!(fixture.service.agent_shell_store().get(&origin).is_none());
}

/// Two real process-backed panes and independent registrations under generated
/// directories. The fixture stays local to this test owner: no provider, native
/// conversation, accounting mapping or MMP membership supplies project evidence.
struct ScopeFixture {
    root: std::path::PathBuf,
    first: std::path::PathBuf,
    other: std::path::PathBuf,
    service: RuntimeSessionService,
    primary: ClientId,
    other_pane: String,
    first_id: String,
    other_id: String,
}

impl ScopeFixture {
    /// Creates two trusted sibling directories, each with one registered row.
    fn new(label: &str) -> Self {
        let root = temp_root(label);
        let first = root.join("first");
        let other = root.join("other-worktree");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&other).unwrap();
        let first = fs::canonicalize(first).unwrap();
        let other = fs::canonicalize(other).unwrap();
        let root = fs::canonicalize(root).unwrap();
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
        let first_id = service
            .message_service_mut()
            .register_agent(
                mez_core::ids::PaneId::opaque("%1"),
                None,
                "first-row",
                Vec::new(),
            )
            .agent_id
            .to_string();
        let other_id = service
            .message_service_mut()
            .register_agent(
                mez_core::ids::PaneId::opaque(&other_pane),
                None,
                "other-row",
                Vec::new(),
            )
            .agent_id
            .to_string();
        let mut fixture = Self {
            root,
            first,
            other,
            service,
            primary,
            other_pane,
            first_id,
            other_id,
        };
        fixture.trust_roots(&[fixture.first.clone(), fixture.other.clone()]);
        fixture
    }

    /// Replaces trust with exactly these canonical generated roots.
    fn trust_roots(&mut self, roots: &[std::path::PathBuf]) {
        let mut trust = ProjectTrustStore::default();
        for root in roots {
            trust
                .decide_at(root.clone(), TrustDecision::Trusted, None, 1)
                .unwrap();
        }
        self.service.set_project_trust_store(trust, None);
    }

    /// Checks that displayed IDs and retained action keys have identical scope.
    fn assert_ids(&mut self, expected: &[String]) {
        let (browser, targets) = scoped_snapshot(&mut self.service, &self.primary, "%1");
        let mut ids: Vec<_> = browser.records().iter().map(|row| row.id.clone()).collect();
        ids.sort();
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(ids, expected);
        assert_eq!(targets.keys().cloned().collect::<Vec<_>>(), expected);
    }

    /// Mounts one already-captured typed snapshot in the ordinary retained
    /// presentation container, without executing a fake command or second query.
    fn mount(&mut self, query: &crate::runtime::control::agent_browser::AgentBrowserQuery) {
        let (browser, targets) = self
            .service
            .agent_management_browser_for_query(&self.primary, query)
            .unwrap();
        self.service
            .show_primary_display_overlay(vec!["fixture browser".into()])
            .unwrap();
        self.service
            .primary_display_overlay_mut()
            .unwrap()
            .record_browser = Some(
            crate::runtime::service_state::RuntimeRecordBrowserOverlayState {
                pane_id: "%1".into(),
                command: "list-agents".into(),
                source: Some(
                    crate::runtime::service_state::RuntimeRecordBrowserOverlaySource::Agents {
                        client_id: self.primary.to_string(),
                        query: query.clone(),
                        targets,
                        confirmation: None,
                    },
                ),
                browser,
                stack: Vec::new(),
            },
        );
        self.service.reflow_primary_record_browser_overlay();
    }

    /// Routes bytes through actual attached-terminal modal input precedence.
    ///
    /// This helper deliberately routes rather than invoking admin reducers
    /// directly, preserving search/prompt and confirmation precedence.
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

    /// Asserts query identity and exact record/target membership after a mounted
    /// operation; confirmations are inspected separately when deliberately armed.
    fn assert_mounted(
        &self,
        query: &crate::runtime::control::agent_browser::AgentBrowserQuery,
        expected: &[String],
    ) {
        use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
        let state = self
            .service
            .primary_display_overlay()
            .unwrap()
            .record_browser
            .as_ref()
            .unwrap();
        let Some(RuntimeRecordBrowserOverlaySource::Agents {
            query: actual,
            targets,
            ..
        }) = state.source.as_ref()
        else {
            panic!("typed agent source must remain mounted");
        };
        assert_eq!(actual, query);
        let mut ids: Vec<_> = state
            .browser
            .records()
            .iter()
            .map(|row| row.id.clone())
            .collect();
        ids.sort();
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(ids, expected);
        assert_eq!(targets.keys().cloned().collect::<Vec<_>>(), expected);
    }

    /// Starts a queued native task on the remote pane in the invoking project,
    /// using existing offline scheduler admission without executing a provider.
    fn native_other_in_origin_project(&mut self) -> String {
        self.service
            .set_pane_current_working_directory(&self.other_pane, self.first.clone());
        self.service
            .agent_shell_store_mut()
            .enter_or_resume(&self.other_pane)
            .unwrap();
        self.service
            .start_agent_prompt_turn(&self.other_pane, "remote fixture task")
            .unwrap();
        format!("agent-{}", self.other_pane)
    }

    /// Retains a foreign-project registration throughout mounted native tests,
    /// so an accidental All rebuild fails membership assertions on every action.
    fn add_foreign_sentinel(&mut self) {
        let pane = self
            .service
            .create_window_with_pane_process(&self.primary, "foreign", false, None)
            .unwrap()
            .pane_id
            .to_string();
        self.service
            .set_pane_current_working_directory(&pane, self.other.clone());
        self.service.message_service_mut().register_agent(
            mez_core::ids::PaneId::opaque(pane),
            None,
            "foreign-sentinel",
            Vec::new(),
        );
    }
}

impl Drop for ScopeFixture {
    /// Retires only this fixture's processes and generated temporary directory.
    fn drop(&mut self) {
        let processes = self.service.terminate_all_pane_processes();
        let directory = fs::remove_dir_all(&self.root);
        if !std::thread::panicking() {
            processes.unwrap();
            directory.unwrap();
        }
    }
}

/// Exact canonical roots, not repository prefixes or retained audience groups,
/// define the cohort. Sibling worktrees differ; broad trusted ancestors group
/// siblings; trusting a nested root separates it from its trusted parent. A
/// symlink alias resolves to the same typed root rather than its display path.
#[test]
fn scoped_browser_canonical_alias_nested_and_broad_root_relations() {
    let mut fixture = ScopeFixture::new("scoped-browser-root-relations");
    fixture.assert_ids(&[fixture.first_id.clone()]);
    fixture.trust_roots(&[fixture.root.clone()]);
    fixture.assert_ids(&[fixture.first_id.clone(), fixture.other_id.clone()]);
    let nested = fixture.first.join("nested");
    fs::create_dir(&nested).unwrap();
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, nested.clone());
    fixture.trust_roots(&[fixture.first.clone()]);
    fixture.assert_ids(&[fixture.first_id.clone(), fixture.other_id.clone()]);
    fixture.trust_roots(&[fixture.first.clone(), nested]);
    fixture.assert_ids(&[fixture.first_id.clone()]);
    #[cfg(unix)]
    {
        let alias = fixture.root.join("alias");
        std::os::unix::fs::symlink(&fixture.first, &alias).unwrap();
        fixture
            .service
            .set_pane_current_working_directory(&fixture.other_pane, alias);
        fixture.assert_ids(&[fixture.first_id.clone(), fixture.other_id.clone()]);
    }
}

/// Lossy human labels may collide for two distinct Unix paths. Membership must
/// still use canonical typed filesystem identity, not the visible UTF-8 column.
#[cfg(unix)]
#[test]
fn scoped_browser_distinguishes_non_utf8_roots_with_identical_labels() {
    use std::os::unix::ffi::OsStringExt;
    let mut fixture = ScopeFixture::new("scoped-browser-lossy-labels");
    let first = fixture
        .root
        .join(std::ffi::OsString::from_vec(b"project-\xfe".to_vec()));
    let other = fixture
        .root
        .join(std::ffi::OsString::from_vec(b"project-\xff".to_vec()));
    fs::create_dir(&first).unwrap();
    fs::create_dir(&other).unwrap();
    assert_eq!(first.to_string_lossy(), other.to_string_lossy());
    fixture.trust_roots(&[first.clone(), other.clone()]);
    fixture
        .service
        .set_pane_current_working_directory("%1", first);
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, other);
    fixture.assert_ids(&[fixture.first_id.clone()]);
}

/// Withheld project evidence never equals another withheld project. Each
/// unavailable-origin result must be explicitly empty, nonsensitive, and offer
/// the All alternative rather than silently returning session-wide inventory.
#[test]
fn scoped_browser_withholds_unknown_rejected_pending_and_failed_trust() {
    let mut fixture = ScopeFixture::new("scoped-browser-withheld");
    for decision in [
        None,
        Some(TrustDecision::Rejected),
        Some(TrustDecision::Revoked),
        Some(TrustDecision::Pending),
    ] {
        let mut trust = ProjectTrustStore::default();
        if let Some(decision) = decision {
            fixture_trust_decision(&mut trust, fixture.root.clone(), decision);
        }
        fixture.service.set_project_trust_store(trust, None);
        let (browser, targets) = scoped_snapshot(&mut fixture.service, &fixture.primary, "%1");
        assert!(browser.records().is_empty(), "unknown roots must not match");
        assert!(targets.is_empty());
        let exported = browser.render_page().markdown;
        assert!(
            exported.contains("Current project unavailable"),
            "{exported}"
        );
        assert!(exported.contains("--all"), "{exported}");
        assert!(!exported.contains(&fixture.root.to_string_lossy().to_string()));
        assert!(!exported.contains(&fixture.other_id));
    }
    fixture.trust_roots(&[fixture.first.clone(), fixture.other.clone()]);
    fixture
        .service
        .set_pane_current_working_directory("%1", fixture.root.join("secret-missing-path"));
    fixture.assert_ids(&[]);
    fixture
        .service
        .set_pane_current_working_directory("%1", fixture.first.clone());
    let database = fixture.root.join("trust.sqlite");
    let mut trust = ProjectTrustStore::default();
    trust
        .decide_at(fixture.root.clone(), TrustDecision::Trusted, None, 1)
        .unwrap();
    trust.save_to_file(&database).unwrap();
    fixture
        .service
        .set_project_trust_store(trust, Some(database.clone()));
    fs::remove_file(&database).unwrap();
    fs::create_dir(&database).unwrap();
    let (browser, targets) = scoped_snapshot(&mut fixture.service, &fixture.primary, "%1");
    assert!(browser.records().is_empty());
    assert!(targets.is_empty());
    let exported = browser.render_page().markdown;
    assert!(exported.contains("trust refresh failed"), "{exported}");
    assert!(exported.contains("--all"), "{exported}");
    assert!(!exported.contains(&fixture.root.to_string_lossy().to_string()));
}

/// A valid origin cannot admit candidates with withheld or missing evidence,
/// including unattached registrations. Two matching labels such as unavailable
/// or em dash never constitute project identity or native control authority.
#[test]
fn scoped_browser_valid_origin_excludes_unqualified_candidates() {
    let mut fixture = ScopeFixture::new("scoped-browser-candidate-evidence");
    fixture
        .service
        .message_service_mut()
        .register_agent(None, None, "no-pane", Vec::new());
    fixture.service.message_service_mut().register_agent(
        mez_core::ids::PaneId::opaque("%missing"),
        None,
        "missing-pane",
        Vec::new(),
    );
    for decision in [
        None,
        Some(TrustDecision::Rejected),
        Some(TrustDecision::Revoked),
        Some(TrustDecision::Pending),
    ] {
        let mut trust = ProjectTrustStore::default();
        trust
            .decide_at(fixture.first.clone(), TrustDecision::Trusted, None, 1)
            .unwrap();
        if let Some(decision) = decision {
            fixture_trust_decision(&mut trust, fixture.other.clone(), decision);
        }
        fixture.service.set_project_trust_store(trust, None);
        fixture.assert_ids(&[fixture.first_id.clone()]);
    }
    fixture.trust_roots(&[fixture.root.clone()]);
    for path in [
        std::path::PathBuf::from("relative"),
        fixture.root.join("missing"),
    ] {
        fixture
            .service
            .set_pane_current_working_directory(&fixture.other_pane, path);
        fixture.assert_ids(&[fixture.first_id.clone()]);
    }
}

/// Separate native, internal, subagent and qualified external registrations on
/// the origin pane must retain distinct IDs, with one project lookup for that
/// pane. The external launch fixture exercises real restricted ingress using a
/// generated runtime capability; project equality never adds native controls.
#[test]
fn scoped_browser_keeps_all_kinds_and_native_external_shared_pane_distinct() {
    let mut fixture = ScopeFixture::new("scoped-browser-kinds");
    fixture
        .service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = fixture
        .service
        .start_agent_prompt_turn("%1", "fixture task")
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
    let (browser, targets) = scoped_snapshot(&mut fixture.service, &fixture.primary, "%1");
    assert_eq!(browser.records().len(), 5);
    assert_eq!(targets.len(), 5);
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (1, 2)
    );
    assert!(!targets.contains_key(&fixture.other_id));
    assert!(targets["agent-%1"].lifecycle.is_some());
    assert!(browser.records().iter().any(
        |row| row.id == internal && row.metadata.contains(&("Kind".into(), "internal".into()))
    ));
    assert!(
        browser.records().iter().any(
            |row| row.id == child && row.metadata.contains(&("Kind".into(), "subagent".into()))
        )
    );
    let external = browser
        .records()
        .iter()
        .find(|row| row.title == "External fixture")
        .unwrap();
    assert_ne!(external.id, "agent-%1");
    assert!(targets[&external.id].lifecycle.is_none());
    assert!(
        external
            .metadata
            .contains(&("Model".into(), "unavailable".into()))
    );
    assert!(external.metadata.contains(&(
        "Controls".into(),
        "focus only; native controls unavailable".into()
    )));
    let external_id = external.id.clone();
    let expected: Vec<_> = browser.records().iter().map(|row| row.id.clone()).collect();
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    let turn_count = fixture.service.agent_turn_ledger().turns().len();
    fixture.mount(&query);
    fixture.input(format!("/{external_id}\r").as_bytes());
    crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
    for input in [b"i".as_slice(), b"p".as_slice(), b"d".as_slice()] {
        fixture.input(input);
        fixture.assert_mounted(&query, &expected);
        assert!(fixture.service.agent_shell_pane_has_active_turn("%1"));
        assert!(fixture.service.agent_human_pause_status("%1").is_none());
        assert!(matches!(
            fixture.service.active_record_browser_effective_source(),
            Some(
                crate::runtime::service_state::RuntimeRecordBrowserOverlaySource::Agents {
                    confirmation: None,
                    ..
                }
            )
        ));
    }
    assert_eq!(
        fixture.service.agent_turn_ledger().turns().len(),
        turn_count
    );
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (0, 0)
    );
    let other_primary = fixture
        .service
        .attach_primary("other", true, Size::new(80, 24).unwrap(), 121)
        .unwrap();
    assert!(fixture.service.primary_display_overlay().is_none());
    assert_eq!(
        fixture
            .service
            .apply_agent_management_overlay_input(&other_primary, b"p")
            .unwrap(),
        None
    );
    fixture
        .service
        .prepare_client_render(&fixture.primary, ClientViewRole::Primary)
        .unwrap();
    fixture.assert_mounted(&query, &expected);
    assert_eq!(
        fixture
            .service
            .apply_agent_management_overlay_input(&other_primary, b"p")
            .unwrap(),
        Some(false)
    );
    assert!(
        fixture
            .service
            .refresh_agent_management_overlay(&other_primary)
            .is_err()
    );
    fixture.assert_mounted(&query, &expected);
    fixture.input(b"\r");
    assert!(fixture.service.primary_display_overlay().is_none());
    assert_eq!(fixture.service.active_pane_id().unwrap().as_str(), "%1");
}

/// The actual shared save and copy actions consume filtered records, not a
/// session-wide backing inventory. No excluded row, objective or project can be
/// exported even after searching for its full marker, and these operations must
/// perform zero project or trust lookups after snapshot construction.
#[test]
fn scoped_browser_save_copy_and_search_cannot_recover_excluded_rows() {
    use mez_mux::record_browser::{RecordBrowserAction, RecordBrowserOutcome};
    let mut fixture = ScopeFixture::new("scoped-browser-exports");
    let (mut browser, _) = scoped_snapshot(&mut fixture.service, &fixture.primary, "%1");
    crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
    assert!(!browser.set_active_record_id(&fixture.other_id));
    assert!(browser.set_active_record_id(&fixture.first_id));
    let RecordBrowserOutcome::CopyRequested { markdown } = browser
        .apply_action(RecordBrowserAction::CopyActive)
        .unwrap()
    else {
        panic!("copy must return scoped active source");
    };
    assert!(!markdown.contains(&fixture.other_id));
    assert!(!markdown.contains("other-row"));
    assert!(!markdown.contains("other-worktree"));
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
        panic!("save must return scoped raw source");
    };
    assert!(markdown.contains(&fixture.first_id));
    assert!(!markdown.contains(&fixture.other_id));
    assert!(!markdown.contains("other-row"));
    assert!(!markdown.contains("other-worktree"));
    for query in [fixture.other_id.as_str(), "other-row", "other-worktree"] {
        let page = browser.render_page_matching(query);
        assert!(
            page.markdown.contains("No records found."),
            "{}",
            page.markdown
        );
        assert!(!page.raw_markdown.contains(&fixture.other_id));
    }
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (0, 0)
    );
}

/// A retained MMP peer in project A can currently occupy project B. Human
/// membership follows the pane's current trusted root, without rebinding the
/// protocol identity, lineage, or accounting. Moving it back admits it again.
#[test]
fn scoped_browser_current_cwd_differs_from_frozen_mmp_membership() {
    let mut fixture = ScopeFixture::new("scoped-browser-mmp-divergence");
    let id = mez_core::ids::AgentId::opaque(&fixture.other_id).unwrap();
    let scope = mez_agent::messaging::ProjectScopeId::from_canonical_root_bytes(
        fixture.first.as_os_str().as_encoded_bytes(),
    );
    fixture
        .service
        .message_service_mut()
        .rebind_agent_project_scope(&id, Some(scope))
        .unwrap();
    fixture.service.set_subagent_lineage(
        &fixture.other_id,
        RuntimeSubagentLineage {
            parent_agent_id: fixture.first_id.clone(),
            root_agent_id: fixture.first_id.clone(),
            depth: 1,
            display_name: "moved child".into(),
            terminal: true,
        },
    );
    let before = fixture
        .service
        .message_service()
        .registered_identity(&id)
        .unwrap()
        .clone();
    let lineage = fixture
        .service
        .subagent_lineage(&fixture.other_id)
        .unwrap()
        .clone();
    fixture.assert_ids(&[fixture.first_id.clone()]);
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, fixture.first.clone());
    fixture.assert_ids(&[fixture.first_id.clone(), fixture.other_id.clone()]);
    assert_eq!(
        fixture.service.message_service().registered_identity(&id),
        Some(&before)
    );
    assert_eq!(
        fixture.service.subagent_lineage(&fixture.other_id),
        Some(&lineage)
    );
}

/// Generated real git worktrees share repository history but have distinct
/// canonical trusted roots. Browser membership must not infer a shared project
/// from git ancestry, and a deliberately broad trusted ancestor may group both.
#[test]
fn scoped_browser_separate_git_worktrees_are_distinct_trusted_roots() {
    let mut fixture = ScopeFixture::new("scoped-browser-git-worktrees");
    let repository = fixture.root.join("repository");
    let worktree = fixture.root.join("linked-worktree");
    let home = fixture.root.join("git-home");
    let hooks = home.join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    let git = || {
        let mut command = std::process::Command::new("git");
        command
            .env_clear()
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-c")
            .arg(format!("core.hooksPath={}", hooks.display()))
            .arg("-c")
            .arg("init.templateDir=");
        command
    };
    let status = git()
        .args(["init", "--quiet"])
        .arg(&repository)
        .status()
        .unwrap();
    assert!(status.success());
    let status = git()
        .arg("-C")
        .arg(&repository)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Generated fixture",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let status = git()
        .arg("-C")
        .arg(&repository)
        .args(["worktree", "add", "--quiet", "--detach"])
        .arg(&worktree)
        .status()
        .unwrap();
    assert!(status.success());
    fixture
        .service
        .set_pane_current_working_directory("%1", repository.clone());
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, worktree.clone());
    fixture.trust_roots(&[repository, worktree]);
    fixture.assert_ids(&[fixture.first_id.clone()]);
    fixture.trust_roots(&[fixture.root.clone()]);
    fixture.assert_ids(&[fixture.first_id.clone(), fixture.other_id.clone()]);
}

/// A missing trust store and an origin with no live process both yield explicit
/// empty scope, without disclosing failed paths or treating unknowns as peers.
/// An All snapshot remains available independently of scoped membership.
#[test]
fn scoped_browser_missing_store_or_process_is_explicitly_empty() {
    let mut fixture = ScopeFixture::new("scoped-browser-missing-evidence");
    fixture.service.integration.set_project_trust_store(None);
    let (browser, targets) = scoped_snapshot(&mut fixture.service, &fixture.primary, "%1");
    assert!(browser.records().is_empty());
    assert!(targets.is_empty());
    let page = browser.render_page();
    assert!(page.markdown.contains("Current project unavailable"));
    assert!(page.markdown.contains("--all"));
    assert!(!page.raw_markdown.contains(&fixture.other_id));
    fixture.trust_roots(&[fixture.root.clone()]);
    fixture.service.terminate_all_pane_processes().unwrap();
    let (browser, targets) = scoped_snapshot(&mut fixture.service, &fixture.primary, "%1");
    assert!(browser.records().is_empty());
    assert!(targets.is_empty());
    assert!(browser.render_page().markdown.contains("--all"));
    let (all, all_targets) = fixture
        .service
        .agent_management_browser(&fixture.primary)
        .unwrap();
    assert_eq!(all.records().len(), 2);
    assert_eq!(all_targets.len(), 2);
}

/// Hold one mounted query while focus moves elsewhere, candidates and origin
/// change cwd, and trust is revoked/restored. Only snapshot boundaries change
/// cohort; search, render and reflow are pure. Stable selection/search/viewport
/// and source clones/reconstruction retain the same query, never All or focus.
#[test]
fn scoped_browser_mounted_refresh_keeps_origin_search_selection_and_viewport() {
    let mut fixture = ScopeFixture::new("scoped-browser-mounted-refresh");
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.mount(&query);
    fixture
        .service
        .session
        .select_pane_global(&fixture.primary, &fixture.other_pane)
        .unwrap();
    fixture.input(format!("/{}\r", fixture.first_id).as_bytes());
    fixture
        .service
        .primary_display_overlay_mut()
        .unwrap()
        .record_browser
        .as_mut()
        .unwrap()
        .browser
        .set_scroll_offset(3);
    crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
    fixture.service.reflow_primary_record_browser_overlay();
    fixture
        .service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(50, 12).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap();
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (0, 0)
    );
    fixture.input(b"r");
    fixture.assert_mounted(&query, &[fixture.first_id.clone()]);
    let overlay = fixture.service.primary_display_overlay().unwrap();
    assert_eq!(
        overlay.search_query.as_deref(),
        Some(fixture.first_id.as_str())
    );
    assert_eq!(
        overlay
            .record_browser
            .as_ref()
            .unwrap()
            .browser
            .active_record_id(),
        Some(fixture.first_id.as_str())
    );
    assert_eq!(
        overlay
            .record_browser
            .as_ref()
            .unwrap()
            .browser
            .scroll_offset(),
        3
    );
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (1, 2)
    );
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, fixture.first.clone());
    fixture.assert_mounted(&query, &[fixture.first_id.clone()]);
    fixture.input(b"r");
    fixture.assert_mounted(
        &query,
        &[fixture.first_id.clone(), fixture.other_id.clone()],
    );
    fixture
        .service
        .set_pane_current_working_directory("%1", fixture.other.clone());
    fixture.input(b"r");
    fixture.assert_mounted(&query, &[fixture.first_id.clone()]);
    let mut trust = ProjectTrustStore::default();
    trust
        .decide_at(fixture.other.clone(), TrustDecision::Revoked, None, 1)
        .unwrap();
    fixture.service.set_project_trust_store(trust, None);
    fixture.input(b"r");
    fixture.assert_mounted(&query, &[]);
    assert!(
        fixture
            .service
            .primary_display_overlay()
            .unwrap()
            .record_browser
            .as_ref()
            .unwrap()
            .browser
            .render_page()
            .markdown
            .contains("--all")
    );
    fixture.trust_roots(&[fixture.first.clone(), fixture.other.clone()]);
    fixture.input(b"r");
    fixture.assert_mounted(&query, &[fixture.first_id.clone()]);
    let source = fixture
        .service
        .active_record_browser_effective_source()
        .unwrap();
    assert_eq!(
        fixture.service.record_browser_source_toggled_scope(&source),
        source
    );
    let frame = fixture
        .service
        .primary_display_overlay()
        .unwrap()
        .record_browser
        .as_ref()
        .unwrap()
        .clone();
    fixture
        .service
        .primary_display_overlay_mut()
        .unwrap()
        .record_browser = Some(frame);
    fixture.service.reflow_primary_record_browser_overlay();
    fixture.input(b"r");
    fixture.assert_mounted(&query, &[fixture.first_id.clone()]);
    assert_eq!(
        fixture.service.active_pane_id().unwrap().as_str(),
        fixture.other_pane
    );
}

/// Captured missing evidence cannot adopt a future process; captured live
/// evidence cannot adopt a restarted incarnation or a new conversation owner.
/// A different attached primary cannot reuse the query. These failures remain
/// explicitly empty/forbidden instead of redirecting to current focus or All.
#[test]
fn scoped_browser_retained_origin_rejects_replacement_and_client_transfer() {
    let mut fixture = ScopeFixture::new("scoped-browser-origin-replacement");
    let live = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.service.terminate_all_pane_processes().unwrap();
    let missing = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.service.start_initial_pane_process(None).unwrap();
    fixture
        .service
        .set_pane_current_working_directory("%1", fixture.first.clone());
    for query in [&live, &missing] {
        let (browser, targets) = fixture
            .service
            .agent_management_browser_for_query(&fixture.primary, query)
            .unwrap();
        assert!(browser.records().is_empty());
        assert!(targets.is_empty());
        assert!(
            browser
                .render_page()
                .markdown
                .contains("invoking pane incarnation or owner unavailable")
        );
        assert!(browser.render_page().markdown.contains("--all"));
        assert!(
            !browser
                .render_page()
                .markdown
                .contains(&fixture.root.to_string_lossy().to_string())
        );
    }
    let fresh = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    assert_eq!(
        fixture
            .service
            .agent_management_browser_for_query(&fixture.primary, &fresh)
            .unwrap()
            .0
            .records()
            .len(),
        1
    );
    // Identity injection models preferred-observation traces using this live
    // process's genuine PID/birth/generation. Exec alone is not a new owner;
    // switching from foreground fallback to preferred root is a different owner
    // even when the old identity is still accepted by the weaker observed guard.
    use crate::runtime::processes::{RuntimePaneProcessIdentityInjection, RuntimePaneProcessRole};
    let process = fixture.service.pane_process_identity("%1").unwrap();
    let inject = |service: &mut RuntimeSessionService, role, executable_path| {
        service.inject_pane_process_identity_for_tests(
            "%1",
            RuntimePaneProcessIdentityInjection::Identity {
                role,
                generation: process.generation,
                process_id: process.process_id,
                start_token: process.start_token,
                executable_path,
                live_start_token: Some(process.start_token),
            },
        );
    };
    inject(
        &mut fixture.service,
        process.role,
        fixture.root.join("different-executable"),
    );
    assert_eq!(
        fixture
            .service
            .agent_management_browser_for_query(&fixture.primary, &fresh)
            .unwrap()
            .0
            .records()
            .len(),
        1
    );
    inject(
        &mut fixture.service,
        RuntimePaneProcessRole::ForegroundProcessGroupLeader,
        process.executable_path.clone(),
    );
    let foreground_query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    let mut still_observed = process.clone();
    still_observed.role = RuntimePaneProcessRole::ForegroundProcessGroupLeader;
    assert!(
        fixture
            .service
            .pane_process_identity_is_current("%1", &still_observed)
    );
    assert!(
        fixture
            .service
            .agent_management_browser_for_query(&fixture.primary, &foreground_query)
            .unwrap()
            .0
            .records()
            .is_empty()
    );
    let other_client = fixture
        .service
        .attach_primary("other", true, Size::new(80, 24).unwrap(), 121)
        .unwrap();
    crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
    assert!(
        fixture
            .service
            .agent_management_browser_for_query(&other_client, &fresh)
            .is_err()
    );
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (0, 0)
    );
    fixture
        .service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (browser, targets) = fixture
        .service
        .agent_management_browser_for_query(&fixture.primary, &fresh)
        .unwrap();
    assert!(browser.records().is_empty());
    assert!(targets.is_empty());
    let owned = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture
        .service
        .agent_shell_store_mut()
        .start_new_conversation("%1")
        .unwrap();
    assert!(
        fixture
            .service
            .agent_management_browser_for_query(&fixture.primary, &owned)
            .unwrap()
            .0
            .records()
            .is_empty()
    );
}

/// Closing an invoking pane destroys the retained origin even while another
/// matching pane is active. The browser must not pivot to that pane's project,
/// and an unknown invocation pane must likewise remain explicitly unavailable.
#[test]
fn scoped_browser_destroyed_origin_never_pivots_to_active_pane() {
    let mut fixture = ScopeFixture::new("scoped-browser-origin-destroyed");
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, fixture.first.clone());
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.mount(&query);
    fixture
        .service
        .session
        .select_pane_global(&fixture.primary, &fixture.other_pane)
        .unwrap();
    fixture
        .service
        .dispatch_runtime_pane_close(
            &fixture.primary,
            &serde_json::json!({"pane_id":"%1", "force":true}).to_string(),
        )
        .unwrap();
    fixture.input(b"r");
    fixture.assert_mounted(&query, &[]);
    assert_eq!(
        fixture.service.active_pane_id().unwrap().as_str(),
        fixture.other_pane
    );
    let missing = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%not-present")
        .unwrap();
    let (browser, targets) = fixture
        .service
        .agent_management_browser_for_query(&fixture.primary, &missing)
        .unwrap();
    assert!(browser.records().is_empty());
    assert!(targets.is_empty());
    assert!(browser.render_page().markdown.contains("--all"));
}

/// Pause/resume, interrupt and both close cancellation keys rebuild only the
/// original scoped cohort. Force-close confirmation removes the selected remote
/// owner and its generic shared-pane row, never origin or excluded registration.
/// Every operation keeps focus unchanged and retains the exact typed query.
#[test]
fn scoped_browser_admin_refreshes_and_close_cancellation_keep_query() {
    let mut fixture = ScopeFixture::new("scoped-browser-admin-refresh");
    let native = fixture.native_other_in_origin_project();
    fixture.add_foreign_sentinel();
    fixture
        .service
        .session
        .select_pane_global(&fixture.primary, "%1")
        .unwrap();
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.mount(&query);
    fixture.input(format!("/{native}\r").as_bytes());
    let expected = [
        fixture.first_id.clone(),
        fixture.other_id.clone(),
        native.clone(),
    ];
    fixture.assert_mounted(&query, &expected);
    fixture.input(b"p");
    assert_eq!(
        fixture
            .service
            .agent_human_pause_status(&fixture.other_pane),
        Some("paused")
    );
    fixture.assert_mounted(&query, &expected);
    fixture.input(b"p");
    assert!(
        fixture
            .service
            .agent_human_pause_status(&fixture.other_pane)
            .is_none()
    );
    fixture.assert_mounted(&query, &expected);
    fixture.input(b"i");
    assert!(
        !fixture
            .service
            .agent_shell_pane_has_active_turn(&fixture.other_pane)
    );
    fixture.assert_mounted(&query, &expected);
    for cancel in [b"n".as_slice(), b"\x1b".as_slice()] {
        fixture.input(b"d");
        assert!(
            fixture
                .service
                .primary_display_overlay()
                .unwrap()
                .lines
                .iter()
                .any(|line| line.contains(&format!("Confirm close {native}")))
        );
        fixture.input(cancel);
        assert!(
            fixture
                .service
                .find_pane_descriptor(&fixture.other_pane)
                .is_some()
        );
        fixture.assert_mounted(&query, &expected);
    }
    fixture.input(b"d");
    fixture.input(b"y");
    assert!(
        fixture
            .service
            .find_pane_descriptor(&fixture.other_pane)
            .is_none()
    );
    assert!(fixture.service.find_pane_descriptor("%1").is_some());
    fixture.assert_mounted(&query, &[fixture.first_id.clone()]);
    assert_eq!(fixture.service.active_pane_id().unwrap().as_str(), "%1");
}

/// Refreshing after a selected target leaves the project retires its armed
/// confirmation and clickable generation. A following old y/click cannot close
/// or focus the fallback neighbor, even when that neighbor has native controls.
#[test]
fn scoped_browser_disappearing_target_retires_confirmation_and_old_action() {
    use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
    let mut fixture = ScopeFixture::new("scoped-browser-stale-confirmation");
    fixture.add_foreign_sentinel();
    fixture
        .service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    fixture
        .service
        .start_agent_prompt_turn("%1", "origin fixture task")
        .unwrap();
    let native = fixture.native_other_in_origin_project();
    fixture
        .service
        .session
        .select_pane_global(&fixture.primary, "%1")
        .unwrap();
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.mount(&query);
    fixture.input(format!("/{native}\r").as_bytes());
    let old_action = fixture
        .service
        .primary_display_overlay()
        .unwrap()
        .selections[0]
        .action_id;
    fixture.input(b"d");
    assert!(matches!(
        fixture.service.active_record_browser_effective_source(),
        Some(RuntimeRecordBrowserOverlaySource::Agents {
            confirmation: Some(_),
            ..
        })
    ));
    fixture
        .service
        .set_pane_current_working_directory(&fixture.other_pane, fixture.other.clone());
    fixture
        .service
        .refresh_agent_management_overlay(&fixture.primary)
        .unwrap();
    fixture.assert_mounted(&query, &[fixture.first_id.clone(), "agent-%1".into()]);
    assert!(matches!(
        fixture.service.active_record_browser_effective_source(),
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
    assert_eq!(fixture.service.active_pane_id().unwrap().as_str(), "%1");
    fixture.assert_mounted(&query, &[fixture.first_id.clone(), "agent-%1".into()]);
}

/// The existing colon mounted browser remains exactly All: same records,
/// metadata, table/export and target identities as the explicit typed query,
/// with no agent shell entry or new colon --all parser. Retained refresh stays
/// All even when current panes belong to different trusted projects.
#[test]
fn scoped_browser_explicit_all_matches_colon_and_retained_refresh() {
    use crate::runtime::control::agent_browser::AgentBrowserQuery;
    let mut fixture = ScopeFixture::new("scoped-browser-all-parity");
    let (browser, targets) = fixture
        .service
        .agent_management_browser_for_query(&fixture.primary, &AgentBrowserQuery::All)
        .unwrap();
    fixture
        .service
        .execute_attached_display_command(&fixture.primary, "list-agents")
        .unwrap();
    fixture.assert_mounted(
        &AgentBrowserQuery::All,
        &[fixture.first_id.clone(), fixture.other_id.clone()],
    );
    let state = fixture
        .service
        .primary_display_overlay()
        .unwrap()
        .record_browser
        .as_ref()
        .unwrap();
    assert_eq!(state.browser.records(), browser.records());
    assert_eq!(
        state.browser.render_page().raw_markdown,
        browser.render_page().raw_markdown
    );
    let Some(crate::runtime::service_state::RuntimeRecordBrowserOverlaySource::Agents {
        targets: mounted_targets,
        ..
    }) = state.source.as_ref()
    else {
        panic!("All source expected");
    };
    assert_eq!(mounted_targets, &targets);
    fixture.input(b"r");
    fixture.assert_mounted(
        &AgentBrowserQuery::All,
        &[fixture.first_id.clone(), fixture.other_id.clone()],
    );
    assert!(fixture.service.agent_shell_store().get("%1").is_none());
    assert!(
        fixture
            .service
            .execute_terminal_command(&fixture.primary, "list-agents --all")
            .is_err()
    );
}

/// The visible wheel/pager viewport belongs to overlay.scroll_offset, not only
/// the browser's internal detail/list offset. With many searched rows, r must
/// not jump back to the first search match. When scope becomes unavailable, the
/// retained visible viewport must clamp to the new short empty browser instead.
#[test]
fn scoped_browser_refresh_preserves_actual_searched_viewport_and_clamps_empty() {
    let mut fixture = ScopeFixture::new("scoped-browser-visible-viewport");
    for index in 0..70 {
        fixture.service.message_service_mut().register_agent(
            mez_core::ids::PaneId::opaque("%1"),
            None,
            format!("viewport-marker-{index}"),
            Vec::new(),
        );
    }
    let query = fixture
        .service
        .current_project_agent_browser_query(&fixture.primary, "%1")
        .unwrap();
    fixture.mount(&query);
    fixture.input(b"/viewport-marker\r");
    crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
    fixture
        .service
        .apply_primary_display_overlay_scroll(10)
        .unwrap();
    let visible_scroll = fixture
        .service
        .primary_display_overlay()
        .unwrap()
        .scroll_offset;
    assert!(
        visible_scroll > 0,
        "tall searched view must actually scroll"
    );
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (0, 0)
    );
    fixture.input(b"r");
    assert_eq!(
        fixture
            .service
            .primary_display_overlay()
            .unwrap()
            .scroll_offset,
        visible_scroll,
        "refresh must preserve actual searched viewport, not reset to first match"
    );
    assert_eq!(
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
        (1, 2)
    );
    fixture
        .service
        .set_pane_current_working_directory("%1", std::path::PathBuf::from("relative"));
    fixture.input(b"r");
    fixture.assert_mounted(&query, &[]);
    assert_eq!(
        fixture
            .service
            .primary_display_overlay()
            .unwrap()
            .scroll_offset,
        0
    );
}

/// A short search term that actually appears in rendered rows exercises the
/// next-search-match reflow branch, unlike full metadata clipped from the table.
/// Both direct owner refresh and attached r must preserve the visible viewport
/// once it is scrolled away from the first match, retaining query and selection.
#[test]
fn scoped_browser_rendered_search_match_refresh_preserves_visible_viewport() {
    for direct in [true, false] {
        let mut fixture = ScopeFixture::new("scoped-browser-rendered-search");
        let mut expected = vec![fixture.first_id.clone()];
        for index in 0..70 {
            expected.push(
                fixture
                    .service
                    .message_service_mut()
                    .register_agent(
                        mez_core::ids::PaneId::opaque("%1"),
                        None,
                        format!("matching-{index}"),
                        Vec::new(),
                    )
                    .agent_id
                    .to_string(),
            );
        }
        let query = fixture
            .service
            .current_project_agent_browser_query(&fixture.primary, "%1")
            .unwrap();
        fixture.mount(&query);
        fixture.input(b"/a\r");
        let overlay = fixture.service.primary_display_overlay().unwrap();
        assert_eq!(overlay.search_query.as_deref(), Some("a"));
        let matched = overlay
            .search_match
            .expect("query must match actual rendered text");
        assert!(overlay.lines[matched.line_index].contains('a'));
        let first_match_offset = matched
            .line_index
            .saturating_sub(mez_mux::overlay::overlay_fixed_prefix_rows(overlay));
        assert!(
            fixture
                .service
                .apply_primary_display_overlay_scroll(10)
                .unwrap()
        );
        let visible_scroll = fixture
            .service
            .primary_display_overlay()
            .unwrap()
            .scroll_offset;
        assert_ne!(
            visible_scroll, first_match_offset,
            "viewport must leave the initial match"
        );
        let overlay = fixture.service.primary_display_overlay().unwrap();
        let state = overlay.record_browser.as_ref().unwrap();
        let selected = overlay
            .active_selection_index
            .and_then(|index| overlay.selections.get(index))
            .and_then(|selection| state.browser.records().get(selection.logical_id))
            .map(|record| record.id.clone())
            .expect("rendered stable selection must exist before refresh");
        crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts();
        if direct {
            fixture
                .service
                .refresh_agent_management_overlay(&fixture.primary)
                .unwrap();
        } else {
            fixture.input(b"r");
        }
        fixture.assert_mounted(&query, &expected);
        let overlay = fixture.service.primary_display_overlay().unwrap();
        assert_eq!(overlay.search_query.as_deref(), Some("a"));
        assert_eq!(
            overlay
                .record_browser
                .as_ref()
                .unwrap()
                .browser
                .active_record_id(),
            Some(selected.as_str())
        );
        assert_eq!(
            overlay.scroll_offset, visible_scroll,
            "direct={direct}: visible-search refresh must preserve viewport"
        );
        assert_eq!(
            crate::runtime::control::agent_browser::take_agent_browser_project_lookup_counts(),
            (1, 2)
        );
    }
}
