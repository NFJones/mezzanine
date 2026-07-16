//! Runtime tests for actions memory behavior.

use super::*;
use mez_agent::memory::MemorySearchRequest;

/// Verifies runtime service owns session memory and clears it on kill.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_owns_session_memory_and_clears_it_on_kill() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    service
        .upsert_session_memory(MemoryRecord::new_with_defaults(
            "runtime-note",
            mez_agent::memory::MemoryScope::Session {
                session_id: service.session().id.to_string(),
            },
            120,
            120,
            mez_agent::memory::MemorySource::User,
            20,
            "prefer focused regression tests",
        ))
        .unwrap();

    assert_eq!(service.memory_records().len(), 1);
    assert_eq!(
        service
            .session_memory()
            .inspect("runtime-note")
            .unwrap()
            .content,
        "prefer focused regression tests"
    );

    service.kill_session(&primary, true).unwrap();

    assert!(service.memory_records().is_empty());
}

/// Verifies `/compact` opportunistically prunes expired persistent memories
/// before queueing model-backed compaction while preserving non-expired
/// persistent records in both disk and session state.
#[test]
fn runtime_agent_shell_compact_prunes_expired_persistent_memory_before_queueing() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-prune-memory".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[memory]
enabled = true
[agents]
default_provider = "openai"
default_model_profile = "compact-prune-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-prune-test"]
default_model = "gpt-compact-prune-test"
[model_profiles.compact-prune-test]
provider = "openai"
model = "gpt-compact-prune-test"
context_window_tokens = 4500
"#
            .to_string(),
        }])
        .unwrap();
    let config_root = temp_root("runtime-agent-compact-prune-memory");
    service.set_config_root(config_root.clone());
    let store = crate::memory::PersistentMemoryStore::under_config_root(&config_root);
    let mut expired = mez_agent::memory::MemoryRecord::new_with_defaults(
        "expired-compact-memory".to_string(),
        mez_agent::memory::MemoryScope::Global,
        1,
        1,
        mez_agent::memory::MemorySource::User,
        100,
        "expired compact memory".to_string(),
    );
    expired.expires_at_unix_seconds = Some(2);
    let live = mez_agent::memory::MemoryRecord::new_with_defaults(
        "live-compact-memory".to_string(),
        mez_agent::memory::MemoryScope::Global,
        1,
        1,
        mez_agent::memory::MemorySource::User,
        100,
        "live compact memory".to_string(),
    );
    service.upsert_session_memory(expired.clone()).unwrap();
    service.upsert_session_memory(live.clone()).unwrap();
    store.upsert(expired).unwrap();
    store.upsert(live).unwrap();

    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-prune"));
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "compact-prune".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "compact this transcript".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 80).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "compact-prune", 1)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-prune","method":"agent/shell/command","params":{"idempotency_key":"compact-prune","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains("state=queued"), "{compact}");
    let persisted_ids = store
        .list()
        .unwrap()
        .into_iter()
        .map(|record| record.id)
        .collect::<Vec<_>>();
    assert!(
        !persisted_ids
            .iter()
            .any(|id| id == "expired-compact-memory"),
        "{persisted_ids:?}"
    );
    assert!(
        persisted_ids.iter().any(|id| id == "live-compact-memory"),
        "{persisted_ids:?}"
    );
    let session_ids = service
        .memory_records()
        .into_iter()
        .map(|record| record.id)
        .collect::<Vec<_>>();
    assert!(
        !session_ids.iter().any(|id| id == "expired-compact-memory"),
        "{session_ids:?}"
    );
    assert!(
        session_ids.iter().any(|id| id == "live-compact-memory"),
        "{session_ids:?}"
    );
}

