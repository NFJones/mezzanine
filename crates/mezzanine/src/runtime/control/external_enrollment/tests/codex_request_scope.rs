//! Original Codex request scope under actual cwd changes and observer replacement.
//!
//! The native creator and shared ledger provide evidence. No injected origin,
//! successful worker result, current pane hint or successor generation authorizes
//! a callback. Already incurred expense can settle without a stale helper ACK.

use super::codex_observation::{callback, declaration};
use super::helper_presentation::{child, settle};
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Constructs only normalized counters on a public original-session selector.
fn post() -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"post","method":"agent/external/codex-observe",
        "params":{"pane_id":"%1","external_session_id":"session-a","event":"PostModelRequest","turn_id":"turn-a",
        "counters":{"input_tokens":10,"output_tokens":3}}}).to_string()).unwrap()
}

/// Changes the actual isolated fixture creator's cwd and waits for its own
/// acknowledgment. No production RPC accepts this path as accounting authority.
async fn change_creator_directory(fixture: &mut Fixture, directory: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt;
    let bytes = directory.as_os_str().as_bytes();
    fixture.socket.write_all(&[8]).await.unwrap();
    fixture
        .socket
        .write_all(&u32::try_from(bytes.len()).unwrap().to_be_bytes())
        .await
        .unwrap();
    fixture.socket.write_all(bytes).await.unwrap();
    let mut changed = [0];
    tokio::time::timeout(
        Duration::from_secs(10),
        fixture.socket.read_exact(&mut changed),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(changed, [1]);
}

/// The pre boundary captures actual creator cwd A despite a stale pane hint B.
/// The creator then changes physically to B before final counters arrive; those
/// counters and a replay after a view reset retain A. Only a later genuine pre
/// may admit B, even though the pane now misleadingly advertises A. Project IDs
/// are read from committed expense and readiness rather than inferred from UI.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_model_origin_survives_native_cwd_change_and_view_reset() {
    use crate::security::project::{ProjectTrustStore, TrustDecision};
    let directory = std::env::temp_dir().join(format!(
        "mez-codex-scope-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let a = directory.join("original");
    let b = directory.join("later");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let Some(mut fixture) = fixture_with_start_directory("hold", Some(&a)).await else {
        panic!("Codex origin qualification requires native peer evidence");
    };
    let store =
        crate::storage::token_usage::TokenUsageStore::new(fixture.directory.join("usage.sqlite"));
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(a.clone(), TrustDecision::Trusted, None)
        .unwrap();
    trust
        .decide(b.clone(), TrustDecision::Trusted, None)
        .unwrap();
    let inventory = store
        .prepare_accounting_projects(&trust.records().cloned().collect::<Vec<_>>())
        .unwrap();
    let project_a = inventory
        .iter()
        .find(|row| row.root == a.canonicalize().unwrap())
        .unwrap()
        .id
        .clone()
        .unwrap();
    let project_b = inventory
        .iter()
        .find(|row| row.root == b.canonicalize().unwrap())
        .unwrap()
        .id
        .clone()
        .unwrap();
    fixture.service.set_project_trust_store(trust, None);
    fixture.service.set_token_usage_store(store.clone());
    fixture
        .service
        .persistence
        .install_accounting_projects(&store, Some(inventory));
    fixture
        .service
        .set_pane_current_working_directory("%1", b.clone());
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
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "turn-a"), &source)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &source)
            .await
            .get("error")
            .is_none()
    );
    let mut sockets = Vec::new();
    let database = rusqlite::Connection::open(store.path()).unwrap();
    for (index, expected) in [project_a.as_str(), project_b.as_str()]
        .into_iter()
        .enumerate()
    {
        let (pre_socket, pre) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback("PreModelRequest", "turn-a"), &pre)
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &pre).await["result"]["observed"],
            true
        );
        if index == 0 {
            change_creator_directory(&mut fixture, &b).await;
            fixture
                .service
                .set_pane_current_working_directory("%1", a.clone());
        }
        let (post_socket, connection) = child(&mut fixture).await;
        for replay in [false, true] {
            if replay {
                assert!(fixture.service.reset_external_token_usage("%1"));
            }
            let work = fixture
                .service
                .prepare_external_helper_presentation(&post(), &connection)
                .unwrap();
            let reply = settle(&mut fixture, work, &connection).await;
            assert_eq!(reply["result"]["durable"], true, "{reply}");
            assert_eq!(reply["result"]["applied"], !replay, "{reply}");
        }
        let tokens: i64 = database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events WHERE project_id=?1",
                [expected],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tokens, 10);
        sockets.extend([pre_socket, post_socket]);
    }
    let counts: (i64, i64) = database
        .query_row(
            "SELECT count(*),sum(input_tokens) FROM token_usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (2, 20));
    let origins: Vec<String> = database
        .prepare("SELECT project_id FROM external_usage_requests ORDER BY project_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    let mut expected = vec![project_a.as_str().to_owned(), project_b.as_str().to_owned()];
    expected.sort();
    assert_eq!(origins, expected);
    drop(fixture);
    std::fs::remove_dir_all(directory).unwrap();
}

/// Native work can commit incurred expense before observer replacement reaches
/// actor settlement. The old callback must receive no durable ACK or successor
/// custody after that fence, while the original ledger still records one charge.
/// Stateless callbacks cannot automatically discover or adopt the new epoch.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_model_published_expense_survives_rotation_without_stale_ack() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Codex rotation qualification requires native peer evidence")
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
    let first = settle(&mut fixture, work, &source).await;
    assert_eq!(first["result"]["registered"], true);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "turn-a"), &source)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &source)
            .await
            .get("error")
            .is_none()
    );
    let (_pre_socket, pre) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("PreModelRequest", "turn-a"), &pre)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &pre).await["result"]["observed"],
        true
    );
    let (_post_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&post(), &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok(), "{observed:?}");
    let mut rotation = declaration();
    let mut params: serde_json::Value =
        serde_json::from_str(rotation.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = "rotated-codex-observer".into();
    params["predecessor_generation"] = first["result"]["generation"].clone();
    rotation.params = Some(params.to_string());
    let replacement = fixture
        .service
        .prepare_external_enrollment(&rotation, &source)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, replacement, &source).await["result"]["registered"],
        true
    );
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some(), "{reply}");
    assert_ne!(reply["result"]["durable"], true);
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&post(), &connection)
            .is_err()
    );
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(binding.enrollment.as_ref().unwrap().epoch, 2);
    assert_eq!(
        binding
            .enrollment
            .as_ref()
            .unwrap()
            .codex_request_gap_coverage(),
        None
    );
    assert!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .is_none()
    );
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT count(*),sum(input_tokens) FROM token_usage_events",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (1, 10)
    );
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
}
