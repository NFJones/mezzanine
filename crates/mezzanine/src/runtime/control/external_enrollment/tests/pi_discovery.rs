//! Ordinary installed Pi sources on two real pane roots and distinct packages.
//!
//! Actual CLI publication, package files and kernel writer/root proof precede
//! accounting. Synthetic policy/phase files only control the offline vendor;
//! no gate supplies a source identity, credential, ticket or expense. Separate
//! shell exit status and post-assert completion are required after native EOF.
//! Discovery models the inspected trust decision and exact disabled override,
//! not the vendor trust database, general globs, SDK/provider or macOS loading.

use super::pi_assembled::{AuditedIo, WireAudit, bare_bootstrap, wire_frames};
use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
    AsyncRuntimeSessionHandle,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots,
};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Owns only the generated fixture directory, removed after actor/process guards.
struct Directory(PathBuf);

impl Drop for Directory {
    /// Never removes a real user configuration or follows an external root.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Retains actual fixture process incarnations for best-effort panic cleanup.
#[derive(Default)]
struct Processes(Arc<std::sync::Mutex<Vec<mez_mux::process::ProcessParentIdentity>>>);

impl Drop for Processes {
    /// Reobserves birth/parent before numeric signalling; this is not a race-free
    /// kernel PID-handle or independent positive reaping guarantee.
    fn drop(&mut self) {
        for identity in self.0.lock().unwrap().iter().rev() {
            if mez_mux::process::process_parent_identity_for_pid(identity.process_id)
                == Some(*identity)
            {
                // SAFETY: only this observed fixture incarnation is targeted.
                unsafe { libc::kill(identity.process_id as i32, libc::SIGKILL) };
            }
        }
    }
}

/// One installed user package plus a distinct copied project package.
struct Installed {
    /// Isolated ordinary HOME, also containing private sequencing files.
    home: PathBuf,
    /// Working directory whose project resources have an explicit modeled trust decision.
    cwd: PathBuf,
    /// Legitimate optional vendor directory override, never native authority.
    custom: Option<PathBuf>,
    /// Exact authored settings, protected across every bootstrap and callback.
    settings: Vec<u8>,
    /// Independently authored project settings, outside bootstrap ownership.
    project_settings: Vec<u8>,
}

/// Creates a unique private root without discovering real user configuration.
fn directory() -> Directory {
    let path = std::fs::canonicalize("/tmp").unwrap().join(format!(
        "mez-pi-discovery-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    Directory(path)
}

/// Selects already-installed prerequisites; no vendor or dependency is installed.
fn prerequisites() -> (PathBuf, PathBuf, PathBuf) {
    let node = PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit existing Node required"),
    );
    let helper = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mez");
    assert!(node.is_absolute() && node.is_file() && helper.is_file());
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/pi-discovery-ordinary-fixture.mjs")
        .canonicalize()
        .unwrap();
    (node, helper, script)
}

/// Bare production bootstrap honors a normal vendor override without root flags.
fn bootstrap(helper: &Path, installed: &Installed) -> serde_json::Value {
    if let Some(custom) = &installed.custom {
        let output = std::process::Command::new(helper)
            .args(["--json", "bootstrap", "pi"])
            .env_clear()
            .env("HOME", &installed.home)
            .env("PI_CODING_AGENT_DIR", custom)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "custom-root bootstrap failed");
        assert!(output.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["root_source"], "PI_CODING_AGENT_DIR");
        assert_eq!(report["scope_root"], custom.to_str().unwrap());
        assert_eq!(report["lifecycle"], "unverified");
        assert_eq!(report["usage"], "unverified");
        report
    } else {
        bare_bootstrap(helper, &installed.home, false)
    }
}

/// Copies only generated owned package files, not a live user's extension tree.
fn copy_package(source: &Path, target: &Path) {
    std::fs::create_dir(target).unwrap();
    let mut count = 0;
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file());
        std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        count += 1;
        assert!(count <= 32, "generated package exceeded fixture budget");
    }
    assert!(count > 3);
}