/// Verifies `/remember` opportunistically prunes expired persistent memories
/// before queueing model-backed memory generation while preserving non-expired
/// persistent records in both disk and session state.
#[test]
fn runtime_agent_shell_remember_prunes_expired_persistent_memory_before_queueing() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "remember-prune-memory".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[memory]
enabled = true
[agents]
default_provider = "openai"
default_model_profile = "remember-prune-test"
[providers.openai]
kind = "openai"
models = ["gpt-remember-prune-test"]
default_model = "gpt-remember-prune-test"
[model_profiles.remember-prune-test]
provider = "openai"
model = "gpt-remember-prune-test"
context_window_tokens = 4500
"#
            .to_string(),
        }])
        .unwrap();
    let config_root = temp_root("runtime-agent-remember-prune-memory");
    service.set_config_root(config_root.clone());
    let store = crate::memory::PersistentMemoryStore::under_config_root(&config_root);
    let mut expired = mez_agent::memory::MemoryRecord::new_with_defaults(
        "expired-remember-memory".to_string(),
        mez_agent::memory::MemoryScope::Global,
        1,
        1,
        mez_agent::memory::MemorySource::User,
        100,
        "expired remember memory".to_string(),
    );
    expired.expires_at_unix_seconds = Some(2);
    let live = mez_agent::memory::MemoryRecord::new_with_defaults(
        "live-remember-memory".to_string(),
        mez_agent::memory::MemoryScope::Global,
        1,
        1,
        mez_agent::memory::MemorySource::User,
        100,
        "live remember memory".to_string(),
    );
    service.upsert_session_memory(expired.clone()).unwrap();
    service.upsert_session_memory(live.clone()).unwrap();
    store.upsert(expired).unwrap();
    store.upsert(live).unwrap();

    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 80).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let remember = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"remember-prune","method":"agent/shell/command","params":{"idempotency_key":"remember-prune","input":"/remember Keep the release checklist current."}}"#,
        &primary,
    );

    assert!(remember.contains("state=queued"), "{remember}");
    let persisted_ids = store
        .list()
        .unwrap()
        .into_iter()
        .map(|record| record.id)
        .collect::<Vec<_>>();
    assert!(
        !persisted_ids
            .iter()
            .any(|id| id == "expired-remember-memory"),
        "{persisted_ids:?}"
    );
    assert!(
        persisted_ids.iter().any(|id| id == "live-remember-memory"),
        "{persisted_ids:?}"
    );
    let session_ids = service
        .memory_records()
        .into_iter()
        .map(|record| record.id)
        .collect::<Vec<_>>();
    assert!(
        !session_ids.iter().any(|id| id == "expired-remember-memory"),
        "{session_ids:?}"
    );
    assert!(
        session_ids.iter().any(|id| id == "live-remember-memory"),
        "{session_ids:?}"
    );
}

/// Verifies that `/compact` converts the active conversation transcript into a
/// bounded pane-scoped memory record, retains a raw recent transcript tail, and
/// feeds both into the next prompt context. This keeps context pressure
/// handling from silently dropping recent exact referents.
#[test]
fn runtime_agent_shell_compact_summarizes_transcript_into_memory_context() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-test"]
default_model = "gpt-compact-test"
[model_profiles.compact-test]
provider = "openai"
model = "gpt-compact-test"
context_window_tokens = 4500
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact"));
    for sequence in 1..=12 {
        let (role, content) = match sequence {
            1 => (
                mez_agent::transcript::TranscriptRole::User,
                format!("summarize release plan {}", "summary-word ".repeat(28)),
            ),
            2 => (
                mez_agent::transcript::TranscriptRole::Tool,
                format!(
                    "api_key sk-secret should be hidden {}",
                    "secret-word ".repeat(28)
                ),
            ),
            3 => (
                mez_agent::transcript::TranscriptRole::Assistant,
                format!(
                    "release plan summary is ready {}",
                    "release-word ".repeat(28)
                ),
            ),
            _ if sequence % 2 == 0 => (
                mez_agent::transcript::TranscriptRole::User,
                format!("filler user turn {sequence} {}", "user-word ".repeat(28)),
            ),
            _ => (
                mez_agent::transcript::TranscriptRole::Assistant,
                format!(
                    "filler assistant turn {sequence} {}",
                    "assistant-word ".repeat(28)
                ),
            ),
        };
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "as1".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 80).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as1", 12)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact","method":"agent/shell/command","params":{"idempotency_key":"compact","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains(r#""kind":"mutated""#), "{compact}");
    assert!(compact.contains(r#""command":"compact""#), "{compact}");
    assert!(compact.contains("state=queued"), "{compact}");
    assert!(
        compact.contains("previous_transcript_entries=12"),
        "{compact}"
    );
    assert!(compact.contains("summarized_entries=6"), "{compact}");
    assert!(compact.contains("source=model-compact"), "{compact}");
    assert!(!compact.contains("requires_runtime"), "{compact}");
    assert!(service.agent_compacting_panes.contains_key("%1"));
    assert!(service.pending_agent_compaction_tasks.contains_key("%1"));

    complete_runtime_test_compaction(&mut service, "%1", "summarize release plan\n[redacted]");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: compacting conversation summary"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent: compacted conversation summary"),
        "{pane_text}"
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        6
    );
    let compacted = service
        .memory_records()
        .into_iter()
        .find(|record| record.id == "compact-as1")
        .expect("compacted memory record");
    assert!(
        compacted.content.contains("summarize release plan"),
        "{}",
        compacted.content
    );
    assert!(
        compacted.content.contains("[redacted]"),
        "{}",
        compacted.content
    );
    assert!(
        !compacted.content.contains("sk-secret"),
        "{}",
        compacted.content
    );

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-prompt","method":"agent/shell/command","params":{"idempotency_key":"compact-prompt","input":"continue after compaction"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::Memory
            && block.label.contains("compact-as1")
            && block.content.contains("summarize release plan")
    }));
    assert!(context.blocks.iter().all(|block| {
        !matches!(
            block.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
        ) || !block.content.contains("release plan summary is ready")
    }));
    assert!(context.blocks.iter().all(|block| {
        !matches!(
            block.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
        ) || !block.content.contains("sk-secret")
    }));
    assert!(context.blocks.iter().all(|block| {
        !matches!(
            block.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
        ) || !block.content.contains("summarize release plan")
    }));
}

