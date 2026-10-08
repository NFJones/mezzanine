//! Vendor-neutral persistent-producer admission with real Unix process evidence.
//!
//! Harness labels are inert metadata, not executable/vendor attestation or proof
//! of installed adapter support. Every case uses the same actual native ingress
//! and pane ancestry, retaining all role/usage/topology restrictions. Hook-only
//! helpers and shared servers cannot gain eligibility by changing their label.

use super::*;

/// Changes only the canonical harness metadata; native evidence still comes
/// from the genuine ordinary process and qualified Unix writer in the fixture.
fn harness_request(harness: &str, observer_kind: &str) -> JsonRpcRequest {
    let mut request = request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["harness"] = serde_json::json!(harness);
    params["observer_kind"] = serde_json::json!(observer_kind);
    request.params = Some(params.to_string());
    request
}

/// Each supported canonical label can use the identical kernel-qualified
/// persistent-producer transport without a primary or vendor launcher. Same-run
/// retry preserves the private handle/accounting owner, generic status works,
/// source-continuity usage stays unavailable and no control client is created.
/// This is shared protocol eligibility, not six installed vendor integrations.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_supported_harnesses_share_persistent_provenance_contract() {
    for harness in ["claude", "codex", "copilot", "opencode", "cursor", "pi"] {
        let Some(mut fixture) = fixture("hold").await else {
            return;
        };
        let clients = fixture.service.session().clients().len();
        let request = harness_request(harness, "persistent");
        let mut initial = None;
        let mut accounting_owner = None;
        for _ in 0..2 {
            let work = fixture
                .service
                .prepare_external_enrollment(&request, &fixture.connection)
                .unwrap_or_else(|error| {
                    panic!(
                        "persistent {harness} admission rejected: {}",
                        error.message()
                    )
                });
            let native = work.clone();
            let observed = tokio::task::spawn_blocking(move || native.observe())
                .await
                .unwrap();
            let result: serde_json::Value = serde_json::from_str(
                &fixture
                    .service
                    .complete_external_enrollment(work, observed, &fixture.connection),
            )
            .unwrap();
            assert!(
                result.get("error").is_none(),
                "native {harness} producer rejected"
            );
            assert_eq!(result["result"]["controls"], serde_json::json!([]));
            assert_eq!(result["result"]["usage"], "unavailable-source-continuity");
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
            if let Some(previous) = &accounting_owner {
                assert!(
                    previous == &owner,
                    "same-run retry changed accounting ownership"
                );
            } else {
                accounting_owner = Some(owner);
            }
            if let Some(previous) = &initial {
                assert!(previous == &result, "same-run retry changed response");
            } else {
                initial = Some(result);
            }
        }
        let initial = initial.unwrap();
        let callback = |method, extra: serde_json::Value| {
            let mut params = serde_json::json!({"launch_token":initial["result"]["launch_token"],
                "generation":initial["result"]["generation"],"external_session_id":"session-a"});
            params
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            crate::control::parse_json_rpc_request(
                &serde_json::json!({"jsonrpc":"2.0","id":"event","method":method,"params":params})
                    .to_string(),
            )
            .unwrap()
        };
        let presentation = callback(
            "agent/external/presentation",
            serde_json::json!({"sequence":1,"state":"running"}),
        );
        fixture
            .service
            .dispatch_external_agent_request(&presentation, &fixture.connection)
            .unwrap();
        let rows = fixture.service.external_agent_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["harness"], harness);
        assert_eq!(rows[0]["status"], "running");
        assert_eq!(rows[0]["usage_coverage"], "unavailable-source-continuity");
        let usage = callback("agent/external/usage", serde_json::json!({}));
        assert!(
            fixture
                .service
                .prepare_external_usage(&usage, &fixture.connection)
                .is_err()
        );
        assert_eq!(fixture.service.session().clients().len(), clients);
        assert!(!fixture.connection.initialized());
        assert!(fixture.connection.caller_client_id().is_none());
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .enrollments
                .pending
                .is_empty()
        );
        assert_eq!(
            fixture
                .service
                .control
                .external_agents()
                .enrollments
                .ancestry_budget
                .reserved(),
            1
        );
    }
}

/// Changing the vendor label cannot turn a short-lived helper or shared server
/// into a persistent producer. Unknown/retired/case-alias identities also reject
/// before reservation, so this contract cannot silently resurrect Gemini or add
/// arbitrary future adapters by accepting every non-retired metadata string.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_harness_labels_do_not_enable_unknown_or_helper_topologies() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    for harness in ["claude", "codex", "copilot", "opencode", "cursor", "pi"] {
        for kind in ["helper", "server", "short-lived"] {
            assert!(
                fixture
                    .service
                    .prepare_external_enrollment(
                        &harness_request(harness, kind),
                        &fixture.connection
                    )
                    .is_err()
            );
        }
    }
    for harness in ["gemini", "unknown", "Claude", "codex-hook", ""] {
        assert!(
            fixture
                .service
                .prepare_external_enrollment(
                    &harness_request(harness, "persistent"),
                    &fixture.connection
                )
                .is_err()
        );
    }
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .ancestry_budget
            .reserved(),
        0
    );
}
