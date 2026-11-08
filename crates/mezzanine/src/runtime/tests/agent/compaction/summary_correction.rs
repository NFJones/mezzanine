//! Rejected completed summaries remain private while exact auxiliary work repairs them.

use super::*;
use crate::runtime::RuntimeAgentCompactionTarget;

/// Canonical raw-text auxiliary responses may contain dense Unicode JSON or an
/// optional final-say frame. Measure and publish the payload, never its wrapper;
/// top-level/nested action and rationale data must remain ordinary summary text
/// on both automatic and manual paths, including after a private correction.
#[test]
fn runtime_compaction_raw_and_framed_payloads_preserve_summary_identity() {
    let dense = r#"{"decision":{"actions":["保留🦀"],"rationale":"retain evidence"}}"#;
    let batch = serde_json::json!({"rationale":"fixture", "actions":[{
        "type":"say", "status":"final", "text":"保留🦀 bounded summary",
        "content_type":"text/plain; charset=utf-8",
    }]})
    .to_string();
    for (raw, expected) in [
        (dense.to_string(), dense),
        (
            r#"{"actions":["retain file evidence"],"decision":"done"}"#.to_string(),
            r#"{"actions":["retain file evidence"],"decision":"done"}"#,
        ),
        (
            r#"{"rationale":"keep evidence","actions":["inspect files"]}"#.to_string(),
            r#"{"rationale":"keep evidence","actions":["inspect files"]}"#,
        ),
        (
            r#"{"rationale":"keep evidence","next":"inspect files"}"#.to_string(),
            r#"{"rationale":"keep evidence","next":"inspect files"}"#,
        ),
        // Unframed incomplete JSON is raw text, not positive MAAP identity.
        (
            r#"{"rationale":"unfinished","actions":["#.to_string(),
            r#"{"rationale":"unfinished","actions":["#,
        ),
        (
            "```json\n{\"actions\":[\"retain files\"]}\n```".to_string(),
            "```json\n{\"actions\":[\"retain files\"]}\n```",
        ),
        (batch.clone(), "保留🦀 bounded summary"),
        (
            format!("```mezzanine-action-json\n{batch}\n```"),
            "保留🦀 bounded summary",
        ),
    ] {
        let (mut service, store, turn) = replay_observed::queued();
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        service.claim_agent_compaction_task_state("%1", task);
        let mut response = runtime_test_compaction_response("");
        response.action_batch = None;
        response.raw_text = raw.clone();
        service
            .apply_agent_compaction_completed_event("%1", response)
            .unwrap();
        replay_observed::adopt(&mut service, &turn);
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.agent_provider_task_is_pending(&turn));
        replay_observed::assert_head(&service, &store, &turn, expected);
        qualify_manual_summary_correction(false, Some((&raw, expected)), false);
    }
}

/// Recognizable malformed JSON/fenced frames get exactly two auxiliary repairs,
/// even without adapter MAAP parsing. No frame or partial payload is published,
/// and terminal auxiliary evidence survives normal execution-state retirement.
#[test]
fn runtime_compaction_raw_malformed_frames_exhaust_finitely() {
    for raw in [
        r#"{"rationale":"fixture","actions":[]}"#,
        r#"{"rationale":"","actions":[{"type":"say","status":"final","text":"invalid rationale"}]}"#,
        r#"{"rationale":"fixture","actions":[{"type":"unknown_fixture_action"}]}"#,
        r#"{"rationale":"fixture","actions":[{"type":"say","status":"progress","text":"not final"}]}"#,
        "```mezzanine-action-json\n{}",
        "```mezzanine-action-json\n{}\n```\n```mezzanine-action-json\n{}\n```",
    ] {
        let (mut service, store, turn) = replay_observed::queued();
        let conversation = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .conversation_id
            .clone();
        for attempt in 0..3 {
            let task = service.take_pending_agent_compaction_task("%1").unwrap();
            assert_eq!(task.summary_correction.corrections_admitted, attempt);
            service.claim_agent_compaction_task_state("%1", task);
            let mut response = runtime_test_compaction_response("");
            response.action_batch = None;
            response.raw_text = raw.to_string();
            service
                .apply_agent_compaction_completed_event("%1", response)
                .unwrap();
            assert!(store.compaction_epoch(&conversation).unwrap().is_none());
        }
        assert!(!service.agent_is_compacting("%1"));
        let failure = persisted_compaction_failure(&service, &store, &conversation, &turn);
        assert!(failure.contains("disposition=EmptyOrMalformedExhausted"));
        assert!(failure.contains("corrections_admitted=2"));
        assert!(!failure.contains(raw));
    }
}