/// Verifies provider context receives only the active conversation compaction
/// memory automatically.
///
/// Generic session memory should not be injected into every provider request
/// once transcript replay and compaction summaries already represent the active
/// conversation.
#[test]
fn runtime_agent_context_injects_only_active_compact_memory() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as1", 0)
        .unwrap();
    service
        .upsert_session_memory(MemoryRecord::new_with_defaults(
            "runtime-note",
            mez_agent::memory::MemoryScope::Session {
                session_id: service.session().id.to_string(),
            },
            1,
            1,
            mez_agent::memory::MemorySource::User,
            255,
            "generic memory should not be automatic context",
        ))
        .unwrap();
    service
        .upsert_session_memory(MemoryRecord::new_with_defaults(
            "compact-other",
            mez_agent::memory::MemoryScope::Pane {
                session_id: service.session().id.to_string(),
                pane_id: "%1".to_string(),
            },
            2,
            2,
            mez_agent::memory::MemorySource::Agent,
            255,
            "other compaction should not leak",
        ))
        .unwrap();
    service
        .upsert_session_memory(MemoryRecord::new_with_defaults(
            "compact-as1",
            mez_agent::memory::MemoryScope::Pane {
                session_id: service.session().id.to_string(),
                pane_id: "%1".to_string(),
            },
            3,
            3,
            mez_agent::memory::MemorySource::Agent,
            128,
            "active compact summary",
        ))
        .unwrap();

    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    let memory_blocks = context
        .blocks
        .iter()
        .filter(|block| block.source == ContextSourceKind::Memory)
        .collect::<Vec<_>>();

    assert_eq!(memory_blocks.len(), 2, "{memory_blocks:?}");
    assert!(
        memory_blocks
            .iter()
            .any(|block| block.label == "conversation compaction notice"
                && block.content.contains("Conversation compaction occurred")),
        "{memory_blocks:?}"
    );
    assert!(
        memory_blocks
            .iter()
            .any(|block| block.label.contains("compact-as1")
                && block.content.contains("active compact summary")),
        "{memory_blocks:?}"
    );
    assert!(
        context
            .blocks
            .iter()
            .all(|block| !block.content.contains("generic memory"))
    );
    assert!(
        context
            .blocks
            .iter()
            .all(|block| !block.content.contains("other compaction"))
    );
}

