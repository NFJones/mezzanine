//! Genuine native child starts and shared original request accounting fences.
//!
//! Child labels narrow only an already-qualified parent creator/session. Actual
//! kernel helper writer/parent/root proof, original request custody and SQL decide
//! effects; child facts cannot repaint, renew or retire their parent source.

use super::codex_observation::{callback, declaration};
use super::helper_presentation::{child, settle};
use super::*;

/// Builds only public normalized child selectors and optional checked counters.
fn event(kind: &str, id: Option<&str>, turn: &str, tokens: Option<u64>) -> JsonRpcRequest {
    let mut request = callback(kind, turn);
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    if let Some(id) = id {
        params["agent_id"] = id.into();
    }
    if kind == "PostModelRequest" {
        params["counters"] = serde_json::json!({"input_tokens":tokens.unwrap(),"output_tokens":1});
    }
    request.params = Some(params.to_string());
    request
}

/// Parent and sibling child turns can share the exact public turn label without
/// sharing custody. Unknown children cannot admit readiness; a genuine
/// Stop allows original incurred Post and later continuation requests. Every child
/// fact remains inert for parent presentation/lease, and new child turns fence
/// retired turns without requiring a fabricated parent UserPromptSubmit.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_children_native_requests_isolate_siblings_and_parent() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("child qualification requires native evidence")
    };
    let path = fixture.directory.join("usage.sqlite");
    let store = crate::storage::token_usage::TokenUsageStore::new(&path);
    let project = fixture.directory.join("child-project");
    std::fs::create_dir(&project).unwrap();
    let mut trust = crate::security::project::ProjectTrustStore::default();
    trust
        .decide(
            project.clone(),
            crate::security::project::TrustDecision::Trusted,
            None,
        )
        .unwrap();
    let inventory = store
        .prepare_accounting_projects(&trust.records().cloned().collect::<Vec<_>>())
        .unwrap();
    let project_id = inventory[0].id.clone().unwrap();
    fixture.service.set_project_trust_store(trust, None);
    fixture.service.set_token_usage_store(store.clone());
    fixture
        .service
        .persistence
        .install_accounting_projects(&store, Some(inventory));
    let (_source_socket, source) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &source)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["registered"],
        true
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "shared"), &source)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &source)
            .await
            .get("error")
            .is_none()
    );
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    let original = (
        binding.generation,
        binding.accounting_owner.clone(),
        binding.expires,
    );
    let original_rows = fixture.service.external_agent_rows().len();
    let mut sockets = Vec::new();
    let (unknown_socket, unknown) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &event("PreModelRequest", Some("unknown"), "shared", None),
            &unknown,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &unknown).await["result"]["observed"],
        true
    );
    sockets.push(unknown_socket);
    for id in ["a", "b"] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &event("SubagentStart", Some(id), "shared", None),
                &source,
            )
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &source).await["result"]["changed"],
            true
        );
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &event("SubagentStart", Some(id), "shared", None),
                &source,
            )
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &source).await["result"]["changed"],
            false
        );
    }
    for id in [None, Some("a"), Some("b")] {
        let (mut socket, connection) = child(&mut fixture).await;
        if id == Some("a") {
            super::codex_request_scope::change_native_directory(&mut socket, &project).await;
        }
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &event("PreModelRequest", id, "shared", None),
                &connection,
            )
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &connection).await["result"]["observed"],
            true
        );
        sockets.push(socket);
    }
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &event("SubagentStop", Some("a"), "shared", None),
            &source,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["changed"],
        true
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &event("SubagentStart", Some("a"), "shared", None),
            &source,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["changed"],
        false
    );
    for (id, tokens) in [(None, 10), (Some("a"), 20), (Some("b"), 30)] {
        let (socket, connection) = child(&mut fixture).await;
        for replay in [false, true] {
            let work = fixture
                .service
                .prepare_external_helper_presentation(
                    &event("PostModelRequest", id, "shared", Some(tokens)),
                    &connection,
                )
                .unwrap();
            let reply = settle(&mut fixture, work, &connection).await;
            assert_eq!(reply["result"]["durable"], true, "{reply}");
            assert_eq!(reply["result"]["applied"], !replay, "{reply}");
        }
        sockets.push(socket);
    }
    for (kind, tokens) in [("PreModelRequest", None), ("PostModelRequest", Some(4))] {
        let (socket, connection) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &event(kind, Some("a"), "shared", tokens),
                &connection,
            )
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_none()
        );
        sockets.push(socket);
    }
    assert_eq!(fixture.service.external_agent_rows().len(), original_rows);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(
        (
            binding.generation,
            binding.accounting_owner.clone(),
            binding.expires
        ),
        original
    );
    assert!(!binding.retired);
    assert_eq!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .sequence,
        1
    );
    // A later child turn is admitted independently; returning to its retired raw
    // turn cannot manufacture another request or borrow the parent's turn.
    for (kind, tokens) in [("PreModelRequest", None), ("PostModelRequest", Some(5))] {
        let (socket, connection) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &event(kind, Some("b"), "next", tokens),
                &connection,
            )
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_none()
        );
        sockets.push(socket);
    }
    let (socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &event("PreModelRequest", Some("b"), "shared", None),
            &connection,
        )
        .unwrap();
    assert!(
        settle(&mut fixture, work, &connection)
            .await
            .get("error")
            .is_none()
    );
    sockets.push(socket);
    // Child activity has no parent freshness authority, including after expiry.
    {
        let binding = fixture
            .service
            .control
            .external_agents_mut()
            .bindings
            .values_mut()
            .next()
            .unwrap();
        binding.expires = 0;
        binding
            .enrollment
            .as_mut()
            .unwrap()
            .curated_observer
            .as_mut()
            .unwrap()
            .observed_at = Instant::now().checked_sub(Duration::from_secs(31));
    }
    fixture.service.reconcile_external_agent_registrations();
    assert!(fixture.service.external_agent_rows().is_empty());
    for (kind, tokens) in [
        ("SubagentStart", None),
        ("PreModelRequest", None),
        ("PostModelRequest", Some(2)),
        ("SubagentStop", None),
    ] {
        let (socket, connection) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &event(kind, Some("c"), "dormant", tokens),
                &connection,
            )
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_none()
        );
        sockets.push(socket);
    }
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(binding.expires, 0);
    assert!(!binding.retired);
    assert_eq!(binding.accounting_owner, original.1);
    assert!(fixture.service.external_agent_rows().is_empty());
    assert!(fixture.service.live_pane_harness_status("%1").is_none());
    let database = rusqlite::Connection::open(path).unwrap();
    let counts: (i64, i64) = database
        .query_row(
            "SELECT count(*),sum(input_tokens) FROM token_usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (6, 71));
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events WHERE project_id=?1",
                [project_id.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        20
    );
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events WHERE project_id IS NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        51
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        6
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(DISTINCT owner) FROM external_usage_requests",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
}