/// Dense one-word output exceeds the byte bound and records estimated token
/// progress across real corrections. Guidance keeps its token unit and original
/// provider allocation; final bounded output publishes through the same source.
#[test]
fn runtime_compaction_summary_correction_token_budget_requires_size_progress() {
    let (mut service, store, turn) = replay_observed::queued();
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let mut source_generation = None;
    let mut previous_cost = usize::MAX;
    let mut previous_bytes = usize::MAX;
    for extra in [16, 8] {
        // Real byte overflow supplies dense one-word token evidence; an
        // advisory word/token target cannot reject canonical output by itself.
        complete_runtime_test_compaction(&mut service, "%1", &"界".repeat(128 * 1024 / 3 + extra));
        let retry = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap();
        let rejected = retry.summary_correction.last_rejection.unwrap();
        assert!(rejected.estimated_tokens);
        assert!(rejected.cost < previous_cost && rejected.bytes < previous_bytes);
        previous_cost = rejected.cost;
        previous_bytes = rejected.bytes;
        let generation = retry.summary_correction.source_generation.unwrap();
        assert_eq!(*source_generation.get_or_insert(generation), generation);
        assert_eq!(
            retry.request.max_output_tokens,
            original.request.max_output_tokens
        );
        assert_eq!(retry.compaction_epoch, original.compaction_epoch);
        assert!(
            retry
                .request
                .messages
                .iter()
                .any(|m| m.content.contains("estimated input tokens"))
        );
        assert!(
            store
                .load_replay_epoch(&original.conversation_id, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
        assert!(!service.agent_provider_task_is_pending(&turn));
    }
    complete_runtime_test_compaction(&mut service, "%1", "accepted token-progress summary");
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, "accepted token-progress summary");
}

/// Empty/malformed output consumes its separate finite budget but cannot erase
/// the last size-bearing predecessor. Initial blank -> oversize and oversize ->
/// blank -> improving oversize both remain eligible on the same frozen source.
#[test]
fn runtime_compaction_mixed_summary_defects_preserve_measured_progress() {
    for malformed in [false, true] {
        let (mut service, store, turn) = replay_observed::queued();
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        for step in 0..4 {
            if step % 2 == 0 && malformed {
                let pending = service.take_pending_agent_compaction_task("%1").unwrap();
                service.claim_agent_compaction_task_state("%1", pending);
                let mut response = runtime_test_compaction_response("not semantic size evidence");
                response.action_batch = Some(mez_agent::MaapBatch {
                    rationale: "malformed fixture".into(),
                    actions: vec![],
                });
                service
                    .apply_agent_compaction_completed_event("%1", response)
                    .unwrap();
            } else {
                let output = if step % 2 == 0 {
                    String::new()
                } else {
                    // Byte overflow is a real semantic rejection for canonical
                    // output; the advisory word target is not an acceptance cap.
                    "界".repeat(128 * 1024 / 3 + 4 - step)
                };
                complete_runtime_test_compaction(&mut service, "%1", &output);
            }
            assert!(
                service
                    .pending_agent_compaction_task_for_tests("%1")
                    .is_some(),
                "step={step} malformed={malformed}"
            );
            assert!(
                store
                    .compaction_epoch(&task.conversation_id)
                    .unwrap()
                    .is_none()
            );
        }
        complete_runtime_test_compaction(&mut service, "%1", "short accepted mixed-defect summary");
        replay_observed::adopt(&mut service, &turn);
        replay_observed::assert_head(
            &service,
            &store,
            &turn,
            "short accepted mixed-defect summary",
        );
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.agent_provider_task_is_pending(&turn));
    }
}

