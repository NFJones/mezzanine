//! Context contract regressions grouped by semantic behavior.
//! Child modules retain existing test function names and canonical assertions.

use super::*;
use crate::{ActionContentBlock, ActionResult, ActionStatus, AgentPromptError};

mod history;
mod projection;
mod storage;

mod chronology {
    use super::*;

    /// Imported suffixes preserve trust and execution metadata while receiving
    /// fresh local identities. Stable instructions are not history arrivals;
    /// rejecting such an import leaves the destination unchanged.
    #[test]
    fn imported_history_append_preserves_ownership_and_rejects_stable_slots() {
        let mut candidate = AgentContext::empty();
        candidate
            .append_user_event("active", "exact active prompt")
            .unwrap();
        let mut history = AgentContext::empty();
        history
            .append_peer_message_event("peer", "untrusted source")
            .unwrap();
        let group = ContextExecutionGroupId::new("imported-group").unwrap();
        history
            .append_assistant_event("decision", "answer", group.clone())
            .unwrap();
        history
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "result",
                "evidence",
                group,
                None,
                true,
            )
            .unwrap();
        assert_eq!(candidate.append_imported_history(&history).unwrap(), 3);
        for (actual, original) in candidate.chronology()[1..].iter().zip(history.chronology()) {
            assert_eq!(actual.block(), original.block());
            assert_eq!(actual.retention(), original.retention());
            assert_eq!(actual.semantic_kind(), original.semantic_kind());
            assert_eq!(actual.execution_group_id(), original.execution_group_id());
            assert_eq!(actual.provider_owner(), original.provider_owner());
            assert!(actual.sequence() > original.sequence());
        }
        let before = candidate.clone();
        let stable = AgentContext::new_durable(vec![ContextBlock {
            source: ContextSourceKind::Policy,
            placement: ContextPlacement::StablePrefix,
            label: "policy".into(),
            content: "not history".into(),
        }])
        .unwrap();
        assert!(candidate.append_imported_history(&stable).is_err());
        assert_eq!(candidate, before);
    }

    /// Rebasing preserves occurrence identities and metadata without promoting
    /// peer authority. A frozen-source rewrite must fail atomically.
    #[test]
    fn chronology_suffix_rebase_preserves_identity_and_rejects_rewrite() {
        let mut live = AgentContext::empty();
        live.append_user_event("user prompt", "original").unwrap();
        let frozen = live.chronology().to_vec();
        let mut candidate = live.clone();
        live.append_peer_message_event("peer 1", "repeated")
            .unwrap();
        live.append_user_event("steering", "exact").unwrap();
        live.append_peer_message_event("peer 2", "repeated")
            .unwrap();
        assert_eq!(
            candidate.rebase_chronology_suffix(&frozen, &live).unwrap(),
            3
        );
        assert_eq!(candidate.chronology(), live.chronology());
        let committed = candidate.clone();
        assert!(candidate.rebase_chronology_suffix(&frozen, &live).is_err());
        assert_eq!(candidate, committed);
        let mut changed = AgentContext::empty();
        changed
            .append_user_event("user prompt", "rewritten")
            .unwrap();
        let mut candidate = AgentContext::empty();
        let before = candidate.clone();
        assert!(
            candidate
                .rebase_chronology_suffix(&frozen, &changed)
                .is_err()
        );
        assert_eq!(candidate, before);
    }
}

mod semantics {
    use super::*;

    /// Peer mail is untrusted data from another agent, so it must not share the
    /// user-input trust domain, while user prompts and routed local messages keep theirs.
    #[test]
    fn peer_message_source_maps_to_a_non_user_trust_domain() {
        assert_eq!(
            TrustDomain::for_source(ContextSourceKind::PeerMessage),
            TrustDomain::WebContent
        );
        assert!(
            TrustDomain::for_source(ContextSourceKind::PeerMessage).is_untrusted_by_default(),
            "provider framing must mark peer text untrusted"
        );
        for source in [
            ContextSourceKind::UserInstruction,
            ContextSourceKind::LocalMessage,
        ] {
            assert_eq!(TrustDomain::for_source(source), TrustDomain::UserInput);
        }
    }
}