/// Main and child readiness consume one existing sixteen-slot budget. Advancing
/// child turns cannot evict earlier admitted requests; a seventeenth readiness
/// writes no origin until an original old child Post frees one slot. Retrying
/// that same native pre then succeeds without inventing another namespace.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_children_share_parent_custody_without_eviction() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("child capacity requires native evidence")
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_source_socket, source) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &source)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["registered"],
        true
    );
    for request in [
        callback("UserPromptSubmit", "main"),
        event("SubagentStart", Some("a"), "turn-1", None),
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
    let mut sockets = Vec::new();
    for index in 0..16 {
        let (socket, connection) = child(&mut fixture).await;
        let request = if index == 0 {
            event("PreModelRequest", None, "main", None)
        } else {
            event("PreModelRequest", Some("a"), &format!("turn-{index}"), None)
        };
        let work = fixture
            .service
            .prepare_external_helper_presentation(&request, &connection)
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &connection).await["result"]["observed"],
            true
        );
        sockets.push(socket);
    }
    let (retry_socket, retry) = child(&mut fixture).await;
    let request = event("PreModelRequest", Some("a"), "turn-16", None);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&request, &retry)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &retry).await["result"]["observed"],
        true
    );
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        16
    );
    let (post_socket, post) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &event("PostModelRequest", Some("a"), "turn-1", Some(7)),
            &post,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &post).await["result"]["durable"],
        true
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&request, &retry)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &retry).await["result"]["observed"],
        true
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        17
    );
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        7
    );
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
    drop((retry_socket, post_socket));
}
