//! Unit tests for audit record redaction, writing, and retention behavior.

use super::{AuditActor, AuditConfig, AuditLog, AuditRecord, AuditRetentionPolicy};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use std::time::UNIX_EPOCH;

/// Verifies audit log writes jsonl with required fields and redaction.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn audit_log_writes_jsonl_with_required_fields_and_redaction() {
    let root = std::env::temp_dir().join(format!("mez-audit-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let path = root.join("audit.jsonl");
    let mut log = AuditLog::new(AuditConfig {
        enabled: true,
        path: path.clone(),
        hash_chain: false,
        required: true,
    });
    let mut record = AuditRecord::new(
        "$1",
        AuditActor {
            kind: "agent".to_string(),
            id: "a1".to_string(),
        },
        "shell_command",
        "run",
    )
    .with_metadata("command", "echo ok")
    .with_metadata("token", "Bearer secret");
    record.approval_state = "approved".to_string();
    record.outcome = "succeeded".to_string();

    let write = log.append(record).unwrap().unwrap();
    let data = fs::read_to_string(path).unwrap();

    assert_eq!(write.event_id, 1);
    assert!(data.contains(r#""version":1"#));
    assert!(data.contains(r#""event_id":1"#));
    assert!(data.contains(r#""session_id":"$1""#));
    assert!(data.contains(r#""redactions":["metadata"]"#));
    assert!(data.contains("[REDACTED]"));
    assert!(!data.contains("Bearer secret"));

    let _ = fs::remove_dir_all(root);
}

/// Verifies that audit deferral keeps event-id and hash-chain state in the
/// audit writer while moving only the encoded JSONL payload out to an async
/// persistence owner. This prevents the runtime actor from blocking on file I/O
/// without letting the persistence worker assign audit identity.
#[test]
fn audit_log_deferred_append_queues_encoded_jsonl_without_writing_file() {
    let root = std::env::temp_dir().join(format!("mez-audit-deferred-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let path = root.join("audit.jsonl");
    let mut log = AuditLog::new(AuditConfig {
        enabled: true,
        path: path.clone(),
        hash_chain: true,
        required: true,
    });
    log.set_defer_writes(true);

    let write = log
        .append(AuditRecord::new(
            "$1",
            AuditActor {
                kind: "agent".to_string(),
                id: "a1".to_string(),
            },
            "shell_command",
            "run",
        ))
        .unwrap()
        .unwrap();
    let deferred = log.drain_deferred_writes();

    assert_eq!(write.event_id, 1);
    assert!(write.hash.is_some());
    assert!(!path.exists());
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].path, path);
    let payload = String::from_utf8(deferred[0].bytes.clone()).unwrap();
    assert!(payload.contains(r#""event_id":1"#), "{payload}");
    assert!(payload.contains(r#""hash":"#), "{payload}");

    let _ = fs::remove_dir_all(root);
}

/// Verifies audit sanitization redacts secret like core fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn audit_sanitization_redacts_secret_like_core_fields() {
    let root = std::env::temp_dir().join(format!(
        "mez-audit-core-redaction-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let path = root.join("audit.jsonl");
    let mut log = AuditLog::new(AuditConfig {
        enabled: true,
        path: path.clone(),
        hash_chain: false,
        required: true,
    });
    let mut record = AuditRecord::new(
        "token=session-secret",
        AuditActor {
            kind: "agent".to_string(),
            id: "sk-actor-secret".to_string(),
        },
        "credential",
        "Authorization: bearer secret",
    )
    .with_window_id("Bearer window-secret")
    .with_metadata("api_key=secret", "password=secret");
    record.policy_mode = "refresh_token=secret".to_string();
    record.outcome = "access_token=secret".to_string();

    log.append(record).unwrap().unwrap();
    let data = fs::read_to_string(path).unwrap();

    assert!(data.contains(r#""session_id":"[REDACTED]""#));
    assert!(data.contains(r#""id":"[REDACTED]""#));
    assert!(data.contains(r#""action":"[REDACTED]""#));
    assert!(data.contains(r#""policy_mode":"[REDACTED]""#));
    assert!(data.contains(r#""outcome":"[REDACTED]""#));
    assert!(data.contains(r#""window_id":"[REDACTED]""#));
    assert!(data.contains(r#""redactions":["#));
    assert!(!data.contains("session-secret"));
    assert!(!data.contains("actor-secret"));
    assert!(!data.contains("window-secret"));
    assert!(!data.contains("password=secret"));

    let _ = fs::remove_dir_all(root);
}

/// Verifies required disabled audit log denies auditable actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn required_disabled_audit_log_denies_auditable_actions() {
    let mut log = AuditLog::new(AuditConfig {
        enabled: false,
        path: PathBuf::from("/tmp/unused"),
        hash_chain: false,
        required: true,
    });

    let error = log
        .append(AuditRecord::new(
            "$1",
            AuditActor {
                kind: "primary".to_string(),
                id: "c1".to_string(),
            },
            "permission_change",
            "bypass-approvals",
        ))
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies hash chain adds hash to each record.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn hash_chain_adds_hash_to_each_record() {
    let root = std::env::temp_dir().join(format!("mez-audit-hash-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let path = root.join("audit.jsonl");
    let mut log = AuditLog::new(AuditConfig {
        enabled: true,
        path: path.clone(),
        hash_chain: true,
        required: false,
    });
    let actor = AuditActor {
        kind: "primary".to_string(),
        id: "c1".to_string(),
    };

    let first = log
        .append(AuditRecord::new("$1", actor.clone(), "approval", "prompt"))
        .unwrap()
        .unwrap();
    let second = log
        .append(AuditRecord::new("$1", actor, "approval", "decide"))
        .unwrap()
        .unwrap();
    let data = fs::read_to_string(path).unwrap();

    assert!(first.hash.is_some());
    assert!(second.hash.is_some());
    assert_ne!(first.hash, second.hash);
    assert_eq!(data.lines().count(), 2);

    let _ = fs::remove_dir_all(root);
}

/// Verifies retention policy prunes jsonl by age and record count.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn retention_policy_prunes_jsonl_by_age_and_record_count() {
    let root =
        std::env::temp_dir().join(format!("mez-audit-retention-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("audit.jsonl");
    fs::write(
        &path,
        [
            audit_line(1, 100, "old"),
            audit_line(2, 250, "middle"),
            audit_line(3, 290, "new"),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let policy = AuditRetentionPolicy {
        max_age_days: Some(1),
        max_records: Some(1),
        max_bytes: None,
    };

    let report = policy
        .enforce_jsonl_at(&path, UNIX_EPOCH + Duration::from_secs(300))
        .unwrap();
    let data = fs::read_to_string(&path).unwrap();

    assert_eq!(report.original_records, 3);
    assert_eq!(report.retained_records, 1);
    assert_eq!(report.pruned_records, 2);
    assert!(data.contains(r#""event_id":3"#));
    assert!(!data.contains(r#""event_id":1"#));
    assert!(!data.contains(r#""event_id":2"#));

    let _ = fs::remove_dir_all(root);
}

/// Verifies that the async retention path uses the same pruning rules and file
/// rewrite behavior as the synchronous audit writer path.
#[tokio::test]
async fn async_retention_policy_prunes_jsonl_by_age_and_record_count() {
    let root = std::env::temp_dir().join(format!(
        "mez-audit-retention-async-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("audit.jsonl");
    fs::write(
        &path,
        [
            audit_line(1, 100, "old"),
            audit_line(2, 250, "middle"),
            audit_line(3, 290, "new"),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let policy = AuditRetentionPolicy {
        max_age_days: Some(1),
        max_records: Some(1),
        max_bytes: None,
    };

    let report = policy
        .enforce_jsonl_at_async(&path, UNIX_EPOCH + Duration::from_secs(300))
        .await
        .unwrap();
    let data = fs::read_to_string(&path).unwrap();

    assert_eq!(report.original_records, 3);
    assert_eq!(report.retained_records, 1);
    assert_eq!(report.pruned_records, 2);
    assert!(data.contains(r#""event_id":3"#));
    assert!(!data.contains(r#""event_id":1"#));
    assert!(!data.contains(r#""event_id":2"#));

    let _ = fs::remove_dir_all(root);
}

/// Verifies retention policy preserves surviving hash chain records.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn retention_policy_preserves_surviving_hash_chain_records() {
    let root = std::env::temp_dir().join(format!(
        "mez-audit-retention-hash-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let path = root.join("audit.jsonl");
    let mut log = AuditLog::new(AuditConfig {
        enabled: true,
        path: path.clone(),
        hash_chain: true,
        required: false,
    });
    let actor = actor();

    log.append(AuditRecord::new("$1", actor.clone(), "approval", "first"))
        .unwrap();
    log.append(AuditRecord::new("$1", actor.clone(), "approval", "second"))
        .unwrap();
    log.append(AuditRecord::new("$1", actor, "approval", "third"))
        .unwrap();
    let original_lines = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();

    AuditRetentionPolicy {
        max_age_days: None,
        max_records: Some(2),
        max_bytes: None,
    }
    .enforce_jsonl(&path)
    .unwrap();
    let retained_lines = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();

    assert_ne!(retained_lines, original_lines[1..]);
    assert!(retained_lines[0].contains(r#""event_id":2"#));
    assert!(retained_lines[1].contains(r#""event_id":3"#));
    assert!(
        retained_lines
            .iter()
            .all(|line| line.contains(r#""hash":"#))
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies audit helpers include required metadata and redact secrets.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn audit_helpers_include_required_metadata_and_redact_secrets() {
    let permission = AuditRecord::permission_decision(
        "$1",
        actor(),
        "perm1",
        "shell_command",
        "allow",
        "read-only",
        "allowed",
    );
    assert_event(&permission, "permission", "decision", "allowed");
    assert_metadata(&permission, "permission_id", "perm1");
    assert_metadata(&permission, "action_kind", "shell_command");
    assert_metadata(&permission, "decision", "allow");
    assert_eq!(permission.policy_mode, "read-only");
    assert_eq!(permission.approval_state, "allow");

    let approval = AuditRecord::approval_decision(
        "$1",
        actor(),
        "ap1",
        "agent:a1",
        "approve",
        "session",
        "approved",
    );
    assert_event(&approval, "approval", "decision", "approved");
    assert_metadata(&approval, "approval_id", "ap1");
    assert_metadata(&approval, "requester", "agent:a1");
    assert_metadata(&approval, "scope", "session");
    assert_eq!(approval.approval_state, "approve");

    let prompt = AuditRecord::approval_prompt(
        "$1",
        actor(),
        "ap2",
        "agent:a1",
        "shell_command",
        "read=[.];write=[]",
        "prompted",
    );
    assert_event(&prompt, "approval", "prompt", "prompted");
    assert_metadata(&prompt, "approval_id", "ap2");
    assert_metadata(&prompt, "requester", "agent:a1");
    assert_metadata(&prompt, "action_kind", "shell_command");
    assert_eq!(prompt.approval_state, "pending");

    let observer = AuditRecord::observer_decision(
        "$1",
        actor(),
        "observer_request",
        "o1",
        "approved",
        "succeeded",
    );
    assert_event(&observer, "observer", "decision", "succeeded");
    assert_metadata(&observer, "target_kind", "observer_request");
    assert_metadata(&observer, "observer_request_id", "o1");
    assert_metadata(&observer, "decision", "approved");
    assert_eq!(observer.approval_state, "approved");

    let auth = AuditRecord::auth_change(
        "$1",
        actor(),
        "openai",
        "Bearer account-secret",
        "login",
        "succeeded",
    );
    assert_event(&auth, "auth", "login", "succeeded");
    assert_metadata(&auth, "provider", "openai");
    assert_metadata(&auth, "account_id", "[REDACTED]");
    assert!(auth.redactions.contains(&"metadata".to_string()));

    let logout = AuditRecord::logout("$1", actor(), "openai", "acct1", "succeeded");
    assert_event(&logout, "auth", "logout", "succeeded");
    assert_metadata(&logout, "provider", "openai");
    assert_metadata(&logout, "account_id", "acct1");

    let mcp = AuditRecord::mcp_call(
        "$1",
        actor(),
        "fs",
        "read_file",
        "call1",
        r#"{"token":"token=secret"}"#,
        "succeeded",
    );
    assert_event(&mcp, "external_integration", "mcp_call", "succeeded");
    assert_metadata(&mcp, "server_id", "fs");
    assert_metadata(&mcp, "tool_name", "read_file");
    assert_metadata(&mcp, "call_id", "call1");
    assert_metadata(&mcp, "arguments_json", "[REDACTED]");
    assert!(mcp.redactions.contains(&"metadata".to_string()));

    let provider =
        AuditRecord::provider_request("$1", actor(), "openai", "gpt-test", "turn-1", "succeeded");
    assert_event(
        &provider,
        "external_integration",
        "provider_request",
        "succeeded",
    );
    assert_metadata(&provider, "provider", "openai");
    assert_metadata(&provider, "model", "gpt-test");
    assert_metadata(&provider, "turn_id", "turn-1");

    let bridge = AuditRecord::local_protocol_bridge_change(
        "$1",
        actor(),
        "mmp/1",
        "agent-1",
        "presence",
        "applied",
    );
    assert_event(&bridge, "local_protocol_bridge", "presence", "applied");
    assert_metadata(&bridge, "protocol", "mmp/1");
    assert_metadata(&bridge, "bridge_id", "agent-1");
    assert_metadata(&bridge, "change", "presence");

    let config =
        AuditRecord::config_change("$1", actor(), "project", "audit.path", "set", "applied");
    assert_event(&config, "configuration", "set", "applied");
    assert_metadata(&config, "scope", "project");
    assert_metadata(&config, "key", "audit.path");
    assert_metadata(&config, "operation", "set");

    let snapshot = AuditRecord::snapshot_operation("$1", actor(), "snap-1", "create", "applied");
    assert_event(&snapshot, "snapshot", "create", "applied");
    assert_metadata(&snapshot, "snapshot_id", "snap-1");
    assert_metadata(&snapshot, "operation", "create");

    let subagent = AuditRecord::subagent_spawn(
        "$1",
        actor(),
        "agent-parent",
        "agent-child",
        "worker",
        "owned-write",
        "accepted",
    );
    assert_event(&subagent, "subagent", "spawn", "accepted");
    assert_eq!(subagent.agent_id.as_deref(), Some("agent-child"));
    assert_metadata(&subagent, "parent_agent_id", "agent-parent");
    assert_metadata(&subagent, "subagent_id", "agent-child");
    assert_metadata(&subagent, "role", "worker");
    assert_metadata(&subagent, "cooperation_mode", "owned-write");

    let credential = AuditRecord::credential_access_attempt(
        "$1",
        actor(),
        "openai",
        "sk-secret-value",
        "provider_request",
        "denied",
    );
    assert_event(&credential, "credential", "access_attempt", "denied");
    assert_metadata(&credential, "provider", "openai");
    assert_metadata(&credential, "credential_id", "[REDACTED]");
    assert_metadata(&credential, "purpose", "provider_request");
    assert!(credential.redactions.contains(&"metadata".to_string()));
}

/// Runs the actor operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn actor() -> AuditActor {
    AuditActor {
        kind: "agent".to_string(),
        id: "a1".to_string(),
    }
}

/// Runs the audit line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn audit_line(event_id: u64, timestamp: u64, label: &str) -> String {
    format!(
        r#"{{"version":1,"event_id":{event_id},"timestamp":"unix:{timestamp}","session_id":"$1","window_id":null,"pane_id":null,"agent_id":null,"actor":{{"kind":"agent","id":"a1"}},"event_type":"test","action":"{label}","policy_mode":"default","approval_state":"not_required","outcome":"succeeded","redactions":[],"metadata":{{}}}}"#
    )
}

/// Runs the assert event operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn assert_event(record: &AuditRecord, event_type: &str, action: &str, outcome: &str) {
    assert_eq!(record.event_type, event_type);
    assert_eq!(record.action, action);
    assert_eq!(record.outcome, outcome);
}

/// Runs the assert metadata operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn assert_metadata(record: &AuditRecord, key: &str, value: &str) {
    assert_eq!(record.metadata.get(key).map(String::as_str), Some(value));
}