mod validation {
    use super::*;

    /// Verifies semantic validation accepts one complete canonical request
    /// chronology with an exact task prelude, direct user prompt, execution
    /// group, and later factual reference event.
    #[test]
    fn context_semantics_accept_canonical_chronology() {
        let blocks = vec![
            ContextBlock::stable_instruction(ContextSourceKind::Policy, "policy", "stable"),
            ContextBlock::task_prelude(
                ContextSourceKind::SkillInstruction,
                "skill",
                "task workflow",
            ),
            ContextBlock::user_event("user prompt", "do the work"),
            ContextBlock::assistant_event("assistant action", "run command"),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "action result action-1",
                "succeeded",
            ),
            ContextBlock::reference_event(
                ContextSourceKind::LocalMessage,
                "local message",
                "avoid overlap",
            ),
            ContextBlock::reference_event(
                ContextSourceKind::RuntimeHint,
                "runtime state",
                "cwd=/repo",
            ),
        ];
        validate_context_semantics(&blocks).unwrap();
    }

    /// Verifies multi-action evidence and mid-turn steering retain their exact
    /// observation order in durable chronology.
    #[test]
    fn context_semantics_preserve_multi_action_and_mid_turn_steering_order() {
        let blocks = vec![
            ContextBlock::user_event("user prompt", "implement the change"),
            ContextBlock::assistant_event("assistant action 1", "inspect owner"),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "result 1",
                "owner found",
            ),
            ContextBlock::assistant_event("assistant action 2", "edit owner"),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "result 2",
                "edit applied",
            ),
            ContextBlock::user_event("user steering", "also update the specification"),
            ContextBlock::assistant_event("assistant action 3", "update specification"),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "result 3",
                "specification updated",
            ),
        ];
        let context = AgentContext::new_durable(blocks).unwrap();
        assert_eq!(
            context
                .blocks
                .iter()
                .map(|block| block.label.as_str())
                .collect::<Vec<_>>(),
            [
                "user prompt",
                "assistant action 1",
                "result 1",
                "assistant action 2",
                "result 2",
                "user steering",
                "assistant action 3",
                "result 3"
            ]
        );
        assert_eq!(context.blocks[5].content, "also update the specification");
        assert_eq!(context.blocks[5].retention(), ContextRetention::Exact);
    }

    /// Verifies semantic validation rejects non-instructions in the stable
    /// prefix and task preludes inserted after the active prompt, with enough
    /// diagnostics to identify the producer.
    #[test]
    fn context_semantics_reject_ambiguous_lifetime_and_authorship() {
        let invalid_cases = [
            vec![ContextBlock {
                source: ContextSourceKind::Memory,
                placement: crate::ContextPlacement::StablePrefix,
                label: "memory".to_string(),
                content: "historical note".to_string(),
            }],
            vec![
                ContextBlock::user_event("user prompt", "do the work"),
                ContextBlock::task_prelude(
                    ContextSourceKind::SkillInstruction,
                    "late skill",
                    "workflow",
                ),
            ],
        ];
        for blocks in invalid_cases {
            let error = validate_context_semantics(&blocks).unwrap_err();
            assert!(error.message().contains("context semantic violation"));
            assert!(error.message().contains("semantic="));
            assert!(error.message().contains("retention="));
        }
    }
}

mod errors {
    use super::*;

    /// Required context validation accepts substantive values and rejects
    /// whitespace-only values with a stable field-specific diagnostic.
    #[test]
    fn context_required_validation_rejects_whitespace() {
        assert!(validate_context_required("model", "gpt-5").is_ok());
        let error = validate_context_required("model", " \t ").unwrap_err();
        assert_eq!(error.to_string(), "model must not be empty");
    }

