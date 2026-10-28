//! Resume and daemon restore preserve independent archive and replay positions.
//!
//! These acceptance tests use real saved archives and command adoption rather
//! than normalizing counters in a fixture. Repeated boundaries must derive raw
//! counts from physical rows and authoritative prefix coverage, independently of
//! selective model projection and any new diagnostic rows written on restart.

use super::fork_fixtures::{CONVERSATION, resumed_source};
use super::*;
use crate::storage::transcript::AgentCompactionRange;

/// Checks both domains against current durable evidence, then verifies that
/// context projection and an actual fork-source read agree with those domains.
fn assert_position(service: &mut RuntimeSessionService, store: &AgentTranscriptStore, prefix: u64) {
    let high = store.inspect(CONVERSATION).unwrap().len() as u64;
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, CONVERSATION);
    assert_eq!(session.transcript_high_water, high);
    assert_eq!(session.transcript_entries, high - prefix);
    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content == "FROZEN_PREFIX")
            .count(),
        1
    );
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content == "raw source 1")
    );
    let snapshot = service
        .prepare_subagent_fork_read_work("agent-%1")
        .unwrap()
        .unwrap()
        .execute()
        .unwrap();
    assert_eq!(snapshot.source_high_water, high);
    assert_eq!(snapshot.entries.len() as u64, high - prefix);
    assert_eq!(
        snapshot.compaction_epoch,
        store.compaction_epoch(CONVERSATION).unwrap()
    );
}

/// Repeated real resume and daemon checkpoint/restore retain the same position
/// semantics for a selective suffix and a fully compacted archive. Selective
/// summaries reduce model content without reducing physical replay cardinality;
/// restart diagnostics may extend both domains but cannot resurrect the prefix.
#[test]
fn runtime_conversation_positions_survive_repeated_resume_and_daemon_restore() {
    for prefix in [1, 5] {
        let (mut service, store, primary) = resumed_source(5, prefix);
        if prefix == 1 {
            store
                .save_compaction_ranges(
                    CONVERSATION,
                    prefix,
                    "FROZEN_PREFIX",
                    vec![AgentCompactionRange {
                        first_sequence: 2,
                        through_sequence: 3,
                        summary: "SELECTIVE_POSITION".into(),
                    }],
                )
                .unwrap();
        }
        for _ in 0..2 {
            assert_position(&mut service, &store, prefix);
            service
                .execute_agent_shell_command(&primary, "/new")
                .unwrap();
            let response = service
                .execute_agent_shell_command(&primary, &format!("/resume {CONVERSATION}"))
                .unwrap();
            let settled = service
                .run_pending_deferred_agent_command_for_tests()
                .unwrap()
                .unwrap_or(response);
            assert!(settled.contains("resumed=true"), "{settled}");
        }
        assert_position(&mut service, &store, prefix);
        service.checkpoint_agent_session_metadata().unwrap();
        let mut restarted = test_runtime_service();
        restarted.session.id = service.session().id.clone();
        restarted.set_agent_transcript_store(AgentTranscriptStore::new(store.root().to_path_buf()));
        assert_eq!(
            restarted
                .restore_agent_sessions_from_transcript_store()
                .unwrap(),
            1
        );
        assert_position(&mut restarted, &store, prefix);
        let context = restarted
            .agent_context_for_pane_prompt("%1", "continue", 0)
            .unwrap();
        if prefix == 1 {
            assert_eq!(
                context
                    .blocks()
                    .iter()
                    .filter(|block| block.content == "SELECTIVE_POSITION")
                    .count(),
                1
            );
            assert!(
                !context
                    .blocks()
                    .iter()
                    .any(|block| block.content.contains("raw source 2")
                        || block.content.contains("raw source 3"))
            );
        }
    }
}

/// A corrupt target fails through real resume preparation without replacing the
/// current compacted conversation or either of its counters. Its valid context
/// and source remain usable after the failed command; no provider is involved.
#[test]
fn runtime_conversation_positions_survive_failed_resume_to_corrupt_target() {
    let (mut service, store, primary) = resumed_source(5, 1);
    let before = service.agent_shell_store().get("%1").unwrap().clone();
    let target = "018f6b3a-1b2c-7000-9000-cafebabe0022";
    let mut entry = super::fork_fixtures::row(1);
    entry.conversation_id = target.into();
    store.append(&entry).unwrap();
    let path = store.transcript_path(target).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{corrupt target}\n");
    std::fs::write(path, bytes).unwrap();
    let result = service
        .execute_agent_shell_command(&primary, &format!("/resume {target}"))
        .and_then(|response| {
            service
                .run_pending_deferred_agent_command_for_tests()
                .map(|outcome| outcome.unwrap_or(response))
        });
    let response: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
    assert_eq!(response["kind"], "display");
    assert!(
        response["body"]
            .as_str()
            .unwrap()
            .contains("agent command error: invalid transcript entry"),
        "corrupt target must report its actual preparation failure: {response}"
    );
    assert!(response["turn"].is_null());
    let after = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(after.session_id, before.session_id);
    assert_eq!(after.transcript_entries, before.transcript_entries);
    assert_eq!(after.transcript_high_water, before.transcript_high_water);
    assert_position(&mut service, &store, 1);
}
