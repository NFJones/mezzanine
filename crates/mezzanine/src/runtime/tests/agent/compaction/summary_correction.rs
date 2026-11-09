//! Rejected completed summaries remain private while exact auxiliary work repairs them.

use super::*;

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
        let mut response = runtime_test_compaction_response("");
        response.action_batch = None;
        response.raw_text = raw.clone();
        run_runtime_test_compaction_wave(&mut service, "%1", Ok(response));
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
        for _ in 0..3 {
            let mut response = runtime_test_compaction_response("");
            response.action_batch = None;
            response.raw_text = raw.to_string();
            run_runtime_test_compaction_wave(&mut service, "%1", Ok(response));
            assert!(store.compaction_epoch(&conversation).unwrap().is_none());
        }
        assert!(!service.agent_is_compacting("%1"));
        let failure = persisted_compaction_failure(&service, &store, &conversation, &turn);
        assert!(failure.contains("invalid-output repair allowance exhausted"));
        assert!(failure.contains("phase=validating"));
        assert!(failure.contains("model_calls=3"));
        assert!(failure.contains("serial_depth=1"));
        assert!(!failure.contains(raw));
    }
}

/// A real decoded-summary memory overflow fails once, preserving original source.
/// Resource limits do not create a semantic length-reduction model loop.
#[test]
fn runtime_compaction_summary_byte_overflow_fails_without_regeneration() {
    let (mut service, store, turn) = replay_observed::queued();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let requests = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response(
            &"界".repeat(128 * 1024 / 3 + 1),
        )),
    );
    assert_eq!(requests.len(), 1);
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
}

/// Invalid-output repair cannot turn a later resource overflow into another
/// arbitrary shortening attempt; the original epoch remains authoritative.
#[test]
fn runtime_compaction_mixed_defects_do_not_regenerate_resource_overflow() {
    for first in ["", "```mezzanine-action-json\n{}\n```"] {
        let (mut service, store, turn) = replay_observed::queued();
        let conversation = service
            .agent_turn_ledger()
            .turn(&turn)
            .unwrap()
            .conversation_id
            .clone();
        complete_runtime_test_compaction(&mut service, "%1", first);
        assert!(
            service
                .pending_agent_compaction_task_for_tests("%1")
                .is_some()
        );
        complete_runtime_test_compaction(&mut service, "%1", &"界".repeat(128 * 1024 / 3 + 1));
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.take_replay_epoch_attempt_for_tests().is_none());
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    }
}

/// A nonretryable provider failure after private repair retires the exact
/// operation, without accepting rejected output or waking ordinary inference.
#[test]
fn runtime_compaction_provider_failure_after_repair_preserves_original_epoch() {
    let (mut service, store, turn) = replay_observed::queued();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    complete_runtime_test_compaction(&mut service, "%1", "");
    let requests = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Err(MezError::new(
            crate::error::MezErrorKind::Forbidden,
            "synthetic provider refusal",
        )),
    );
    assert_eq!(requests.len(), 1);
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    assert_eq!(
        service.agent_turn_ledger().turn(&turn).unwrap().state,
        AgentTurnState::Failed
    );
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
        let mut response = runtime_test_compaction_response("");
        response.action_batch = None;
        response.raw_text = raw.to_string();
        run_runtime_test_compaction_wave(&mut service, "%1", Ok(response));
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

/// An actual input rejection during output repair repacks the same frozen
/// source. It never changes profile output controls or publication ownership.
#[test]
fn runtime_compaction_output_repair_input_rejection_preserves_frozen_source() {
    let (mut service, store, turn) = replay_observed::queued();
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    complete_runtime_test_compaction(&mut service, "%1", "");
    let rejected = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Err(MezError::invalid_state("context length exceeded")
            .with_provider_failure_json(r#"{"error":{"code":"context_length_exceeded"}}"#)),
    );
    assert_eq!(rejected.len(), 1);
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(
        retry.request.as_ref().unwrap().max_output_tokens,
        original.request.as_ref().unwrap().max_output_tokens
    );
    assert_eq!(retry.compaction_epoch, original.compaction_epoch);
    assert!(std::sync::Arc::ptr_eq(
        retry.replay_source.as_ref().unwrap(),
        original.replay_source.as_ref().unwrap()
    ));
    assert!(
        store
            .compaction_epoch(&original.conversation_id)
            .unwrap()
            .is_none()
    );
    for _ in 0..8 {
        if service.pending_agent_compaction_tasks().is_empty() {
            break;
        }
        complete_runtime_test_compaction(&mut service, "%1", "bounded split summary");
    }
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, "bounded split summary");
}