/// Installs actual packages with exact authored policy bytes and no manual plan.
fn install(
    helper: &Path,
    home: PathBuf,
    custom: bool,
    user_disabled: bool,
    project_disabled: bool,
) -> Installed {
    std::fs::create_dir(&home).unwrap();
    let cwd = home.join("project");
    std::fs::create_dir(&cwd).unwrap();
    let custom = custom.then(|| home.join("custom agent"));
    let agent = custom.clone().unwrap_or_else(|| home.join(".pi/agent"));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&agent)
        .unwrap();
    let settings = serde_json::to_vec(&serde_json::json!({"authored":"PRIVATE_DISCOVERY_CONTENT", "extensions": if user_disabled {vec!["-extensions/mezzanine/index.mjs"]} else {vec![]}})).unwrap();
    std::fs::write(agent.join("settings.json"), &settings).unwrap();
    let project = cwd.join(".pi");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(project.join("extensions")).unwrap();
    let project_settings = serde_json::to_vec(&serde_json::json!({"authored":"PRIVATE_DISCOVERY_CONTENT", "extensions": if project_disabled {vec!["-extensions/mezzanine/index.mjs"]} else {vec![]}})).unwrap();
    std::fs::write(project.join("settings.json"), &project_settings).unwrap();
    let installed = Installed {
        home,
        cwd,
        custom,
        settings,
        project_settings,
    };
    assert_eq!(bootstrap(helper, &installed)["result"], "installed");
    copy_package(
        &agent.join("extensions/mezzanine"),
        &project.join("extensions/mezzanine"),
    );
    assert_eq!(bootstrap(helper, &installed)["result"], "unchanged");
    assert_settings(&installed);
    installed
}

/// Checks authored settings independently of loader behavior or runtime success.
fn assert_settings(installed: &Installed) {
    let agent = installed
        .custom
        .clone()
        .unwrap_or_else(|| installed.home.join(".pi/agent"));
    assert!(std::fs::read(agent.join("settings.json")).unwrap() == installed.settings);
    assert!(
        std::fs::read(installed.cwd.join(".pi/settings.json")).unwrap()
            == installed.project_settings
    );
}

/// Models only a documented trust decision and synthetic post-retirement fault.
fn policy(installed: &Installed, scenario: &str, trusted: bool, fault: bool) {
    std::fs::write(
        installed.home.join("discovery-policy.json"),
        serde_json::to_vec(&serde_json::json!({
            "scenario":scenario, "projectTrusted":trusted, "failAfterRetirement":fault
        }))
        .unwrap(),
    )
    .unwrap();
}

