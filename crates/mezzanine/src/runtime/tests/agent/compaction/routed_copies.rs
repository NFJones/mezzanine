//! Routed ephemeral workers inherit a frozen canonical head through actual
//! routing admission and provider dispatch, without borrowing a later archive.

use super::*;

/// The first routed provider call must see the selected typed head and exact
/// retained records. A real parent append after routing cannot enter that call.
#[test]
fn routed_worker_first_request_preserves_typed_head_and_frozen_raw_source() {
    let (mut service, store, turn, conversation) = replay_copies::published_source();
    replay_copies::native_scenario(&mut service, |service| {
        crate::test_support::compaction_source::complete_task(
            service,
            &turn,
            "PARENT_COMPLETED_BEFORE_ROUTING",
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
        );
        service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
        service
            .execute_agent_shell_loop_command("%1", "/loop --limit 1 inspect frozen epoch")
            .unwrap();
        let parent = service
            .agent_loop_turns_for_tests()
            .iter()
            .find(|(_, state)| state.pane_id == "%1")
            .map(|(turn, _)| turn.clone())
            .unwrap();
        let expected = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&conversation).unwrap(),
            service.agent_turn_contexts()[&parent].clone(),
        )
        .unwrap();
        assert_eq!(expected.revision().get(), 2);
        let profile = service.agent_turn_model_profile(&parent).unwrap().clone();
        service
            .apply_routing_selected_transition(
                &AgentId::opaque("agent-%1").unwrap(),
                &parent,
                mez_agent::AutoSizingRoutingSelection {
                    selected_profile: profile.clone(),
                    selected_profile_name: "default".into(),
                    routing_token_usage_by_model: Default::default(),
                    decision_summary: None,
                    fallback: None,
                },
            )
            .unwrap();
        let child_turn = service
            .routed_workflow_for_tests(&parent)
            .unwrap()
            .child_turn_id
            .clone()
            .unwrap();
        let child = service
            .agent_turn_ledger()
            .turn(&child_turn)
            .unwrap()
            .clone();
        let target = service.agent_shell_store().get(&child.pane_id).unwrap();
        assert!(target.ephemeral);
        assert_ne!(target.session_id, conversation);
        let captured = service.agent_turn_contexts()[&child_turn].clone();
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&target.session_id).unwrap(),
            captured.clone(),
        )
        .unwrap();
        assert_eq!(owner.revision(), expected.revision());
        assert_eq!(
            owner.head().unwrap().occurrence_id(),
            expected.head().unwrap().occurrence_id()
        );
        assert_eq!(
            owner.head().unwrap().conversation().as_str(),
            target.session_id
        );
        // A sealed source retains even the controller's original task. The
        // child's new routed instruction is appended as a descriptive reference.
        for original in expected.context().chronology() {
            assert!(captured.chronology().iter().any(|event| {
                event.record() == original.record() && event.block() == original.block()
            }));
        }
        assert!(captured.stable_slots().is_empty());
        assert!(!service.permission_policy().approval_bypass());

        // This is a real ordinary archive write after capture, not a context
        // mutation or a fabricated append acknowledgment for pending source.
        let rows = store.inspect(&conversation).unwrap();
        store
            .append(&TranscriptEntry {
                conversation_id: conversation.clone(),
                sequence: rows.last().unwrap().sequence + 1,
                created_at_unix_seconds: current_unix_seconds(),
                role: TranscriptRole::User,
                turn_id: "later-independent-parent-source".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "LATER_PARENT_APPEND_MUST_NOT_LEAK".into(),
            })
            .unwrap();
        assert_eq!(service.agent_turn_contexts()[&child_turn], captured);

        struct FirstRequest {
            seen: RefCell<Vec<String>>,
            response: mez_agent::ModelResponse,
        }
        impl ModelProvider for FirstRequest {
            fn provider_id(&self) -> &str {
                "runtime-batch"
            }

            fn send_request(
                &self,
                request: &mez_agent::ModelRequest,
            ) -> Result<mez_agent::ModelResponse> {
                self.seen.borrow_mut().push(
                    request
                        .messages
                        .iter()
                        .map(|message| message.content.as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                Ok(self.response.clone())
            }
        }
        let provider = FirstRequest {
            seen: RefCell::new(Vec::new()),
            response: runtime_say_response_for_agent(
                &child_turn,
                &child.agent_id,
                "routed source inspected",
                true,
            ),
        };
        service
            .execute_agent_turn_with_provider(&child_turn, &provider, profile)
            .unwrap();
        let requests = provider.seen.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]
                .matches("original model-authored fork head")
                .count(),
            1
        );
        assert!(requests[0].contains("ORIGINAL_PARENT_RAW_TAIL"));
        assert!(requests[0].contains("PARENT_COMPLETED_BEFORE_ROUTING"));
        assert!(!requests[0].contains("COVERED_PARENT_SOURCE"));
        assert!(!requests[0].contains("LATER_PARENT_APPEND_MUST_NOT_LEAK"));
        assert!(
            !store
                .root()
                .join(&child.conversation_id)
                .join("compaction-epoch.json")
                .exists()
        );
    });
}