/// Verifies that `/compact` is explicit when there is no transcript content to
/// compact and that the empty path does not create a misleading memory record.
#[test]
fn runtime_agent_shell_compact_reports_empty_transcript_without_memory() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-empty","method":"agent/shell/command","params":{"idempotency_key":"compact-empty","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains(r#""kind":"display""#), "{compact}");
    assert!(compact.contains(r#""command":"compact""#), "{compact}");
    assert!(
        compact.contains("compacted=false reason=no-transcript-entries"),
        "{compact}"
    );
    assert!(compact.contains("source=model-compact"), "{compact}");
    assert!(service.memory_records().is_empty());
}

/// Verifies runtime memory actions append audit records that name the action
/// and preserve compact argument metadata without storing raw freeform text.
///
/// This regression keeps memory search and store behavior aligned with other
/// runtime-owned action families so operators can diagnose what executed from
/// the audit log alone.
#[test]
fn runtime_executes_memory_actions_and_audits_action_arguments() {
    let mut service = test_runtime_service();
    let audit_root = temp_root("runtime-memory-audit");
    let audit_path = audit_root.join("audit.jsonl");
    let config_root = temp_root("runtime-memory-action-config");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    service.set_config_root(config_root.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let store = crate::memory::PersistentMemoryStore::under_config_root(&config_root);
    store
        .upsert(mez_agent::memory::MemoryRecord::new_with_defaults(
            "seed-memory".to_string(),
            mez_agent::memory::MemoryScope::Project {
                root: crate::project::discover_project_root(&std::env::current_dir().unwrap())
                    .to_string_lossy()
                    .into_owned(),
            },
            1,
            1,
            mez_agent::memory::MemorySource::User,
            50,
            "prompt cache details".to_string(),
        ))
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-memory-turn","input":"use memory"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let search = mez_agent::AgentAction {
        id: "mem-search".to_string(),
        rationale: "search memory".to_string(),
        payload: mez_agent::AgentActionPayload::MemorySearch {
            query: "prompt cache".to_string(),
            limit: Some(3),
        },
    };
    let store_action = mez_agent::AgentAction {
        id: "mem-store".to_string(),
        rationale: "store memory".to_string(),
        payload: mez_agent::AgentActionPayload::MemoryStore {
            kind: "research".to_string(),
            priority: Some(80),
            scope: Some("project".to_string()),
            keywords: vec!["prompt".to_string(), "research".to_string()],
            content: "remember prompt cache research findings".to_string(),
            expires_in_days: Some(7),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "using memory".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![search.clone(), store_action.clone()],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![
            mez_agent::ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                action_id: search.id.clone(),
                action_type: "memory_search",
                status: ActionStatus::Running,
                content: Vec::new(),
                structured_content_json: None,
                is_error: false,
                error: None,
            },
            mez_agent::ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                action_id: store_action.id.clone(),
                action_type: "memory_store",
                status: ActionStatus::Running,
                content: Vec::new(),
                structured_content_json: None,
                is_error: false,
                error: None,
            },
        ],
        final_turn: true,
        terminal_state: AgentTurnState::Running,
    };

    service
        .execute_running_memory_actions_for_turn(&turn, &mut execution)
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert_eq!(execution.action_results[1].status, ActionStatus::Succeeded);

    let records = fs::read_to_string(&audit_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|record| record["action"] == "runtime_memory_action")
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2, "{records:?}");

    let search_record = records
        .iter()
        .find(|record| record["metadata"]["action_id"] == "mem-search")
        .unwrap();
    assert_eq!(search_record["metadata"]["action_type"], "memory_search");
    assert_eq!(search_record["metadata"]["limit"], "3");
    assert_eq!(search_record["metadata"]["query_bytes"], "12");
    assert!(search_record["metadata"].get("query_sha256").is_some());

    let store_record = records
        .iter()
        .find(|record| record["metadata"]["action_id"] == "mem-store")
        .unwrap();
    assert_eq!(store_record["metadata"]["action_type"], "memory_store");
    assert_eq!(store_record["metadata"]["kind"], "research");
    assert_eq!(store_record["metadata"]["priority"], "80");
    assert_eq!(store_record["metadata"]["scope"], "project");
    assert_eq!(store_record["metadata"]["keyword_count"], "2");
    assert_eq!(store_record["metadata"]["content_bytes"], "39");
    assert_eq!(store_record["metadata"]["expires_in_days"], "7");
    assert!(store_record["metadata"].get("content_sha256").is_some());
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies model-authored memory stores reject episodic and scratch kinds at
/// runtime even if a malformed provider output bypasses the provider schema.
///
/// This regression keeps the runtime fail-closed with the model-facing schema:
/// transient transcript summaries and scratch notes must not be persisted as
/// durable memory records from ordinary `memory_store` actions.
#[test]
fn runtime_memory_store_rejects_episode_and_scratch_kinds() {
    let mut service = test_runtime_service();
    let config_root = temp_root("runtime-memory-kind-config");
    service.set_config_root(config_root.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-memory-kind-turn","input":"store memory"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let actions = ["episode", "scratch"]
        .into_iter()
        .map(|kind| mez_agent::AgentAction {
            id: format!("mem-{kind}"),
            rationale: "store transient memory".to_string(),
            payload: mez_agent::AgentActionPayload::MemoryStore {
                kind: kind.to_string(),
                priority: Some(50),
                scope: Some("project".to_string()),
                keywords: Vec::new(),
                content: "temporary turn note".to_string(),
                expires_in_days: None,
            },
        })
        .collect::<Vec<_>>();
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "using memory".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: actions.clone(),
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: actions
            .iter()
            .map(|action| mez_agent::ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                action_id: action.id.clone(),
                action_type: "memory_store",
                status: ActionStatus::Running,
                content: Vec::new(),
                structured_content_json: None,
                is_error: false,
                error: None,
            })
            .collect(),
        final_turn: true,
        terminal_state: AgentTurnState::Running,
    };

    service
        .execute_running_memory_actions_for_turn(&turn, &mut execution)
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    for result in &execution.action_results {
        assert_eq!(result.status, ActionStatus::Failed);
        let error = result.error.as_ref().expect("memory store should fail");
        assert!(
            error
                .message
                .contains("memory_store kind must be preference, fact, procedure, documentation, research, or warning"),
            "{result:?}"
        );
    }
    let store = crate::memory::PersistentMemoryStore::under_config_root(config_root.clone());
    let records = store
        .search(&MemorySearchRequest {
            query: Some("temporary".to_string()),
            scope: None,
            kind: None,
            state: None,
            source: None,
            limit: 10,
        })
        .unwrap();
    assert!(records.is_empty(), "{records:?}");
    let _ = fs::remove_dir_all(config_root);
}

