//! Accounting across actual detached daemon and ordinary producer replacement.
//!
//! Real binaries, PTYs, Unix peer credentials and normal RPC workers qualify
//! expense. Only public source selectors and normalized fixture reports cross
//! process lifetimes; private enrollment capabilities remain in child memory.
//! Restored panes/new producers cannot borrow an old native source. This tests
//! committed history and conservative exclusion, not a durable unsent outbox,
//! vendor SDK callbacks, crash/power-loss recovery or no-gap billing.

use super::*;
use serde_json::{Value, json};
use std::os::unix::net::UnixStream;

/// Stops and reaps the real daemon, then starts a new daemon against the same
/// private home/ledger. A real new pane child cannot cold-claim its predecessor
/// or submit an old unready outcome. Its genuine new request charges once;
/// original committed expense remains under the original owner without replay.
#[test]
fn foreground_accounting_daemon_restart_preserves_expense_and_fences_old_source() {
    let root = test_root("accounting-restart");
    let home = root.join("home");
    let runtime = root.join("runtime");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime.join("accounting.sock");
    let ledger = home.join(".config/mezzanine/token-usage.sqlite");
    let mut original_event = None;
    for phase in ["first", "second"] {
        let mut daemon = spawn_detached_serve(&home, &runtime, &socket);
        socket_readiness::wait_for_socket(&mut daemon, &socket, Duration::from_secs(15)).unwrap();
        let mut terminal = spawn_foreground_attach(&root, &home, &runtime, &socket);
        let mut output = Vec::new();
        terminal
            .read_until(&mut output, Duration::from_secs(15), |text| {
                contains_default_shell_pane_frame(text) && text.contains('$')
            })
            .unwrap();
        let executable = std::env::current_exe().unwrap();
        let command = format!(
            "MEZ_ACCOUNTING_RESTART_ROOT={} MEZ_ACCOUNTING_RESTART_PHASE={} {} --exact external_accounting::foreground_accounting_ordinary_child_fixture --ignored --quiet\n",
            shlex::try_quote(root.to_str().unwrap()).unwrap(),
            phase,
            shlex::try_quote(executable.to_str().unwrap()).unwrap(),
        );
        output.clear();
        terminal.write_input(command.as_bytes()).unwrap();
        let result_path = root.join(format!("{phase}.json"));
        terminal
            .read_until(&mut output, Duration::from_secs(30), |_| {
                result_path.is_file()
            })
            .unwrap();
        let result: Value = serde_json::from_slice(&fs::read(result_path).unwrap()).unwrap();
        assert_eq!(result["durable"], true);
        assert_eq!(result["duplicate_applied"], false);
        if phase == "second" {
            assert_eq!(result["old_source_refused"], true);
            assert_eq!(result["old_unready_report_refused"], true);
            let first: Value =
                serde_json::from_slice(&fs::read(root.join("first.json")).unwrap()).unwrap();
            assert_ne!(result["source_witness"], first["source_witness"]);
        }
        let sql = rusqlite::Connection::open_with_flags(
            &ledger,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let events = event_rows(&sql);
        if phase == "first" {
            assert_eq!(events.len(), 1);
            original_event = Some(events[0].clone());
        } else {
            assert_eq!(events.len(), 2);
            assert!(
                events.contains(original_event.as_ref().unwrap()),
                "restart changed original incurred history"
            );
        }
        let attempt: String = sql.query_row(
            "SELECT lower(hex(a.attempt)) FROM external_usage_source_activations a JOIN external_usage_observations o ON o.owner=a.owner WHERE o.event_id=?1",
            [result["report"]["event_id"].as_str().unwrap()], |row| row.get(0)
        ).unwrap();
        assert_eq!(
            attempt,
            result["source_receipt"]["activation_id"].as_str().unwrap()
        );
        let expected = if phase == "first" { 1 } else { 2 };
        assert_eq!(
            sql.query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            expected
        );
        assert_eq!(
            sql.query_row(
                "SELECT count(DISTINCT owner) FROM external_usage_observations",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            expected
        );
        assert_eq!(
            sql.query_row("SELECT count(*) FROM external_usage_sources", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            expected
        );
        drop(sql);
        // Normal mux shutdown and exact owned-process reap precede replacement.
        terminal.write_input(b"\x01:exit\r").unwrap();
        terminal
            .read_until_exit(&mut output, Duration::from_secs(15))
            .unwrap();
        wait_for_process_exit(&mut daemon, Duration::from_secs(15)).unwrap();
    }
}

/// Invoked only as a genuine pane descendant by the owning integration test.
/// Native enrollment/report RPCs use its real socket writer; no inherited
/// observer descriptor, injected PID/source mapping or private file capability
/// supplies authority. Missing routing/qualification fails rather than skips.
#[test]
#[ignore = "self-executing ordinary accounting producer fixture"]
fn foreground_accounting_ordinary_child_fixture() {
    let Some(root) = std::env::var_os("MEZ_ACCOUNTING_RESTART_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let phase = std::env::var("MEZ_ACCOUNTING_RESTART_PHASE").unwrap();
    assert!(matches!(phase.as_str(), "first" | "second"));
    let discovery = std::env::var("MEZ").unwrap();
    let mut fields = discovery.split('\x1f');
    let socket = fields.next().unwrap();
    let pane = fields
        .find_map(|field| field.strip_prefix("pane="))
        .unwrap();
    let mut wire = Wire::connect(socket);
    let mut old_source_refused = false;
    let mut old_unready_report_refused = false;
    if phase == "second" {
        let first: Value =
            serde_json::from_slice(&fs::read(root.join("first.json")).unwrap()).unwrap();
        let mut resume = enrollment(pane, "restart-first", "restart-cold");
        resume["durable_source_resume"] =
            json!({"receipt":first["source_receipt"], "operation_id":"e".repeat(64)});
        validate_source_receipt(&first["source_receipt"]);
        let refused = wire.exchange("agent/external/enroll", resume);
        assert_eq!(refused["error"]["code"], -32006);
        // The public cold boundary intentionally withholds native/store details.
        // Valid receipt/schema and the later fresh-source success distinguish
        // this rejection from malformed selectors or an unavailable method.
        assert_eq!(
            refused["error"]["message"],
            "durable source publication unavailable"
        );
        old_source_refused = true;
        assert!(
            old_source_refused,
            "a new native producer recovered the previous source"
        );
    }
    let session = if phase == "first" {
        "restart-first"
    } else {
        "restart-second"
    };
    let mut accepted =
        result(wire.exchange("agent/external/enroll", enrollment(pane, session, session)));
    assert_eq!(accepted["registered"], true);
    // Only genuine authenticated renewals can export a delayed public checkpoint.
    for pause in [0, 100, 250] {
        if accepted.get("source_receipt").is_some() {
            break;
        }
        if pause > 0 {
            thread::sleep(Duration::from_millis(pause));
        }
        let renewal = result(wire.exchange("agent/external/renew", selectors(&accepted, session)));
        if let Some(receipt) = renewal.get("source_receipt") {
            accepted["source_receipt"] = receipt.clone();
        }
    }
    validate_source_receipt(&accepted["source_receipt"]);
    if phase == "second" {
        let first: Value =
            serde_json::from_slice(&fs::read(root.join("first.json")).unwrap()).unwrap();
        let mut old = selectors(&accepted, session);
        for field in [
            "request_id",
            "event_id",
            "observed_at",
            "provider",
            "model",
            "counters",
        ] {
            old[field] = first["report"][field].clone();
        }
        let refused = wire.exchange("agent/external/usage-end", old);
        assert_eq!(refused["error"]["code"], -32006);
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("original readiness unavailable")
        );
        old_unready_report_refused = true;
        assert!(old_unready_report_refused);
    }
    let marker = if phase == "first" { "a" } else { "b" }.repeat(64);
    let event = if phase == "first" { "c" } else { "d" }.repeat(64);
    let mut begin = selectors(&accepted, session);
    begin["request_id"] = json!(marker);
    let ready = result(wire.exchange("agent/external/usage-begin", begin.clone()));
    assert_eq!(ready["accepted"], true);
    assert_eq!(ready["protocol"], "external-request/1");
    assert_eq!(ready["request_id"], marker);
    assert!(ready["admitted_at"].as_u64().is_some_and(|clock| clock > 0));
    assert!(digest(&ready["source_witness"]));
    assert!(matches!(
        ready["origin"].as_str(),
        Some("project" | "unattributed")
    ));
    assert_eq!(ready["controls"], json!([]));
    assert_eq!(ready.as_object().unwrap().len(), 7);
    let report = json!({"request_id":marker,"event_id":event,"observed_at":ready["admitted_at"],"provider":"fixture","model":"service-test","counters":{"input_tokens":1,"output_tokens":0}});
    let mut end = selectors(&accepted, session);
    for field in [
        "request_id",
        "event_id",
        "observed_at",
        "provider",
        "model",
        "counters",
    ] {
        end[field] = report[field].clone();
    }
    let ack = result(wire.exchange("agent/external/usage-end", end.clone()));
    assert_eq!(ack["accepted"], true);
    assert_eq!(ack["durable"], true);
    assert!(
        ack["revision"]
            .as_u64()
            .is_some_and(|revision| revision > 0)
    );
    assert_eq!(ack["coverage"]["source"], "external-reported");
    assert_eq!(ack["coverage"]["reasoning_known"], false);
    assert_eq!(ack.as_object().unwrap().len(), 5);
    assert_eq!(ack["coverage"].as_object().unwrap().len(), 2);
    assert_eq!(ack["applied"], true);
    let restored = result(wire.exchange("agent/external/usage-begin", begin));
    assert_eq!(restored, ready);
    let duplicate = result(wire.exchange("agent/external/usage-end", end));
    assert_eq!(duplicate["durable"], true);
    assert_eq!(duplicate["revision"], ack["revision"]);
    assert_eq!(duplicate["applied"], false);
    let public = json!({"source_receipt":accepted["source_receipt"],"source_witness":ready["source_witness"],"report":report,"durable":ack["durable"],"duplicate_applied":duplicate["applied"],"old_source_refused":old_source_refused,"old_unready_report_refused":old_unready_report_refused});
    let pending = root.join(format!("{phase}.pending"));
    fs::write(&pending, serde_json::to_vec(&public).unwrap()).unwrap();
    fs::rename(pending, root.join(format!("{phase}.json"))).unwrap();
}

/// Validates the exported closed public selector before attempting recovery;
/// malformed metadata or method absence cannot masquerade as a native refusal.
fn validate_source_receipt(receipt: &Value) {
    assert_eq!(receipt["protocol"], "external-source-activation/1");
    assert_eq!(receipt.as_object().unwrap().len(), 3);
    assert!(digest(&receipt["activation_id"]));
    let generation = receipt["observer_generation"].as_str().unwrap();
    let parsed = generation.parse::<u64>().unwrap();
    assert!(parsed > 0);
    assert_eq!(parsed.to_string(), generation);
}

/// Checks only canonical public opaque digests, never a capability or PID.
fn digest(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Reads actual incurred rows for exact history conservation across restart.
/// No fixture SQL creates a source, marker, alias, checkpoint or expense.
fn event_rows(sql: &rusqlite::Connection) -> Vec<Vec<rusqlite::types::Value>> {
    let mut query = sql
        .prepare("SELECT * FROM token_usage_events ORDER BY id")
        .unwrap();
    let columns = query.column_count();
    query
        .query_map([], |row| {
            (0..columns)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

/// Constructs only the fixed ordinary persistent source declaration.
fn enrollment(pane: &str, session: &str, instance: &str) -> Value {
    json!({"pane_id":pane,"harness":"pi","version":"fixture","display_name":"Accounting fixture","external_session_id":session,"observer_instance":instance,"observer_kind":"persistent"})
}

/// Private credentials stay in this child's memory and live socket only.
fn selectors(accepted: &Value, session: &str) -> Value {
    json!({"launch_token":accepted["launch_token"],"generation":accepted["generation"],"external_session_id":session})
}

/// Reports only the public bounded error disposition, never a private reply.
fn result(reply: Value) -> Value {
    assert!(
        reply.get("error").is_none(),
        "accounting RPC refused: code={:?}, message={:?}",
        reply["error"]["code"].as_i64(),
        reply["error"]["message"].as_str()
    );
    assert!(reply["result"].is_object());
    reply["result"].clone()
}

/// One bounded serialized ordinary Unix client; the production daemon owns all
/// authentication, native evidence, storage and typed acknowledgment decisions.
struct Wire {
    socket: UnixStream,
    pending: Vec<u8>,
    next: u64,
}
impl Wire {
    /// Opens the real discovered endpoint without acquiring any client role.
    fn connect(path: &str) -> Self {
        let socket = UnixStream::connect(path).unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Self {
            socket,
            pending: Vec::new(),
            next: 0,
        }
    }
    /// Sends one fixed fixture RPC and requires its exact framed response within
    /// one total read horizon. Unrelated responses cannot count as durable ACKs.
    fn exchange(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        self.socket
            .write_all(&encode_control_body(
                &json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params})
                    .to_string(),
            ))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok((body, consumed)) = decode_control_frame(&self.pending, 16384) {
                self.pending.drain(..consumed);
                let reply: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(reply["id"], self.next);
                return reply;
            }
            assert!(self.pending.len() <= 16384);
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("accounting response deadline elapsed");
            self.socket.set_read_timeout(Some(remaining)).unwrap();
            let mut bytes = [0; 4096];
            let read = self.socket.read(&mut bytes).unwrap();
            assert!(read > 0, "accounting transport closed before response");
            self.pending.extend_from_slice(&bytes[..read]);
        }
    }
}
