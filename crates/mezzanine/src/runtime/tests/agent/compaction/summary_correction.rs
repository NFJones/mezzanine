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
        let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
        let task = service.take_pending_agent_compaction_task("%1").unwrap();
        let conversation = task.conversation_id.clone();
        service.claim_agent_compaction_task_state("%1", task);
        let mut response = runtime_test_compaction_response("");
        response.action_batch = None;
        response.raw_text = raw.clone();
        service
            .apply_agent_compaction_completed_event("%1", response)
            .unwrap();
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.agent_provider_task_is_pending(&turn));
        let epoch = store.compaction_epoch(&conversation).unwrap().unwrap();
        assert!(
            epoch.summary == expected || epoch.ranges.iter().any(|range| range.summary == expected)
        );
        qualify_manual_summary_correction(false, Some((&raw, expected)));
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
        let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
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

/// Dense code-like output has one whitespace word but many estimated input
/// tokens. A corrected completion above the smaller guidance target but within
/// the ORIGINAL token allowance must proceed to complete-request validation.
/// Guidance must not change acceptance or the configured provider allocation.
#[test]
fn runtime_compaction_summary_correction_token_budget_requires_size_progress() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    let conversation = task.conversation_id.clone();
    let context = service.agent_turn_contexts().get(&turn).unwrap();
    let token_plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
        context,
        20000,
        10,
        context.event_sequence_high_water_mark(),
        mez_agent::ProviderBudgetProjection::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            "runtime-batch",
        ),
    )
    .unwrap();
    let budget = token_plan.summary_budget_words();
    let RuntimeAgentCompactionTarget::ActiveTurn {
        plan,
        current_blocks,
        ..
    } = &mut task.target
    else {
        panic!("active turn");
    };
    *current_blocks = token_plan.replacement_blocks().to_vec();
    **plan = token_plan;
    task.request.max_output_tokens = Some(budget);
    service.queue_agent_compaction_task(task);
    complete_runtime_test_compaction(&mut service, "%1", &"abcd".repeat(budget + 1));
    let first = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let allocation = first.request.max_output_tokens;
    assert!(
        first
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("estimated input tokens"))
    );
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    let corrected = "abcd".repeat(budget / 2 + 1);
    assert!(mez_agent::provider_text_input_token_estimate(&corrected) <= budget);
    complete_runtime_test_compaction(&mut service, "%1", &corrected);
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.agent_provider_task_is_pending(&turn));
    assert_eq!(allocation, Some(budget));
}

/// Empty/malformed output consumes its separate finite budget but cannot erase
/// the last size-bearing predecessor. Initial blank -> oversize and oversize ->
/// blank -> improving oversize both remain eligible on the same frozen source.
#[test]
fn runtime_compaction_mixed_summary_defects_preserve_measured_progress() {
    for malformed in [false, true] {
        let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        let RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } = &task.target else {
            panic!("active turn");
        };
        let budget = plan.summary_budget_words();
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
                    "x ".repeat(budget + 4 - step)
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
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.agent_provider_task_is_pending(&turn));
    }
}

/// Correction history survives a subsequent nonretryable provider failure and
/// automatic terminal wrapping. It is auxiliary evidence, not synthetic MAAP
/// attribution, and no rejected summary or partial epoch becomes authoritative.
#[test]
fn runtime_compaction_provider_failure_preserves_correction_provenance() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
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
    qualify_manual_summary_correction(false, None);
}

/// A valid repaired manual summary cannot publish over selected durable rows
/// rewritten while auxiliary correction is in flight. Immutable original rows
/// are revalidated by the same candidate owner used for correction admission.
#[test]
fn runtime_manual_compaction_summary_correction_rejects_source_changed_during_retry() {
    qualify_manual_summary_correction(true, None);
}

