//! Actual Codex-shaped producer replacement beneath one retained native pane.
//!
//! Kernel lifetime anchors prove creator exit and replacement, while the normal
//! PTY shell and shared SQL ledger remain unchanged. This tests native producer
//! restart/resume, not preservation of PTYs across a full Mez daemon restart or
//! actual vendor/provider execution. Public session labels never recover custody.

use super::codex_children::event;
use super::codex_observation::{callback, declaration};
use super::helper_presentation::{child, release, settle};
use super::*;

/// An exact session end has no fabricated turn, generation or child selector.
fn end() -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"end",
        "method":"agent/external/codex-observe","params":{"pane_id":"%1","external_session_id":"session-a","event":"SessionEnd"}}).to_string()).unwrap()
}

/// A real creator exits after one committed main request and one pending child
/// admission. Its queued Post must fail before expense, and an old end helper
/// cannot retire a replacement. A normally invoked successor resumes the same
/// public session beneath the same live pane root, yet receives a distinct
/// durable native owner. Unready parent/child outcomes cannot borrow history;
/// only fresh genuine Pre boundaries charge new work. Already recorded expense
/// remains under the old owner, and every owned helper is released explicitly.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_native_creator_restart_resume_never_adopts_old_custody() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Codex restart qualification requires native peer evidence");
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let root = fixture.service.pane_process_identity("%1").unwrap();
    let original_creator = fixture.connection.unix_origin().unwrap().clone();
    let mut helpers = Vec::new();
    let (socket, source) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &source)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["registered"],
        true
    );
    let owner = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .accounting_owner
        .clone();
    for request in [
        callback("UserPromptSubmit", "same"),
        event("SubagentStart", Some("child"), "same", None),
    ] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&request, &source)
            .unwrap();
        assert!(
            settle(&mut fixture, work, &source)
                .await
                .get("error")
                .is_none()
        );
    }
    helpers.push((socket, source));
    for request in [
        event("PreModelRequest", None, "same", None),
        event("PostModelRequest", None, "same", Some(10)),
        event("PreModelRequest", Some("child"), "same", None),
    ] {
        let (socket, connection) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_helper_presentation(&request, &connection)
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_none()
        );
        helpers.push((socket, connection));
    }
    let (old_post_socket, old_post) = child(&mut fixture).await;
    let pending = fixture
        .service
        .prepare_external_helper_presentation(
            &event("PostModelRequest", Some("child"), "same", Some(999)),
            &old_post,
        )
        .unwrap();
    release(&mut fixture.socket, &fixture.connection).await;
    assert!(
        !original_creator.is_live(),
        "socket closure is not creator death proof"
    );
    assert!(
        fixture
            .service
            .pane_process_identity("%1")
            .unwrap()
            .same_incarnation(&root)
    );
    assert!(
        Instant::now() < pending.deadline,
        "qualify native loss before the work deadline"
    );
    let refused = settle(&mut fixture, pending, &old_post).await;
    assert!(refused.get("error").is_some(), "{refused}");
    assert_ne!(refused["result"]["durable"], true);
    fixture.service.reconcile_external_agent_registrations();
    assert!(fixture.service.external_agent_rows().is_empty());
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );

    let (socket, connection) =
        launch_ordinary_fixture(&mut fixture.service, &fixture.listener, "hold")
            .await
            .unwrap();
    fixture.socket = socket;
    fixture.connection = connection;
    let successor = fixture.connection.unix_origin().unwrap().identity;
    assert_ne!(
        (successor.process_id, successor.start_token),
        (
            original_creator.identity.process_id,
            original_creator.identity.start_token
        )
    );
    assert!(
        fixture
            .service
            .pane_process_identity("%1")
            .unwrap()
            .same_incarnation(&root)
    );
    let (socket, source) = child(&mut fixture).await;
    let mut resume = declaration();
    let mut params: serde_json::Value =
        serde_json::from_str(resume.params.as_deref().unwrap()).unwrap();
    params["session_boundary"] = "resume".into();
    resume.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&resume, &source)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["registered"],
        true
    );
    let next_owner = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .find(|binding| !binding.retired)
        .unwrap()
        .accounting_owner
        .clone();
    assert_ne!(next_owner, owner);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "same"), &source)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &source)
            .await
            .get("error")
            .is_none()
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&end(), &old_post)
            .is_err()
    );

    for id in [None, Some("child")] {
        let (socket, connection) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &event("PostModelRequest", id, "same", Some(999)),
                &connection,
            )
            .unwrap();
        let unready = settle(&mut fixture, work, &connection).await;
        assert_eq!(unready["result"]["observed"], true);
        assert_ne!(unready["result"]["durable"], true);
        helpers.push((socket, connection));
    }
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &event("SubagentStart", Some("child"), "same", None),
            &source,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["changed"],
        true
    );
    for (id, tokens) in [(None, 10), (Some("child"), 7)] {
        for request in [
            event("PreModelRequest", id, "same", None),
            event("PostModelRequest", id, "same", Some(tokens)),
        ] {
            let (socket, connection) = child(&mut fixture).await;
            let work = fixture
                .service
                .prepare_external_helper_presentation(&request, &connection)
                .unwrap();
            let reply = settle(&mut fixture, work, &connection).await;
            assert!(reply.get("error").is_none(), "{reply}");
            if request
                .params
                .as_ref()
                .unwrap()
                .contains("PostModelRequest")
            {
                assert_eq!(reply["result"]["durable"], true, "{reply}");
            }
            helpers.push((socket, connection));
        }
    }
    let database = rusqlite::Connection::open(path).unwrap();
    for (expected, source_owner) in [(10, owner.as_str()), (17, next_owner.as_str())] {
        let rows = database.prepare("SELECT DISTINCT s.id,s.totals FROM external_usage_streams s JOIN external_usage_observations o ON o.stream_id=s.id WHERE o.owner=?1").unwrap()
            .query_map([source_owner],|row|row.get::<_,String>(1)).unwrap().collect::<std::result::Result<Vec<_>,_>>().unwrap();
        let tokens: u64 = rows
            .iter()
            .map(|raw| {
                serde_json::from_str::<crate::storage::token_usage::ExternalCounters>(raw)
                    .unwrap()
                    .input_tokens
            })
            .sum();
        assert_eq!(tokens, expected);
    }
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        27
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        4
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(fixture.service.external_agent_rows().len(), 1);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .filter(|binding| !binding.retired)
            .all(|binding| binding.accounting_owner == next_owner)
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&end(), &source)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["retired"],
        true
    );
    assert!(fixture.service.external_agent_rows().is_empty());
    assert!(fixture.connection.unix_origin().unwrap().is_live());
    helpers.push((old_post_socket, old_post));
    helpers.push((socket, source));
    for (mut socket, connection) in helpers {
        release(&mut socket, &connection).await;
    }
    release(&mut fixture.socket, &fixture.connection).await;
}