/// Invalid synthesis retries the same private material while accepted raw
/// siblings remain represented. No original source is regenerated for repair.
#[test]
fn runtime_compaction_invalid_chunk_preserves_accepted_staging_and_synthesis() {
    let (mut service, store, turn) = replay_observed::queued();
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Err(MezError::invalid_state("context length exceeded")
            .with_provider_failure_json(r#"{"error":{"code":"context_length_exceeded"}}"#)),
    );
    let raw = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response(
            "accepted private material",
        )),
    );
    assert!(!raw.is_empty(), "genuine rejected input must repack");
    assert!(service.take_replay_epoch_attempt_for_tests().is_none());
    let invalid = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("")),
    );
    assert_eq!(invalid.len(), 1);
    assert!(
        invalid[0]
            .messages
            .last()
            .unwrap()
            .content
            .contains("accepted private material")
    );
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    let repaired = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response("accepted final synthesis")),
    );
    assert_eq!(repaired.len(), 1);
    assert_eq!(invalid[0].messages.last(), repaired[0].messages.last());
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
        let mut response = runtime_test_compaction_response("must not use malformed raw fallback");
        response.action_batch = Some(mez_agent::MaapBatch {
            rationale: "malformed fixture".into(),
            actions: vec![],
        });
        run_runtime_test_compaction_wave(&mut service, "%1", Ok(response));
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
        let attempts = if empty { 3 } else { 1 };
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

/// A usable long final after one empty output is accepted immediately. The
/// repair allowance is not a token/word target or a nonreducing-output fence.
#[test]
fn runtime_compaction_repaired_long_final_is_accepted_without_size_chasing() {
    let (mut service, store, turn) = replay_observed::queued();
    complete_runtime_test_compaction(&mut service, "%1", "");
    let summary = format!("{}END", "usable continuation ".repeat(4000));
    let sent = run_runtime_test_compaction_wave(
        &mut service,
        "%1",
        Ok(runtime_test_compaction_response(&summary)),
    );
    assert_eq!(sent.len(), 1);
    assert!(service.pending_agent_compaction_tasks().is_empty());
    replay_observed::adopt(&mut service, &turn);
    replay_observed::assert_head(&service, &store, &turn, &summary);
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
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let generation = task.task_generation;
    let conversation = task.conversation_id.clone();
    let before = service.agent_token_usage_for_conversation(&conversation);
    let sample = service.agent_latest_request_usage(&conversation).cloned();
    let mut response = runtime_test_compaction_response("");
    response.usage.input_tokens = 37;
    response.usage.output_tokens = 5;
    let event = crate::runtime::AgentCompactionEvent::Completed {
        pane_id: "%1".into(),
        task_generation: generation,
        response: Box::new(response.clone()),
    };
    run_runtime_test_compaction_wave(&mut service, "%1", Ok(response));
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

/// Empty output repairs privately; a valid over-advisory final publishes
/// without another call, while genuine byte-resource overflow fails once.
#[test]
fn runtime_compaction_completed_output_uses_validity_and_resource_limits() {
    for kind in ["long", "empty", "bytes"] {
        let (mut service, store, turn) = replay_observed::queued();
        let original = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        let summary = match kind {
            "empty" => String::new(),
            "bytes" => "界".repeat(128 * 1024 / 3 + 1),
            _ => format!("{}END", "usable summary ".repeat(4000)),
        };
        complete_runtime_test_compaction(&mut service, "%1", &summary);
        if kind == "bytes" {
            assert!(!service.agent_is_compacting("%1"));
            assert!(service.take_replay_epoch_attempt_for_tests().is_none());
            assert!(
                store
                    .compaction_epoch(&original.conversation_id)
                    .unwrap()
                    .is_none()
            );
        } else {
            let expected = if kind == "empty" {
                let retry = service
                    .pending_agent_compaction_task_for_tests("%1")
                    .unwrap();
                assert_ne!(retry.task_generation, original.task_generation);
                assert_eq!(retry.compaction_epoch, original.compaction_epoch);
                let requests = run_runtime_test_compaction_wave(
                    &mut service,
                    "%1",
                    Ok(runtime_test_compaction_response("valid repaired summary")),
                );
                assert!(requests[0].messages.iter().any(|message| {
                    message
                        .content
                        .starts_with("[private epoch compaction output repair]")
                }));
                "valid repaired summary"
            } else {
                &summary
            };
            replay_observed::adopt(&mut service, &turn);
            replay_observed::assert_head(&service, &store, &turn, expected);
            assert!(service.agent_provider_task_is_pending(&turn));
        }
    }
}