/// Seeds one manual frozen source and optionally replaces its row during repair
/// or delivers raw provider text to qualify optional-frame versus JSON identity.
fn qualify_manual_summary_correction(mutate: bool, raw: Option<(&str, &str)>) {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-correction".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"manual-correction\"\n[providers.runtime-batch]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.manual-correction]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=40000\nmax_input_tokens=20000\n".into(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-output-correction"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "manual-correction".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "old".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: "original manual source ".repeat(100),
        })
        .unwrap();
    let before = store.inspect("manual-correction").unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-correction", 1)
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    complete_runtime_test_compaction(&mut service, "%1", "");
    assert!(service.agent_is_compacting("%1"));
    assert!(
        store
            .compaction_epoch("manual-correction")
            .unwrap()
            .is_none()
    );
    assert_eq!(store.inspect("manual-correction").unwrap(), before);
    let changed = if mutate {
        let changed = AgentTranscriptStore::new(temp_root("manual-output-correction-mutated"));
        let mut row = before[0].clone();
        row.content = "genuinely rewritten selected source".into();
        changed.append(&row).unwrap();
        service.set_agent_transcript_store(changed.clone());
        let task = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap();
        let error = service
            .manual_compaction_candidate_context(task, "short accepted manual summary")
            .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
        assert_eq!(
            error.message(),
            "summary correction frozen manual source changed"
        );
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
    assert!(
        store
            .compaction_epoch("manual-correction")
            .unwrap()
            .unwrap()
            .summary
            .ends_with(expected)
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
}

/// Human pause blocks a newly queued correction claim; Stop cancels that exact
/// owner. Neither lifecycle action can publish rejected output or wake an
/// ordinary request, and the logical compaction marker is cleared after Stop.
#[test]
fn runtime_compaction_summary_correction_respects_human_pause_and_stop() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
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
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
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
    for _ in 0..8 {
        if !service.agent_is_compacting("%1") {
            break;
        }
        complete_runtime_test_compaction(&mut service, "%1", "bounded split summary");
    }
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.agent_provider_task_is_pending(&turn));
}

/// Invalid later chunks preserve earlier accepted summaries and pending input;
/// invalid synthesis repeats the exact private synthesis request rather than
/// re-staging its rejected text. A valid next response advances finite source.
#[test]
fn runtime_compaction_invalid_chunk_preserves_accepted_staging_and_synthesis() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
    let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
    let RuntimeAgentCompactionTarget::ActiveTurn {
        completed_summaries,
        pending_blocks,
        current_blocks,
        completed_responses,
        ..
    } = &mut task.target
    else {
        panic!("active turn");
    };
    completed_summaries.push("accepted earlier chunk".into());
    pending_blocks.push(current_blocks.clone());
    *completed_responses = 32;
    let conversation = task.conversation_id.clone();
    service.queue_agent_compaction_task(task);
    complete_runtime_test_compaction(&mut service, "%1", "");
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let RuntimeAgentCompactionTarget::ActiveTurn {
        completed_summaries,
        pending_blocks,
        completed_responses,
        ..
    } = &retry.target
    else {
        panic!("active turn");
    };
    assert_eq!(completed_summaries, &["accepted earlier chunk"]);
    assert_eq!(pending_blocks.len(), 1);
    assert_eq!(*completed_responses, 32);
    complete_runtime_test_compaction(&mut service, "%1", "accepted later chunk");
    let next = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(next.summary_correction.corrections_admitted, 0);
    assert!(
        !next
            .request
            .messages
            .iter()
            .any(|message| message.content.starts_with("[private summary correction]"))
    );
    complete_runtime_test_compaction(&mut service, "%1", "accepted last chunk");
    let synthesis = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    assert!(
        synthesis
            .request
            .messages
            .last()
            .unwrap()
            .content
            .contains("accepted earlier chunk")
    );
    complete_runtime_test_compaction(&mut service, "%1", "");
    let correction = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(
        correction.request.messages.last(),
        synthesis.request.messages.last()
    );
    assert!(store.compaction_epoch(&conversation).unwrap().is_none());
    assert!(!service.agent_provider_task_is_pending(&turn));
    complete_runtime_test_compaction(&mut service, "%1", "accepted final synthesis");
    assert!(service.agent_provider_task_is_pending(&turn));
}