/// Bounded private phase/result observation, never a capability or producer proof.
async fn wait_file(home: &Path, name: &str, expected: Option<&str>) -> String {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let path = home.join(name);
            if let Ok(metadata) = std::fs::metadata(&path) {
                assert!(metadata.len() <= 64, "fixture marker exceeded bound");
                if let Ok(value) = std::fs::read_to_string(&path)
                    && expected.is_none_or(|expected| value == expected)
                {
                    return value;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ordinary fixture phase/result unavailable")
}

/// Releases only one synthetic vendor phase, without providing any runtime fact.
fn release(home: &Path, phase: &str) {
    std::fs::write(home.join("discovery-next.tmp"), phase).unwrap();
    std::fs::rename(home.join("discovery-next.tmp"), home.join("discovery-next")).unwrap();
}

/// Actual fixture ledger counts and totals, not callback or envelope assertions.
fn ledger(path: &Path) -> (i64, i64, i64, i64, i64) {
    let db = rusqlite::Connection::open(path).unwrap();
    let count = |table: &str| {
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    (
        count("external_usage_requests"),
        count("external_usage_observations"),
        count("external_usage_streams"),
        count("token_usage_events"),
        db.query_row(
            "SELECT coalesce(sum(input_tokens),0) FROM token_usage_events",
            [],
            |row| row.get(0),
        )
        .unwrap(),
    )
}

/// Uses only the actual primary for read-only source surfaces and UI selection.
async fn inspect(
    handle: &AsyncRuntimeSessionHandle,
    primary: &mez_core::ids::ClientId,
    pane: &str,
    accounted: bool,
) {
    handle
        .execute_terminal_command(primary.clone(), format!("select-pane -t {pane}"))
        .await
        .unwrap();
    handle
        .execute_terminal_command(primary.clone(), "agent-shell".into())
        .await
        .unwrap();
    let status = handle
        .execute_agent_shell_command(primary.clone(), "/status".into())
        .await
        .unwrap();
    assert!(status.contains("### External Telemetry"));
    assert!(!status.contains("PRIVATE_DISCOVERY_CONTENT"));
    if accounted {
        assert!(status.contains("actual-model"));
    }
    let list = handle
        .execute_terminal_command(primary.clone(), "list-agents".into())
        .await
        .unwrap();
    assert_eq!(
        browser_pi_panes(&list),
        std::collections::BTreeSet::from(["%1".into(), "%2".into()])
    );
    let config = handle
        .terminal_client_loop_config(Default::default())
        .await
        .unwrap();
    let view = handle
        .render_client_view(
            crate::runtime::ClientViewRole::Primary,
            mez_mux::layout::Size::new(120, 40).unwrap(),
            config,
        )
        .await
        .unwrap()
        .unwrap();
    for title in ["Pi pane one", "Pi pane two"] {
        assert!(
            view.lines.iter().any(|line| line.contains(title)),
            "user title was lost"
        );
    }
    assert!(
        handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .is_empty()
    );
    handle
        .execute_terminal_command(primary.clone(), "agent-shell".into())
        .await
        .unwrap();
}

/// Reads the public Harness/Pane columns independently of display names/titles.
/// These presentation labels verify the surface, never grant native authority.
fn browser_pi_panes(response: &str) -> std::collections::BTreeSet<String> {
    let response: serde_json::Value = serde_json::from_str(response).unwrap();
    let outcome = &response["outcomes"][0];
    assert_eq!(outcome["kind"], "display");
    let body = outcome["body"].as_str().unwrap();
    let mut columns = None;
    let mut panes = std::collections::BTreeSet::new();
    for line in body.lines() {
        let cells = line.split('|').map(str::trim).collect::<Vec<_>>();
        if let (Some(harness), Some(pane)) = (
            cells.iter().position(|cell| *cell == "Harness"),
            cells.iter().position(|cell| *cell == "Pane"),
        ) {
            columns = Some((harness, pane));
        } else if let Some((harness, pane)) = columns
            && cells.get(harness) == Some(&"pi")
        {
            panes.insert(cells.get(pane).unwrap().to_string());
        }
    }
    assert!(columns.is_some(), "browser columns unavailable");
    panes
}

/// One actual source must enroll/charge/end once despite distinct module copies.
fn assert_wire(audit: &WireAudit) -> serde_json::Value {
    let requests = wire_frames(&audit.requests);
    let replies = wire_frames(&audit.replies);
    for method in [
        "agent/external/enroll",
        "agent/external/usage-end",
        "agent/external/source-end",
    ] {
        assert_eq!(
            requests
                .iter()
                .filter(|row| row["method"] == method)
                .count(),
            1,
            "unexpected exchange count for {method}"
        );
    }
    // Initial admission and pre-settlement revalidation repeat one marker;
    // neither another request nor a resampled origin may be introduced.
    let begins = requests
        .iter()
        .filter(|row| row["method"] == "agent/external/usage-begin")
        .collect::<Vec<_>>();
    assert_eq!(begins.len(), 2);
    assert!(begins[0]["params"] == begins[1]["params"]);
    let first_ready = replies
        .iter()
        .find(|row| row["id"] == begins[0]["id"])
        .unwrap();
    let confirmed_ready = replies
        .iter()
        .find(|row| row["id"] == begins[1]["id"])
        .unwrap();
    assert_eq!(first_ready["result"]["accepted"], true);
    assert!(
        first_ready["result"] == confirmed_ready["result"],
        "original readiness changed during settlement revalidation"
    );
    assert!(requests.iter().all(|row| matches!(
        row["method"].as_str(),
        Some(
            "agent/external/enroll"
                | "agent/external/pi-observation"
                | "agent/external/usage-begin"
                | "agent/external/usage-end"
                | "agent/external/source-end"
        )
    )));
    let enrolled = replies
        .iter()
        .find(|row| row["result"]["registered"] == true)
        .unwrap();
    assert!(enrolled["result"]["controls"] == serde_json::json!([]));
    let report = requests
        .iter()
        .find(|row| row["method"] == "agent/external/usage-end")
        .unwrap();
    assert_eq!(report["params"]["model"], "actual-model");
    assert_eq!(report["params"]["counters"]["input_tokens"], 10);
    let receipt = replies
        .iter()
        .find(|row| row["id"] == report["id"])
        .unwrap();
    assert_eq!(receipt["result"]["durable"], true);
    assert_eq!(receipt["result"]["applied"], true);
    let ending = requests.last().unwrap();
    assert_eq!(ending["method"], "agent/external/source-end");
    let receipt = replies
        .iter()
        .find(|row| row["id"] == ending["id"])
        .unwrap();
    assert_eq!(receipt["result"]["protocol"], "external-source-end/1");
    assert_eq!(receipt["result"]["durable"], true);
    assert_eq!(receipt["result"]["retired"], true);
    for sentinel in [
        "PRIVATE_DISCOVERY_CONTENT",
        "same-entry-in-both-panes",
        "selected-model",
    ] {
        assert!(!String::from_utf8_lossy(&audit.requests).contains(sentinel));
    }
    let event_id = report["params"]["event_id"].as_str().unwrap();
    assert_eq!(event_id.len(), 64);
    serde_json::json!(event_id)
}

/// Shared two-source composition; the negative case faults only after its real
/// source-end receipt/EOF, so an earlier error cannot qualify the regression.
async fn qualify(fault: bool) {
    let directory = directory();
    let processes = Processes::default();
    let (node, helper, script) = prerequisites();
    let a = install(&helper, directory.0.join("one"), false, false, false);
    let b = install(&helper, directory.0.join("two"), true, false, false);
    policy(&a, "observe", true, fault);
    policy(&b, "observe", true, false);
    let bin = directory.0.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let shim = bin.join("pi");
    // The ordinary shell owns/waits its actual Node child and publishes its real
    // status. These private files cannot supply native enrollment authority.
    std::fs::write(&shim, format!("#!/bin/sh\n{} {} &\nchild=$!\nwait \"$child\"\nresult=$?\nprintf '%s\\n' \"$result\" > \"$HOME/discovery-exit.tmp\"\nmv \"$HOME/discovery-exit.tmp\" \"$HOME/discovery-exit\"\nexit \"$result\"\n", shlex::try_quote(node.to_str().unwrap()).unwrap(), shlex::try_quote(script.to_str().unwrap()).unwrap())).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.0.join("control.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
    let usage = directory.0.join("usage.sqlite");
    let mut service = RuntimeServiceFixture::new().control_socket(&path).build();
    let primary = service
        .attach_primary(
            "discovery-user",
            true,
            mez_mux::layout::Size::new(120, 40).unwrap(),
            120,
        )
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .split_pane_with_process(&primary, mez_mux::layout::SplitDirection::Vertical, None)
        .unwrap();
    service.session.select_pane_global(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let native = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service.toggle_active_agent_shell().unwrap();
    let mut roots = std::collections::BTreeMap::new();
    for (pane, title) in [("%1", "Pi pane one"), ("%2", "Pi pane two")] {
        service
            .session
            .set_pane_title_explicit(pane, title)
            .unwrap();
        let root = mez_mux::process::process_parent_identity_for_pid(
            service.primary_pid_for_live_pane_process(pane).unwrap(),
        )
        .unwrap();
        processes.0.lock().unwrap().push(root);
        roots.insert(pane, root);
    }
    service.set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
    for (pane, installed) in [("%1", &a), ("%2", &b)] {
        let custom = installed
            .custom
            .as_ref()
            .map(|path| {
                format!(
                    "PI_CODING_AGENT_DIR={} ",
                    shlex::try_quote(path.to_str().unwrap()).unwrap()
                )
            })
            .unwrap_or_default();
        let command = format!(
            "unset PI_CODING_AGENT_DIR; cd {} && HOME={} {}PATH={}:$PATH pi\n",
            shlex::try_quote(installed.cwd.to_str().unwrap()).unwrap(),
            shlex::try_quote(installed.home.to_str().unwrap()).unwrap(),
            custom,
            shlex::try_quote(bin.to_str().unwrap()).unwrap()
        );
        service
            .write_runtime_pane_input(pane, command.as_bytes())
            .unwrap();
    }
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut actors = tokio::task::JoinSet::new();
    actors.spawn(actor.run());
    let mut transports = tokio::task::JoinSet::new();
    for _ in 0..2 {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let uid = crate::runtime::current_effective_uid();
        let origin =
            Arc::new(crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid).unwrap());
        assert_eq!(
            mez_mux::process::process_executable_identity_for_pid(origin.identity.process_id)
                .unwrap()
                .executable_path,
            node.canonicalize().unwrap()
        );
        let shim =
            mez_mux::process::process_parent_identity_for_pid(origin.identity.parent_process_id)
                .unwrap();
        assert!(
            roots
                .values()
                .any(|root| root.process_id == shim.parent_process_id)
        );
        let source_root = shim.parent_process_id;
        processes.0.lock().unwrap().extend([shim, origin.identity]);
        let audit = Arc::new(std::sync::Mutex::new(WireAudit::default()));
        let handle = handle.clone();
        transports.spawn(async move {
            let mut connection = ControlConnectionState::new(true, false);
            connection
                .bind_authenticated_peer(AuthenticatedPeer::unix_user(uid))
                .unwrap();
            connection.bind_unix_origin(origin.clone()).unwrap();
            let qualified =
                crate::runtime::UnixOriginStream::new(&mut socket, Some(origin.clone()));
            let mut io = AuditedIo {
                inner: qualified,
                audit: audit.clone(),
            };
            let served = serve_authenticated_async_runtime_control_connection_loop_with_snapshots(
                &mut io,
                AuthenticatedPeer::unix_user(uid),
                &handle,
                &mut connection,
                AsyncRuntimeControlConnectionConfig::new(8192, uid).unwrap(),
                None,
                |_, _| false,
            )
            .await
            .unwrap();
            assert!(served > 0 && origin.writer_confirmed());
            assert!(!connection.initialized() && connection.caller_client_id().is_none());
            (origin, audit, source_root)
        });
    }
    for phase in ["registered", "accounted"] {
        wait_file(&a.home, "discovery-phase", Some(phase)).await;
        wait_file(&b.home, "discovery-phase", Some(phase)).await;
        for pane in ["%1", "%2"] {
            inspect(&handle, &primary, pane, phase == "accounted").await;
        }
        if phase == "accounted" {
            assert_eq!(ledger(&usage), (2, 2, 2, 2, 20));
        }
        if phase == "registered" {
            release(&a.home, phase);
            release(&b.home, phase);
        }
    }
    // End A first while B remains blocked at its accounted phase.
    release(&a.home, "accounted");
    wait_file(&a.home, "discovery-phase", Some("retired")).await;
    let list = handle
        .execute_terminal_command(primary.clone(), "list-agents".into())
        .await
        .unwrap();
    assert_eq!(
        browser_pi_panes(&list),
        std::collections::BTreeSet::from(["%2".into()])
    );
    assert_eq!(ledger(&usage), (2, 2, 2, 2, 20));
    release(&a.home, "retired");
    let a_status = wait_file(&a.home, "discovery-exit", None).await;
    let a_result = wait_file(&a.home, "discovery-result", None).await;
    assert_eq!(a_status.trim(), if fault { "1" } else { "0" });
    assert_eq!(
        a_result,
        if fault {
            "post-retirement-failure"
        } else {
            "complete"
        }
    );
    release(&b.home, "accounted");
    wait_file(&b.home, "discovery-phase", Some("retired")).await;
    release(&b.home, "retired");
    assert_eq!(wait_file(&b.home, "discovery-exit", None).await.trim(), "0");
    assert_eq!(
        wait_file(&b.home, "discovery-result", None).await,
        "complete"
    );
    let mut reports = Vec::new();
    while let Some(report) = transports.join_next().await {
        reports.push(report.unwrap());
    }
    assert_eq!(reports.len(), 2);
    let ids = reports
        .iter()
        .map(|(_, audit, _)| assert_wire(&audit.lock().unwrap()))
        .collect::<Vec<_>>();
    assert!(
        ids[0] == ids[1],
        "fixture did not exercise equal persisted entry identities"
    );
    assert_eq!(ledger(&usage), (2, 2, 2, 2, 20));
    handle.shutdown().await.unwrap();
    let mut exit = actors.join_next().await.unwrap().unwrap();
    assert_eq!(exit.service.session().clients().len(), 1);
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        native
    );
    assert!(exit.service.agent_turn_ledger().turns().is_empty());
    let bindings = &exit.service.control.external_agents().bindings;
    assert_eq!(bindings.len(), 2);
    let mut owners = std::collections::BTreeSet::new();
    for (pane, _) in [("%1", &a), ("%2", &b)] {
        let binding = bindings.values().find(|row| row.pane_id == pane).unwrap();
        assert!(binding.retired && binding.harness == "pi");
        let (_, _, observed_root) = reports
            .iter()
            .find(|(origin, _, _)| {
                binding
                    .enrollment
                    .as_ref()
                    .unwrap()
                    .producer
                    .matches_socket(origin)
            })
            .unwrap();
        assert_eq!(*observed_root, roots[pane].process_id);
        assert_eq!(binding.process.process_id, roots[pane].process_id);
        owners.insert(binding.accounting_owner.clone());
        assert!(
            processes
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|root| root.process_id == binding.process.process_id
                    && mez_mux::process::process_parent_identity_for_pid(root.process_id)
                        == Some(*root)),
            "original pane root did not survive child completion"
        );
    }
    assert_eq!(owners.len(), 2);
    assert!(exit.service.external_agent_rows().is_empty());
    assert_eq!(
        exit.service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_settings(&a);
    assert_settings(&b);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Two actual pane roots and distinct user/project modules must retain separate
/// native owners despite equal session/entry labels. End A while B stays live;
/// both actual shell exits and final assertions precede fixture teardown.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit installed Node and fresh sibling mez required; offline two-pane installed discovery"]
async fn external_pi_two_panes_distinct_packages_keep_accounting_and_retirement_independent() {
    tokio::time::timeout(Duration::from_secs(60), qualify(false))
        .await
        .unwrap();
}

/// An actual fault after source-end/EOF cannot qualify as ordinary completion.
/// The same real two-pane path must reach its exact fault stage and shell exit1,
/// while B still completes and both original expenses/retirements remain intact.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit installed Node and fresh sibling mez required; post-retirement child failure"]
async fn external_pi_two_panes_reject_post_retirement_child_failure_as_completion() {
    tokio::time::timeout(Duration::from_secs(60), qualify(true))
        .await
        .unwrap();
}

/// Source-shaped discovery honors a legitimate custom directory, modeled
/// project trust and exact authored disabled paths. Actual installed factories
/// remain neutral outside Mez or with an unavailable configured daemon. This
/// is not real SDK, general policy-glob or vendor trust-database certification.
#[test]
#[ignore = "explicit installed Node and fresh sibling mez required; offline discovery policy/neutrality"]
fn external_pi_installed_discovery_preserves_policy_custom_roots_and_neutrality() {
    let directory = directory();
    let (node, helper, script) = prerequisites();
    for (name, custom, trusted, user_disabled, project_disabled, scenario) in [
        ("outside", false, true, false, false, "outside"),
        ("custom", true, false, false, false, "outside"),
        ("untrusted", true, false, true, false, "disabled"),
        ("disabled", false, true, true, true, "disabled"),
        ("outage", true, true, false, false, "outage"),
    ] {
        let installed = install(
            &helper,
            directory.0.join(name),
            custom,
            user_disabled,
            project_disabled,
        );
        policy(&installed, scenario, trusted, false);
        let mut command = std::process::Command::new(&node);
        command
            .arg(&script)
            .env_clear()
            .env("HOME", &installed.home)
            .current_dir(&installed.cwd)
            .stdin(std::process::Stdio::null());
        if let Some(custom) = &installed.custom {
            command.env("PI_CODING_AGENT_DIR", custom);
        }
        if scenario != "outside" {
            command
                .env(
                    "MEZ",
                    format!(
                        "{}\x1fsession=fixture\x1fwindow=@1\x1fpane=%1\x1fprotocol=mez-control/1",
                        directory.0.join("missing.sock").display()
                    ),
                )
                .env("MEZ_PANE", "%1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "discovery/neutrality child failed: {name}"
        );
        assert_eq!(output.stdout, b"PI_DISCOVERY_COMPLETE\n");
        assert!(output.stderr.is_empty());
        assert_eq!(
            std::fs::read(installed.home.join("discovery-result")).unwrap(),
            b"complete"
        );
        assert_settings(&installed);
        assert_eq!(bootstrap(&helper, &installed)["result"], "unchanged");
    }
}

/// Unexpected activation values must fail before HOME/discovery with a static
/// diagnostic. Owned synthetic sentinels make a value-echoing assertion fail
/// this check without reading any private user configuration or opening a socket.
#[test]
#[ignore = "explicit installed Node required; fixture guard privacy"]
fn external_pi_discovery_setup_rejects_private_activation_without_value_echo() {
    let node = PathBuf::from(
        std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit existing Node required"),
    );
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/pi-discovery-ordinary-fixture.mjs")
        .canonicalize()
        .unwrap();
    let sentinel = "SYNTHETIC_PI_DISCOVERY_PRIVATE_SENTINEL_NOT_A_CREDENTIAL";
    for key in [
        "MEZ_HARNESS_SESSION",
        "MEZ_HARNESS_OBSERVER_FD",
        "MEZ_HARNESS_HELPER",
        "MEZ_EXTERNAL_LAUNCH_TOKEN",
    ] {
        let output = std::process::Command::new("timeout")
            .arg("120")
            .arg(&node)
            .arg(&script)
            .env_clear()
            .env(key, sentinel)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected activation input"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(sentinel));
    }
}

/// An inherited agent directory outside the generated HOME, including a link
/// escaping that HOME, must be rejected before evaluating package code. Both
/// targets are fixture-owned; a harmless import marker independently detects a
/// missing containment fence without touching any real user's configuration.
#[test]
#[ignore = "explicit installed Node and fresh sibling mez required; private discovery containment"]
fn external_pi_discovery_refuses_external_and_symlinked_agent_directories() {
    let directory = directory();
    let (node, helper, script) = prerequisites();
    let installed = install(&helper, directory.0.join("home"), false, false, false);
    policy(&installed, "outside", false, false);
    let external = directory.0.join("external-agent");
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join("settings.json"), b"{}").unwrap();
    std::fs::create_dir(external.join("extensions")).unwrap();
    let package = external.join("extensions/mezzanine");
    std::fs::create_dir(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        br#"{"pi":{"extensions":["index.mjs"]}}"#,
    )
    .unwrap();
    let marker = external.join("imported");
    std::fs::write(package.join("index.mjs"),format!("import {{writeFileSync}} from 'node:fs'; writeFileSync({}, 'loaded'); export default function() {{}}\n",serde_json::json!(marker.to_str().unwrap()))).unwrap();
    let link = installed.home.join("linked-agent");
    std::os::unix::fs::symlink(&external, &link).unwrap();
    for (path, diagnostic) in [
        (&external, "discovery path outside fixture home"),
        (&link, "discovery link outside fixture home"),
    ] {
        let output = std::process::Command::new("timeout")
            .arg("120")
            .arg(&node)
            .arg(&script)
            .env_clear()
            .env("HOME", &installed.home)
            .env("PI_CODING_AGENT_DIR", path)
            .current_dir(&installed.cwd)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(external.to_str().unwrap()));
        assert!(!marker.exists(), "external package code was evaluated");
    }
}