/// Correction history survives a subsequent nonretryable provider failure and
/// automatic terminal wrapping. It is auxiliary evidence, not synthetic MAAP
/// attribution, and no rejected summary or partial epoch becomes authoritative.
#[test]
fn runtime_compaction_provider_failure_preserves_correction_provenance() {
    let (mut service, store, turn) = replay_observed::queued();
    complete_runtime_test_compaction(&mut service, "%1", "");
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    task.compaction_request_shape = Some((
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        Default::default(),
        false,
    ));
    let conversation = task.conversation_id.clone();
    service.claim_agent_compaction_task_state("%1", task);
    assert!(
        service
            .apply_agent_compaction_failed_event(
                "%1",
                "forbidden",
                "synthetic provider refusal",
                None
            )
            .unwrap()
    );
    assert!(!service.agent_is_compacting("%1"));
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    let failure = persisted_compaction_failure(&service, &store, &conversation, &turn);
    assert!(failure.contains("phase=ProviderInference"));
    assert!(failure.contains("corrections_admitted=1"));
    assert!(failure.contains("disposition=ProviderRejected"));
    assert!(failure.contains("openai_max_output_tokens_transmitted=Some(false)"));
}

/// Manual compaction uses the same bounded completed-output repair and frozen
/// durable rows. Rejecting output keeps the original archive authoritative;
/// accepting a short model summary publishes once without an ordinary turn.
#[test]
fn runtime_manual_compaction_invalid_completed_summary_repairs_privately() {
    qualify_manual_summary_correction(false, None, false);
}

/// A valid repaired manual summary cannot publish over selected durable rows
/// rewritten while auxiliary correction is in flight. Immutable original rows
/// are revalidated under the original writer's lease before publication.
#[test]
fn runtime_manual_compaction_summary_correction_rejects_source_changed_during_retry() {
    qualify_manual_summary_correction(true, None, false);
}

/// A repaired proposal keeps the original selected source through correction
/// even when another installed store reuses its conversation with other bytes.
/// Publication and MCP controls belong to the original store; the replacement
/// receives neither a transcript append nor a compaction epoch from this job.
#[test]
fn runtime_manual_compaction_summary_correction_preserves_original_store() {
    qualify_manual_summary_correction(false, None, true);
}

