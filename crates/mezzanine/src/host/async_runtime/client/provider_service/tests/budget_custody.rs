//! A real configured loopback HTTP request retains native budget custody after
//! its original runtime is gone. No paid provider or user store participates.

use super::*;
use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::runtime::RuntimeSessionService;
use crate::storage::token_usage::TokenUsageStore;
use tokio::io::AsyncReadExt;

/// The shared fixture creates this unique directory itself. Keep the exact path
/// until every owned service/request/socket has retired, then verify deletion.
/// Drop cannot await request retirement: on unwind it diagnoses and retains,
/// never unlinks a live custody inode or risks a second panic during cleanup.
struct FixtureRoot(Option<std::path::PathBuf>);
impl FixtureRoot {
    fn cleanup(&mut self) {
        let path = self.0.as_ref().unwrap();
        std::fs::remove_dir_all(path).unwrap();
        assert!(!path.exists());
        self.0 = None;
    }
}
impl Drop for FixtureRoot {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            eprintln!(
                "budget HTTP fixture retained pending owner retirement: {}",
                path.display()
            );
        }
    }
}

#[tokio::test]
async fn budget_native_custody_actual_http_worker_outlives_actor_until_join() {
    qualify_http_custody(false).await;
}

/// Actual unwind of the root guard while HTTP custody is live must retain its
/// inode. The outer owner then retires/joins the original work and cleans it.
#[tokio::test]
async fn budget_native_custody_http_fixture_unwind_preserves_live_lock() {
    qualify_http_custody(true).await;
}

async fn qualify_http_custody(unwind: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (mut service, agent, turn_id) =
        super::incomplete::prepared_fixture("hold original HTTP custody").await;
    let transcripts = service.agent_transcript_store().unwrap().clone();
    let path = transcripts.root().parent().unwrap().to_owned();
    let mut root = FixtureRoot(Some(path.clone()));
    let store = TokenUsageStore::new(path.join("usage.sqlite"));
    service.replace_config_layers(vec![ConfigLayer {
        name: "budget-loopback".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\nrouting=false\nshell_mode=\"native\"\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\n[permissions]\nsandbox=\"policy-only\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://{address}/v1\"\nmodels=[\"test-model\"]\ndefault_model=\"test-model\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"test-model\"\n"),
    }]).unwrap();
    service.enable_runtime_budget_membership_for_tests();
    let preparation = service.prepare_agent_provider_work(&turn_id).unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    let mut dispatch = service
        .claim_configured_agent_provider_task(&agent, &turn_id)
        .unwrap()
        .unwrap();
    service
        .record_claimed_agent_provider_task(&dispatch, 7, 30_000)
        .unwrap();
    dispatch.claim_generation = 7;
    assert!(dispatch.budget_binding_lease.is_some());
    let member = store
        .budget_membership(&dispatch.turn.conversation_id)
        .unwrap()
        .unwrap()
        .0;
    let (sender, _observations) = tokio::sync::mpsc::channel(8);
    let request = tokio::spawn(execute_runtime_agent_provider_dispatch(
        dispatch,
        None,
        sender,
        Default::default(),
    ));
    let request_guard = AbortProviderWorkerOnDrop(request.abort_handle());
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !bytes.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let mut chunk = [0; 1024];
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0, "actual request must reach loopback server");
            bytes.extend_from_slice(&chunk[..read]);
            assert!(bytes.len() <= 1024 * 1024, "bounded request evidence");
        }
    })
    .await
    .unwrap();
    if unwind {
        let panic = std::panic::catch_unwind(move || {
            let _root = root;
            panic!("injected budget fixture failure after HTTP admission");
        });
        assert!(panic.is_err());
        assert!(
            path.join("transcripts")
                .join(&member.conversation)
                .join("budget-binding.lock")
                .is_file()
        );
        root = FixtureRoot(Some(path));
    }
    // Retire all exact fixture-owned native roots before discarding the actor.
    // Only the actual provider future now retains the original binding lock.
    service.terminate_all_pane_processes().unwrap();
    drop(service);
    assert!(!request.is_finished());
    let refused = store
        .publish_native_budget_membership(&transcripts, &member, "successor", None)
        .is_err();
    request.abort();
    let mut request = request;
    let joined = tokio::time::timeout(Duration::from_secs(10), &mut request)
        .await
        .unwrap();
    drop(request_guard);
    assert!(joined.unwrap_err().is_cancelled());
    assert!(
        refused,
        "actor destruction alone cannot retire HTTP custody"
    );
    let successor = store
        .publish_native_budget_membership(&transcripts, &member, "successor", None)
        .unwrap();
    assert_eq!(
        store
            .budget_membership(&member.conversation)
            .unwrap()
            .unwrap()
            .0,
        member
    );
    drop((successor, socket, listener));
    root.cleanup();
}
