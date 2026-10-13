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

/// Temporary RED seam: invoke the existing snapshot until the production typed
/// query replaces this helper. A behavioral failure, not a missing API compile
/// error, establishes that All currently leaks unrelated records and targets.
fn scoped_snapshot(
    service: &mut RuntimeSessionService,
    client: &ClientId,
    _origin: &str,
) -> (RecordBrowser, BTreeMap<String, AgentBrowserTarget>) {
    service.agent_management_browser(client).unwrap()
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
            trust
                .decide_at(fixture.root.clone(), decision, None, 1)
                .unwrap();
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
            trust
                .decide_at(fixture.other.clone(), decision, None, 1)
                .unwrap();
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
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(&repository)
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new("git")
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
    let status = std::process::Command::new("git")
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