/// Seeds one manual frozen source and optionally replaces its row during repair
/// or delivers raw provider text to qualify optional-frame versus JSON identity.
fn qualify_manual_summary_correction(mutate: bool, raw: Option<(&str, &str)>, replace_store: bool) {
    let (mut service, store) =
        manual_model::queued("manual-correction", "original manual source ".repeat(100));
    let before = store.inspect("manual-correction").unwrap();
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .replay_source
        .clone()
        .expect("manual correction must use canonical source");
    complete_runtime_test_compaction(&mut service, "%1", "");
    assert!(service.agent_is_compacting("%1"));
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &original,
        retry.replay_source.as_ref().unwrap()
    ));
    assert_eq!(retry.summary_correction.corrections_admitted, 1);
    assert!(
        store
            .compaction_epoch("manual-correction")
            .unwrap()
            .is_none()
    );
    assert_eq!(store.inspect("manual-correction").unwrap(), before);
    let replacement = if replace_store {
        let changed = AgentTranscriptStore::new(temp_root("manual-output-correction-mutated"));
        let mut row = before[0].clone();
        row.content = "genuinely rewritten selected source".into();
        changed.append(&row).unwrap();
        service.set_agent_transcript_store(changed.clone());
        Some((changed, row))
    } else {
        None
    };
    let changed = if mutate {
        let changed = store.clone();
        changed
            .compare_and_swap_entry_content(
                "manual-correction",
                1,
                "%1",
                &changed.transcript_entry_revision(&before[0]).unwrap(),
                "genuinely rewritten selected source".into(),
            )
            .unwrap();
        Some(changed)
    } else {
        None
    };
    let expected = if let Some((raw, expected)) = raw {
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        service.claim_agent_compaction_task_state("%1", task);
        let mut response = runtime_test_compaction_response("");
        response.action_batch = None;
        response.raw_text = raw.to_string();
        service
            .apply_agent_compaction_completed_event("%1", response)
            .unwrap();
        expected
    } else {
        complete_runtime_test_compaction(&mut service, "%1", "short accepted manual summary");
        "short accepted manual summary"
    };
    // Inference has not committed a head or completed the idle operation. The
    // original off-actor writer, not actor-side filesystem inspection, checks
    // the frozen physical source and returns its real receipt or refusal.
    assert!(service.agent_is_compacting("%1"));
    assert!(
        store
            .load_replay_epoch("manual-correction", mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    let completion = service
        .take_replay_epoch_attempt_for_tests()
        .expect("original canonical publication attempt")
        .execute();
    assert_eq!(completion.error().is_some(), mutate);
    service.apply_replay_epoch_completion(completion).unwrap();
    assert!(!service.agent_is_compacting("%1"));
    if let Some(changed) = changed {
        assert!(
            changed
                .compaction_epoch("manual-correction")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .compaction_epoch("manual-correction")
                .unwrap()
                .is_none()
        );
        assert!(service.pending_agent_provider_tasks().is_empty());
        return;
    }
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch("manual-correction", mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology()[0].block().content, expected);
    assert_eq!(store.inspect("manual-correction").unwrap(), before);
    if let Some((replacement, row)) = replacement {
        assert_eq!(replacement.inspect("manual-correction").unwrap(), vec![row]);
        assert!(
            replacement
                .compaction_epoch("manual-correction")
                .unwrap()
                .is_none()
        );
    }
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// Human pause blocks a newly queued correction claim; Stop cancels that exact
/// owner. Neither lifecycle action can publish rejected output or wake an
/// ordinary request, and the logical compaction marker is cleared after Stop.
#[test]
fn runtime_compaction_summary_correction_respects_human_pause_and_stop() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (mut service, store, turn) = replay_observed::queued_with_service(service);
    let primary = service
        .attach_primary(
            "correction-controller",
            true,
            Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    complete_runtime_test_compaction(&mut service, "%1", "");
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    let conversation = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .conversation_id
        .clone();
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    assert!(
        service
            .claim_agent_compaction_task("%1", generation)
            .unwrap()
            .is_none()
    );
    assert!(service.agent_is_compacting("%1"));
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(!service.agent_is_compacting("%1"));
    assert!(!service.agent_provider_task_is_pending(&turn));
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Context rejection during a correction splits only temporary input. The
/// fresh request must retain the advisory target/unit and correction
/// guidance, then complete frozen chunks and synthesis without ordinary replay.
#[test]
fn runtime_compaction_summary_correction_preserves_ceiling_after_context_split() {
    let (mut service, store, turn) = replay_observed::queued();
    complete_runtime_test_compaction(&mut service, "%1", &"too big ".repeat(50_000));
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    let allocation = task.request.max_output_tokens;
    let guidance = task.summary_correction.guidance_target;
    let epoch = task.compaction_epoch;
    let conversation = task.conversation_id.clone();
    task.compaction_request_shape = Some((
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        Default::default(),
        false,
    ));
    service.claim_agent_compaction_task_state("%1", task);
    assert!(
        service
            .apply_agent_compaction_failed_event(
                "%1",
                "invalid_state",
                "context length exceeded",
                Some(r#"{"error":{"code":"context_length_exceeded"}}"#)
            )
            .unwrap()
    );
    let split = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(split.request.max_output_tokens, allocation);
    assert_eq!(split.summary_correction.guidance_target, guidance);
    assert_eq!(split.compaction_epoch, epoch);
    assert!(
        split
            .request
            .messages
            .iter()
            .any(|message| message.content.starts_with("[private summary correction]"))
    );
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    let mut completed_summaries = 0;
    for _ in 0..8 {
        if service
            .pending_agent_compaction_task_for_tests("%1")
            .is_none()
        {
            break;
        }
        complete_runtime_test_compaction(&mut service, "%1", "bounded split summary");
        completed_summaries += 1;
    }
    assert!(
        completed_summaries >= 2,
        "split input must complete multiple auxiliary responses"
    );
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, "bounded split summary");
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.agent_provider_task_is_pending(&turn));
}

/// Invalid later chunks preserve earlier accepted summaries and pending input;
/// invalid synthesis repeats the exact private synthesis request rather than
/// re-staging its rejected text. A valid next response advances finite source.
#[test]
fn runtime_compaction_invalid_chunk_preserves_accepted_staging_and_synthesis() {
    let (mut service, store, turn) = replay_observed::queued();
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    let conversation = task.conversation_id.clone();
    task.compaction_request_shape = Some((
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        Default::default(),
        false,
    ));
    service.claim_agent_compaction_task_state("%1", task);
    assert!(
        service
            .apply_agent_compaction_failed_event(
                "%1",
                "invalid_state",
                "context length exceeded",
                Some(r#"{"error":{"code":"context_length_exceeded"}}"#)
            )
            .unwrap()
    );
    // Accept a genuine first chunk before rejecting the next issued chunk.
    // No completed count or accepted summary is assigned by the fixture.
    complete_runtime_test_compaction(&mut service, "%1", "accepted earlier chunk");
    let before = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let RuntimeAgentCompactionTarget::ActiveTurn {
        completed_summaries: accepted,
        pending_blocks: pending,
        current_blocks: current,
        completed_responses: count,
        ..
    } = &before.target
    else {
        panic!("active turn");
    };
    assert_eq!(accepted, &["accepted earlier chunk"]);
    complete_runtime_test_compaction(&mut service, "%1", "");
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let RuntimeAgentCompactionTarget::ActiveTurn {
        completed_summaries,
        pending_blocks,
        current_blocks,
        completed_responses,
        ..
    } = &retry.target
    else {
        panic!("active turn");
    };
    assert_eq!(completed_summaries, accepted);
    assert_eq!(pending_blocks, pending);
    assert_eq!(current_blocks, current);
    assert_eq!(completed_responses, count);
    for _ in 0..8 {
        let next = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap();
        if matches!(
            &next.target,
            RuntimeAgentCompactionTarget::ActiveTurn {
                synthesis_source_bytes: Some(_),
                ..
            }
        ) {
            break;
        }
        complete_runtime_test_compaction(&mut service, "%1", "accepted later chunk");
    }
    let synthesis = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    assert!(matches!(
        &synthesis.target,
        RuntimeAgentCompactionTarget::ActiveTurn {
            synthesis_source_bytes: Some(_),
            ..
        }
    ));
    let input = &synthesis.request.messages.last().unwrap().content;
    assert!(input.contains("accepted earlier chunk") && input.contains("accepted later chunk"));
    complete_runtime_test_compaction(&mut service, "%1", "");
    let correction = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(
        correction.request.messages.last(),
        synthesis.request.messages.last()
    );
    assert!(
        store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    assert!(!service.agent_provider_task_is_pending(&turn));
    complete_runtime_test_compaction(&mut service, "%1", "accepted final synthesis");
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, "accepted final synthesis");
}

/// A completed batch with executable content is malformed summary output, not
/// authorization to run actions. Its finite correction replaces only inference
/// and requires a final summary response before ordinary work can continue.
#[test]
fn runtime_compaction_malformed_completed_batch_is_private_and_bounded() {
    let (mut service, store, turn) = replay_observed::queued();
    let conversation = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .conversation_id
        .clone();
    for attempt in 0..3 {
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        service.claim_agent_compaction_task_state("%1", task);
        let mut response = runtime_test_compaction_response("must not use malformed raw fallback");
        response.action_batch = Some(mez_agent::MaapBatch {
            rationale: "malformed fixture".into(),
            actions: vec![],
        });
        assert!(
            service
                .apply_agent_compaction_completed_event("%1", response)
                .unwrap()
        );
        assert_eq!(service.agent_is_compacting("%1"), attempt < 2);
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    }
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
}

/// Repeated blank responses have exactly two auxiliary corrections even when
/// transport retries are unlimited. No-progress oversized output fails on its
/// next completion rather than redispatching indefinitely or publishing it.
#[test]
fn runtime_compaction_summary_correction_exhausts_without_publication() {
    for empty in [false, true] {
        let (mut service, store, turn) = replay_observed::queued();
        service.configure_provider_retry_policy(mez_agent::ProviderRetryPolicy {
            max_attempts: 0,
            unlimited: true,
            initial_delay_ms: 20,
            max_delay_ms: 40,
        });
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        let invalid = if empty {
            String::new()
        } else {
            // Canonical summary targets are advisory. Exceed the actual
            // bounded payload size to establish a rejected complete summary.
            "x ".repeat(70_000)
        };
        let attempts = if empty { 3 } else { 2 };
        for index in 0..attempts {
            complete_runtime_test_compaction(&mut service, "%1", &invalid);
            assert!(
                store
                    .compaction_epoch(&task.conversation_id)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(service.agent_is_compacting("%1"), index + 1 < attempts);
        }
        assert_eq!(
            service.agent_turn_ledger().turn(&turn).unwrap().state,
            AgentTurnState::Failed
        );
        assert!(!service.agent_provider_task_is_pending(&turn));
    }
}

/// A frozen-valid summary is accepted even after guidance reached one. A later
/// still-invalid but improving completion at that issued floor fails finitely
/// and reports real auxiliary counts without manufacturing MAAP attribution.
#[test]
fn runtime_compaction_summary_correction_floor_validates_before_exhaustion() {
    for valid in [true, false] {
        let (mut service, store, turn) = replay_observed::queued();
        let original = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        let mut completed = 0;
        // Actual rejected responses reach the issued floor through fresh claims;
        // the fixture does not assign a count or a private rejection predecessor.
        for step in 0..32 {
            if service
                .pending_agent_compaction_task_for_tests("%1")
                .unwrap()
                .summary_correction
                .guidance_target
                == Some(1)
            {
                break;
            }
            complete_runtime_test_compaction(
                &mut service,
                "%1",
                &"界".repeat(128 * 1024 / 3 + 256 - 4 * step),
            );
            completed += 1;
        }
        let floor = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap();
        assert_eq!(floor.summary_correction.guidance_target, Some(1));
        assert_eq!(floor.summary_correction.corrections_admitted, completed);
        assert!(completed > 0);
        assert_eq!(
            floor.request.max_output_tokens,
            original.request.max_output_tokens
        );
        let response = if valid {
            "valid bounded summary".to_string()
        } else {
            "界".repeat(128 * 1024 / 3 + 256 - 4 * usize::try_from(completed).unwrap())
        };
        complete_runtime_test_compaction(&mut service, "%1", &response);
        if valid {
            replay_observed::adopt(&mut service, &turn);
            replay_observed::assert_head(&service, &store, &turn, &response);
        } else {
            assert!(!service.agent_is_compacting("%1"));
            assert!(!service.agent_provider_task_is_pending(&turn));
            assert!(
                store
                    .load_replay_epoch(&original.conversation_id, mez_agent::AgentContext::empty())
                    .unwrap()
                    .is_none()
            );
            let failure =
                persisted_compaction_failure(&service, &store, &original.conversation_id, &turn);
            assert!(failure.contains("disposition=GuidanceFloor"));
            assert!(failure.contains(&format!("corrections_admitted={completed}")));
            assert!(!failure.contains(&response));
        }
    }
}

/// A selected-source rewrite is an integrity failure even when output is blank.
/// Correctable output must not mask a genuinely changed frozen chronology or
/// queue another inference against corrupt evidence.
#[test]
fn runtime_compaction_invalid_summary_does_not_retry_mutated_source() {
    let (mut service, store, turn) = replay_observed::queued();
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let context = service.agent_turn_contexts().get(&turn).unwrap().clone();
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(&task.conversation_id).unwrap(),
        context.clone(),
    )
    .unwrap();
    // A checked descriptive edit preserves identity but changes selected raw
    // bytes. It cannot preserve this already frozen compaction source or gain
    // the old controller's continuation rights by reconstruction.
    let edited = owner
        .checkpoint()
        .unwrap()
        .revise_raw_member(owner.members()[0].occurrence_id(), "new source".into())
        .unwrap();
    let changed = mez_agent::ReplayEpochContext::from_checkpoint(
        mez_agent::ReplayConversationId::new(&task.conversation_id).unwrap(),
        edited,
        mez_agent::AgentContext::empty(),
    )
    .unwrap();
    service
        .agent_turn_contexts_mut()
        .insert(turn.clone(), changed.into_owned_context());
    complete_runtime_test_compaction(&mut service, "%1", "");
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
    assert!(
        store
            .compaction_epoch(&task.conversation_id)
            .unwrap()
            .is_none()
    );
}

/// Discarded output still incurred expense once; duplicate delivery cannot
/// account it again, overwrite the ordinary execution sample or retire the
/// newly queued correction. Cancellation then makes its late completion inert.
#[test]
fn runtime_compaction_invalid_summary_usage_duplicate_and_cancel_are_exact() {
    let (mut service, store, turn) = replay_observed::queued();
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    let generation = task.task_generation;
    let conversation = task.conversation_id.clone();
    let before = service.agent_token_usage_for_conversation(&conversation);
    let sample = service.agent_latest_request_usage(&conversation).cloned();
    service.claim_agent_compaction_task_state("%1", task);
    let mut response = runtime_test_compaction_response("");
    response.usage.input_tokens = 37;
    response.usage.output_tokens = 5;
    let event = crate::runtime::AgentCompactionEvent::Completed {
        pane_id: "%1".into(),
        task_generation: generation,
        response: Box::new(response),
    };
    assert!(
        service
            .apply_agent_compaction_transition(event.clone())
            .unwrap()
            .applied
    );
    let expense = service.agent_token_usage_for_conversation(&conversation);
    assert_ne!(expense, before);
    assert!(
        !service
            .apply_agent_compaction_transition(event)
            .unwrap()
            .applied
    );
    assert_eq!(
        expense,
        service.agent_token_usage_for_conversation(&conversation)
    );
    assert_eq!(
        sample,
        service.agent_latest_request_usage(&conversation).cloned()
    );
    let retry = service.take_pending_agent_compaction_task("%1").unwrap();
    let generation = retry.task_generation;
    service.claim_agent_compaction_task_state("%1", retry);
    service.cancel_current_agent_compaction_task("%1");
    assert!(
        !service
            .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
                pane_id: "%1".into(),
                task_generation: generation,
                response: Box::new(runtime_test_compaction_response("must not publish")),
            })
            .unwrap()
            .applied
    );
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    assert!(!service.agent_provider_task_is_pending(&turn));
}

/// Accepted inference hands ownership to actual publication. A late provider
/// failure cannot retire that work either before its write or after an actual
/// durable receipt has returned but has not yet been adopted by the actor.
#[test]
fn runtime_compaction_late_failed_callback_cannot_cancel_accepted_publication() {
    for after_write in [false, true] {
        for malformed in [false, true] {
            let (mut service, store, turn) = replay_observed::queued();
            let task = service
                .pending_agent_compaction_task_for_tests("%1")
                .unwrap()
                .clone();
            complete_runtime_test_compaction(&mut service, "%1", "accepted original publication");
            let expense = service.agent_token_usage_for_conversation(&task.conversation_id);
            let before = service.agent_turn_contexts()[&turn].clone();
            let written =
                after_write.then(|| replay_publication::execute_pending_publication(&mut service));
            let late = crate::runtime::AgentCompactionEvent::Failed {
                pane_id: "%1".into(),
                task_generation: task.task_generation,
                kind: if malformed { "invalid_args" } else { "io" }.into(),
                message: "late provider result after accepted inference".into(),
                usage: mez_agent::ModelTokenUsage {
                    input_tokens: 999,
                    ..Default::default()
                },
                provider_failure_json: malformed
                    .then(|| r#"{"type":"malformed_model_output"}"#.into()),
                provider_raw_text: None,
            };
            for _ in 0..2 {
                assert!(
                    !service
                        .apply_agent_compaction_transition(late.clone())
                        .unwrap()
                        .applied,
                    "late failure must not replace accepted publication: written={after_write} malformed={malformed}"
                );
            }
            assert_eq!(service.agent_turn_contexts()[&turn], before);
            assert!(!service.agent_provider_task_is_pending(&turn));
            assert_eq!(
                service.agent_token_usage_for_conversation(&task.conversation_id),
                expense
            );
            assert!(service.pending_agent_compaction_tasks().is_empty());
            let receipt = written
                .unwrap_or_else(|| replay_publication::execute_pending_publication(&mut service));
            assert!(
                service
                    .apply_replay_epoch_completion(receipt)
                    .unwrap()
                    .applied
            );
            assert!(service.agent_provider_task_is_pending(&turn));
            replay_observed::assert_head(&service, &store, &turn, "accepted original publication");
        }
    }
}

/// Empty and byte-heavy outputs require private correction before publication.
/// A bounded summary above the advisory word target still reaches actual final
/// request sizing and can publish without an unnecessary inference retry.
#[test]
fn runtime_compaction_corrects_invalid_completed_summary_before_publication() {
    for kind in ["budget", "empty", "bytes"] {
        let (mut service, store, turn_id) = replay_observed::queued();
        let original = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } =
            &original.target
        else {
            panic!("expected active turn");
        };
        let invalid = match kind {
            "budget" => "word "
                .repeat(plan.summary_budget_words() + 1)
                .trim()
                .to_owned(),
            "empty" => " \n\t".to_string(),
            _ => "界".repeat(128 * 1024 / 3 + 1),
        };
        complete_runtime_test_compaction(&mut service, "%1", &invalid);
        if kind == "budget" {
            assert!(service.pending_agent_compaction_tasks().is_empty());
            assert!(
                store
                    .load_replay_epoch(&original.conversation_id, mez_agent::AgentContext::empty())
                    .unwrap()
                    .is_none()
            );
            replay_observed::adopt(&mut service, &turn_id);
            replay_observed::assert_head(&service, &store, &turn_id, &invalid);
            continue;
        }
        let retry = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap_or_else(|| panic!("{kind}: invalid output must queue auxiliary correction"));
        assert_ne!(retry.task_generation, original.task_generation);
        assert_eq!(retry.compaction_epoch, original.compaction_epoch);
        assert_eq!(retry.resume_turn_id, original.resume_turn_id);
        let guidance = &retry
            .request
            .messages
            .iter()
            .find(|message| message.content.starts_with("[private summary correction]"))
            .unwrap()
            .content;
        assert!(guidance.contains("complete proposed request"));
        assert!(!guidance.contains("authoritative semantic allowance"));
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        assert!(
            store
                .load_replay_epoch(&original.conversation_id, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
        assert!(
            service
                .agent_turn_ledger()
                .turns()
                .iter()
                .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
        );
        complete_runtime_test_compaction(&mut service, "%1", "valid short summary");
        replay_observed::adopt(&mut service, &turn_id);
        replay_observed::assert_head(&service, &store, &turn_id, "valid short summary");
        assert!(service.agent_provider_task_is_pending(&turn_id));
        assert!(!service.agent_is_compacting("%1"));
    }
}