/// Verifies failed runtime memory actions tell the model to continue without
/// retrying memory.
///
/// This regression keeps runtime-fed memory rejection text aligned with the
/// prompt guidance so a failed memory action nudges the model toward direct
/// evidence instead of looping on memory again.
#[test]
fn runtime_memory_disabled_failure_tells_model_to_continue_without_retrying_memory() {
    let mut service = test_runtime_service();
    let config_root = temp_root("runtime-memory-disabled-config");
    service.set_config_root(config_root.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[memory]\nenabled = false\n".to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-memory-disabled-turn","input":"use memory"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "mem-search".to_string(),
        rationale: "search memory".to_string(),
        payload: mez_agent::AgentActionPayload::MemorySearch {
            query: "prompt cache".to_string(),
            limit: Some(3),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "using memory".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![action.clone()],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult {
            protocol: "maap/1".to_string(),
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            action_id: action.id.clone(),
            action_type: "memory_search",
            status: ActionStatus::Running,
            content: Vec::new(),
            structured_content_json: None,
            is_error: false,
            error: None,
        }],
        final_turn: true,
        terminal_state: AgentTurnState::Running,
    };

    service
        .execute_running_memory_actions_for_turn(&turn, &mut execution)
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert_eq!(execution.action_results[0].status, ActionStatus::Failed);
    let error = execution.action_results[0]
        .error
        .as_ref()
        .expect("memory search should fail when memory is disabled");
    assert_eq!(error.code, "memory_disabled");
    assert!(
        error
            .message
            .contains("continue with current action results, MCP, shell, web, or a bounded report instead of retrying memory actions"),
        "{error:?}"
    );
    let _ = fs::remove_dir_all(config_root);
}