/// A completed batch with executable content is malformed summary output, not
/// authorization to run actions. Its finite correction replaces only inference
/// and requires a final summary response before ordinary work can continue.
#[test]
fn runtime_compaction_malformed_completed_batch_is_private_and_bounded() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
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
        let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
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
            "x ".repeat(50_000)
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
        let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
        let mut task = service.take_pending_agent_compaction_task("%1").unwrap();
        let RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } = &task.target else {
            panic!("active turn");
        };
        let budget = plan.summary_budget_words();
        let conversation = task.conversation_id.clone();
        task.summary_correction.guidance_target = Some(1);
        task.summary_correction.source_generation = Some(task.task_generation);
        task.summary_correction.corrections_admitted = 3;
        task.compaction_request_shape = Some((
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            Default::default(),
            false,
        ));
        task.summary_correction.last_rejection = Some(
            mez_agent::context_compaction::ModelCompactionSummaryRejection {
                defect: mez_agent::context_compaction::ModelCompactionSummaryDefect::Budget,
                cost: budget + 2,
                bytes: 2 * (budget + 2),
                budget,
                estimated_tokens: false,
            },
        );
        service.queue_agent_compaction_task(task);
        let response = if valid {
            "valid bounded summary".to_string()
        } else {
            "x ".repeat(budget + 1)
        };
        complete_runtime_test_compaction(&mut service, "%1", &response);
        assert!(!service.agent_is_compacting("%1"));
        if valid {
            assert!(service.agent_provider_task_is_pending(&turn));
            assert!(store.compaction_epoch(&conversation).unwrap().is_some());
        } else {
            assert!(!service.agent_provider_task_is_pending(&turn));
            assert!(store.compaction_epoch(&conversation).unwrap().is_none());
            let failure = persisted_compaction_failure(&service, &store, &conversation, &turn);
            assert!(failure.contains("disposition=GuidanceFloor"));
            assert!(failure.contains("corrections_admitted=3"));
            assert!(failure.contains("openai_max_output_tokens_transmitted=Some(false)"));
            assert!(!failure.contains(&response));
        }
    }
}

/// A selected-source rewrite is an integrity failure even when output is blank.
/// Correctable output must not mask a genuinely changed frozen chronology or
/// queue another inference against corrupt evidence.
#[test]
fn runtime_compaction_invalid_summary_does_not_retry_mutated_source() {
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn)
        .unwrap()
        .replace_after_compaction(vec![ContextBlock::user_event("mutated", "new source")])
        .unwrap();
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
    let (mut service, store, turn) = queue_observed_input_compaction_with_exact_history();
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

/// Complete but over-budget, empty and byte-heavy outputs must not settle the
/// waiting turn. Each case retains the frozen source and logical epoch under a
/// fresh generation, publishes nothing, and resumes ordinary work once after a
/// valid model response. This covers rejection before final-request sizing.
#[test]
fn runtime_compaction_corrects_invalid_completed_summary_before_publication() {
    for kind in ["budget", "empty", "bytes"] {
        let (mut service, store, turn_id) = queue_observed_input_compaction_with_exact_history();
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
            "budget" => "word ".repeat(plan.summary_budget_words() + 1),
            "empty" => " \n\t".to_string(),
            _ => "界".repeat(128 * 1024 / 3 + 1),
        };
        complete_runtime_test_compaction(&mut service, "%1", &invalid);
        let retry = service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap_or_else(|| panic!("{kind}: invalid output must queue auxiliary correction"));
        assert_ne!(retry.task_generation, original.task_generation);
        assert_eq!(retry.compaction_epoch, original.compaction_epoch);
        assert_eq!(retry.resume_turn_id, original.resume_turn_id);
        assert!(!service.agent_provider_task_is_pending(&turn_id));
        assert!(
            store
                .compaction_epoch(&original.conversation_id)
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
        let epoch = store
            .compaction_epoch(&original.conversation_id)
            .unwrap()
            .unwrap();
        assert_eq!(epoch.ranges.len(), 1);
        assert_eq!(epoch.ranges[0].summary, "valid short summary");
        assert!(service.agent_provider_task_is_pending(&turn_id));
        assert!(!service.agent_is_compacting("%1"));
    }
}