    /// Request assembly preserves invalid-argument classification when either
    /// context validation or prompt-profile validation rejects an input.
    #[test]
    fn request_assembly_preserves_invalid_argument_errors() {
        let context_error = AgentRequestAssemblyError::from(AgentContextError::new("bad model"));
        let prompt_error =
            AgentRequestAssemblyError::from(AgentPromptError::invalid_args("bad profile"));
        assert_eq!(
            context_error.kind(),
            AgentRequestAssemblyErrorKind::InvalidArgs
        );
        assert_eq!(
            prompt_error.kind(),
            AgentRequestAssemblyErrorKind::InvalidArgs
        );
    }

    /// Request assembly retains invalid-state classification for failures in
    /// product-supplied prompt assets so the composition layer can adapt it.
    #[test]
    fn request_assembly_preserves_prompt_asset_errors() {
        let error =
            AgentRequestAssemblyError::from(AgentPromptError::invalid_state("asset missing"));
        assert_eq!(error.kind(), AgentRequestAssemblyErrorKind::InvalidState);
        assert_eq!(error.message(), "asset missing");
    }
}

mod settlement {
    use super::*;

    /// Builds one valid successful or running action-result fixture.
    fn action_result(action_id: &str, status: ActionStatus, text: &str) -> ActionResult {
        ActionResult {
            protocol: "maap/1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            action_id: action_id.to_string(),
            action_type: "shell_command",
            status,
            content: vec![ActionContentBlock::text(text)],
            structured_content_json: None,
            permission_evaluation: None,
            is_error: false,
            error: None,
        }
    }

    /// Verifies settlement atomically replaces volatile evidence with one
    /// immutable chronological result and remains idempotent on replay.
    #[test]
    fn settled_action_result_commit_removes_volatile_copy_exactly_once() {
        let running = action_result("action-1", ActionStatus::Running, "still running");
        let settled = action_result("action-1", ActionStatus::Succeeded, "finished");
        let mut context = AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                label: "system".to_string(),
                content: "policy".to_string(),
            },
            ContextBlock::assistant_event("assistant response action-1", "execute action-1"),
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "action result action-1".to_string(),
                content: crate::action_result_context_content(&running),
            },
            ContextBlock {
                source: ContextSourceKind::RuntimeHint,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "scheduler".to_string(),
                content: "waiting".to_string(),
            },
        ])
        .unwrap();
        assert_eq!(
            context.commit_settled_action_results(&[settled]).unwrap(),
            1
        );
        let committed = context.blocks.clone();
        assert_eq!(
            context
                .commit_settled_action_results(&[action_result(
                    "action-1",
                    ActionStatus::Succeeded,
                    "finished"
                )])
                .unwrap(),
            0
        );
        assert_eq!(context.blocks, committed);
        let action_blocks = context
            .blocks
            .iter()
            .filter(|block| block.source == ContextSourceKind::ActionResult)
            .collect::<Vec<_>>();
        assert_eq!(action_blocks.len(), 1);
        assert_eq!(
            action_blocks[0].placement,
            crate::ContextPlacement::ConversationAppend
        );
        context.validate_placement_order().unwrap();
    }

    /// Verifies a batch containing unresolved controller state is rejected
    /// before any otherwise terminal sibling can mutate chronology.
    #[test]
    fn settled_action_result_commit_rejects_unresolved_batches_atomically() {
        let mut context = AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::System,
            placement: crate::ContextPlacement::StablePrefix,
            label: "system".to_string(),
            content: "policy".to_string(),
        }])
        .unwrap();
        let original = context.clone();
        let error = context
            .commit_settled_action_results(&[
                action_result("action-1", ActionStatus::Succeeded, "finished"),
                action_result("action-2", ActionStatus::Running, "running"),
            ])
            .unwrap_err();
        assert_eq!(
            error.message(),
            "only terminal action results may be committed to immutable chronology"
        );
        assert_eq!(context, original);
    }
}
