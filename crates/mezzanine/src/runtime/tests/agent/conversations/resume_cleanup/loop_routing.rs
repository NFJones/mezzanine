//! Automatic model selection during a delegated pane's first ordinary loop.
//!
//! Genuine and restored lineage both select models in place. These native
//! fixtures exercise that routing boundary together with loop restoration so
//! an existing cleanup proof survives without granting historical ancestry a
//! new proof or manufacturing an unsupported nested routed-worker workflow.

use super::*;

/// A real delegated pane keeps automatic model selection in place during its
/// first fork/new iteration. Restoring its durable conversation must preserve
/// both its incoming cleanup obligation and its existing native child's edge.
/// The otherwise identical independently resumed pane retains only historical
/// ancestry: model selection and restoration must not let its former parent
/// reclaim either native process. No provider request is executed.
#[test]
fn runtime_cleanup_first_loop_routing_selection_preserves_exact_native_tree() {
    for mode in ["--fork", "--new"] {
        for independent in [false, true] {
            let mut fixture = CleanupFixture::new();
            let child = fixture.spawn("agent-%1");
            let grandchild = fixture.spawn(&child);
            if independent {
                fixture.save("independent-routing-loop", Some("agent-%1"));
                fixture.resume(&child, "independent-routing-loop");
            }
            let pane = child.strip_prefix("agent-").unwrap();
            let durable = fixture
                .service
                .agent_shell_store()
                .get(pane)
                .unwrap()
                .session_id
                .clone();
            let child_native = fixture.native_identity(&child);
            let grandchild_native = fixture.native_identity(&grandchild);
            fixture
                .service
                .execute_agent_shell_loop_command(
                    pane,
                    &format!("/loop {mode} --limit 1 select model within owned loop"),
                )
                .unwrap();
            let turn_id = fixture
                .service
                .agent_shell_store()
                .get(pane)
                .unwrap()
                .running_turn_id
                .clone()
                .unwrap();
            let turn = fixture
                .service
                .agent_turn_ledger()
                .turn(&turn_id)
                .unwrap()
                .clone();
            assert_eq!(
                fixture.service.auto_sizing_routing_policy_for_turn(&turn),
                mez_agent::AutoSizingRoutingPolicy::InPlace,
                "delegated and restored lineage must select in place"
            );
            let profile = fixture
                .service
                .agent_turn_model_profile(&turn_id)
                .unwrap()
                .clone();
            fixture
                .service
                .apply_routing_selected_transition(
                    &AgentId::opaque(child.clone()).unwrap(),
                    &turn_id,
                    mez_agent::AutoSizingRoutingSelection {
                        selected_profile: profile,
                        selected_profile_name: "default".to_string(),
                        routing_token_usage_by_model: Default::default(),
                        decision_summary: None,
                        fallback: None,
                    },
                )
                .unwrap();
            assert!(
                fixture
                    .service
                    .routed_workflow_for_tests(&turn_id)
                    .is_none()
            );
            let state = fixture.service.agent_loop_state(pane).unwrap();
            assert_eq!(state.execution_pane_id, pane);
            assert!(state.routed_parent_turn_id.is_none());
            assert_eq!(
                fixture.service.agent_turn_ledger().turn(&turn_id),
                Some(&turn),
                "selection must preserve the exact existing work turn"
            );
            fixture.assert_native_identity(&child, child_native);
            fixture.assert_native_identity(&grandchild, grandchild_native);

            fixture.service.stop_agent_turn_for_pane(pane).unwrap();
            assert_eq!(
                fixture
                    .service
                    .agent_shell_store()
                    .get(pane)
                    .unwrap()
                    .session_id,
                durable
            );
            fixture.close("agent-%1");
            if independent {
                fixture.assert_native_identity(&child, child_native);
                fixture.assert_native_identity(&grandchild, grandchild_native);
                fixture.close(&child);
            } else {
                assert!(!fixture.present(&child));
                assert_ne!(
                    mez_mux::process::process_start_token_for_pid(child_native.0),
                    Some(child_native.1)
                );
            }
            assert!(!fixture.present(&grandchild));
            assert_ne!(
                mez_mux::process::process_start_token_for_pid(grandchild_native.0),
                Some(grandchild_native.1)
            );
        }
    }
}
