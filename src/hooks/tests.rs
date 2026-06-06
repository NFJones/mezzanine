//! Unit tests for hook planning, execution, queueing, and audit emission.

use super::{
    FocusedShellExecutor, FocusedShellHookDispatchStatus, FocusedShellHookOutput,
    FocusedShellHookQueue, HookDefinition, HookEvent, HookExecutionPlan, HookExecutionStatus,
    HookFailure, HookFailureDecision, HookFailureKind, HookInvocation, HookMatcherGroup,
    HookMatcherOperator, HookMatcherPredicate, HookOnFailure, decide_hook_failure,
    execute_focused_shell_hook, execute_focused_shell_hook_with_audit, execute_program_hook,
    execute_program_hook_async, execute_program_hook_with_audit, plan_event, plan_hook,
    plan_hook_with_payload,
};
use crate::audit::{AuditActor, AuditConfig, AuditLog};
use crate::error::Result;
use std::fs;

/// Carries Fake Focused Shell state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Default)]
struct FakeFocusedShell {
    /// Stores the seen command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    seen_command: Option<String>,
    /// Stores the seen payload value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    seen_payload: Option<String>,
    /// Stores the output value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    output: Option<FocusedShellHookOutput>,
}

impl FocusedShellExecutor for FakeFocusedShell {
    /// Runs the run hook command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn run_hook_command(&mut self, plan: &HookExecutionPlan) -> Result<FocusedShellHookOutput> {
        self.seen_command = plan.shell_command.clone();
        self.seen_payload = Some(plan.event_payload_json.clone());
        Ok(self.output.clone().unwrap_or(FocusedShellHookOutput {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            shell_unavailable: false,
            policy_denied: false,
        }))
    }
}

/// Verifies program hook plans direct invocation.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn program_hook_plans_direct_invocation() {
    let hook = HookDefinition {
        id: "audit".to_string(),
        event: HookEvent::SessionDetach,
        invocation: HookInvocation::Program {
            command: "logger".to_string(),
            args: vec!["mez detached".to_string()],
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: None,
        on_failure: None,
    };

    let plan = plan_hook(&hook).unwrap().unwrap();

    assert!(!plan.run_in_focused_shell);
    assert_eq!(plan.program.as_deref(), Some("logger"));
    assert_eq!(plan.args, vec!["mez detached"]);
    assert_eq!(plan.timeout_ms, 30_000);
    assert_eq!(plan.on_failure, HookOnFailure::Warn);
}

/// Verifies program hook execution receives payload on stdin.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn program_hook_execution_receives_payload_on_stdin() {
    let hook = HookDefinition {
        id: "echo".to_string(),
        event: HookEvent::SessionDetach,
        invocation: HookInvocation::Program {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "cat".to_string()],
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: Some(1000),
        on_failure: None,
    };
    let plan = plan_hook_with_payload(&hook, r#"{"event":"detach"}"#)
        .unwrap()
        .unwrap();

    let result = execute_program_hook(&plan).unwrap();

    assert_eq!(result.status, HookExecutionStatus::Succeeded);
    assert_eq!(result.stdout, r#"{"event":"detach"}"#);
    assert!(result.failure.is_none());
}

/// Verifies that the Tokio hook executor preserves the synchronous program
/// hook contract while avoiding the blocking child wait loop. The async
/// runtime side-effect worker uses this path, so stdin payload delivery and
/// stdout collection must match the compatibility executor exactly.
#[tokio::test(flavor = "current_thread")]
async fn async_program_hook_execution_receives_payload_on_stdin() {
    let hook = HookDefinition {
        id: "echo-async".to_string(),
        event: HookEvent::SessionDetach,
        invocation: HookInvocation::Program {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "cat".to_string()],
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: Some(1000),
        on_failure: None,
    };
    let plan = plan_hook_with_payload(&hook, r#"{"event":"detach"}"#)
        .unwrap()
        .unwrap();

    let result = execute_program_hook_async(&plan).await.unwrap();

    assert_eq!(result.status, HookExecutionStatus::Succeeded);
    assert_eq!(result.stdout, r#"{"event":"detach"}"#);
    assert!(result.failure.is_none());
}

/// Verifies that the Tokio hook executor applies hook timeouts through Tokio
/// time rather than a blocking polling loop. This keeps the async hook
/// side-effect worker responsive when a child process stalls.
#[tokio::test(flavor = "current_thread")]
async fn async_program_hook_execution_enforces_timeout() {
    let hook = HookDefinition {
        id: "slow-async".to_string(),
        event: HookEvent::SessionDetach,
        invocation: HookInvocation::Program {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 1".to_string()],
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: Some(1),
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();

    let result = execute_program_hook_async(&plan).await.unwrap();

    assert_eq!(result.status, HookExecutionStatus::TimedOut);
    assert_eq!(
        result.failure.as_ref().unwrap().kind,
        HookFailureKind::Timeout
    );
}

/// Verifies program hook execution reports non zero exit.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn program_hook_execution_reports_non_zero_exit() {
    let hook = HookDefinition {
        id: "fail".to_string(),
        event: HookEvent::SessionDetach,
        invocation: HookInvocation::Program {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "exit 7".to_string()],
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: Some(1000),
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();

    let result = execute_program_hook(&plan).unwrap();

    assert_eq!(result.status, HookExecutionStatus::Failed);
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(
        result.failure.as_ref().unwrap().kind,
        HookFailureKind::ExitNonZero
    );
}

/// Verifies program hook execution can emit audit record.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn program_hook_execution_can_emit_audit_record() {
    let root = std::env::temp_dir().join(format!("mez-hook-audit-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let audit_path = root.join("audit.jsonl");
    let mut audit_log = AuditLog::new(AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    });
    let hook = HookDefinition {
        id: "audited".to_string(),
        event: HookEvent::SessionDetach,
        invocation: HookInvocation::Program {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "true".to_string()],
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: Some(1000),
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();

    let result = execute_program_hook_with_audit(
        &plan,
        &mut audit_log,
        "$1",
        AuditActor {
            kind: "primary".to_string(),
            id: "c1".to_string(),
        },
    )
    .unwrap();
    let audit = fs::read_to_string(&audit_path).unwrap();

    assert_eq!(result.status, HookExecutionStatus::Succeeded);
    assert!(audit.contains(r#""event_type":"hook""#));
    assert!(audit.contains(r#""hook_id":"audited""#));
    assert!(audit.contains(r#""outcome":"succeeded""#));

    let _ = fs::remove_dir_all(root);
}

/// Verifies program hook execution enforces timeout.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn program_hook_execution_enforces_timeout() {
    let hook = HookDefinition {
        id: "slow".to_string(),
        event: HookEvent::SessionDetach,
        invocation: HookInvocation::Program {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 1".to_string()],
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: Some(1),
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();

    let result = execute_program_hook(&plan).unwrap();

    assert_eq!(result.status, HookExecutionStatus::TimedOut);
    assert_eq!(
        result.failure.as_ref().unwrap().kind,
        HookFailureKind::Timeout
    );
}

/// Verifies focused shell hook blocks on shell availability.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn focused_shell_hook_blocks_on_shell_availability() {
    let hook = HookDefinition {
        id: "notify".to_string(),
        event: HookEvent::AgentTurnStop,
        invocation: HookInvocation::FocusedShell {
            command: "printf done".to_string(),
        },
        enabled: true,
        required: false,
        agent_hook: true,
        matcher_groups: Vec::new(),
        timeout_ms: Some(5000),
        on_failure: None,
    };

    let plan = plan_hook(&hook).unwrap().unwrap();

    assert!(plan.run_in_focused_shell);
    assert!(plan.blocks_on_shell_availability);
    assert_eq!(plan.shell_command.as_deref(), Some("printf done"));
    assert_eq!(plan.timeout_ms, 5000);
}

/// Verifies focused shell hook executor receives command and payload.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn focused_shell_hook_executor_receives_command_and_payload() {
    let hook = HookDefinition {
        id: "notify".to_string(),
        event: HookEvent::AgentTurnStop,
        invocation: HookInvocation::FocusedShell {
            command: "printf done".to_string(),
        },
        enabled: true,
        required: false,
        agent_hook: true,
        matcher_groups: Vec::new(),
        timeout_ms: Some(5000),
        on_failure: None,
    };
    let plan = plan_hook_with_payload(&hook, r#"{"turn":"t1"}"#)
        .unwrap()
        .unwrap();
    let mut executor = FakeFocusedShell::default();

    let result = execute_focused_shell_hook(&plan, &mut executor).unwrap();

    assert_eq!(result.status, HookExecutionStatus::Succeeded);
    assert_eq!(executor.seen_command.as_deref(), Some("printf done"));
    assert_eq!(executor.seen_payload.as_deref(), Some(r#"{"turn":"t1"}"#));
}

/// Verifies focused shell hook reports unavailable shell.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn focused_shell_hook_reports_unavailable_shell() {
    let hook = HookDefinition {
        id: "notify".to_string(),
        event: HookEvent::AgentTurnStop,
        invocation: HookInvocation::FocusedShell {
            command: "printf done".to_string(),
        },
        enabled: true,
        required: false,
        agent_hook: true,
        matcher_groups: Vec::new(),
        timeout_ms: Some(5000),
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();
    let mut executor = FakeFocusedShell {
        output: Some(FocusedShellHookOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: "busy".to_string(),
            timed_out: false,
            shell_unavailable: true,
            policy_denied: false,
        }),
        ..FakeFocusedShell::default()
    };

    let result = execute_focused_shell_hook(&plan, &mut executor).unwrap();

    assert_eq!(result.status, HookExecutionStatus::Failed);
    assert_eq!(
        result.failure.as_ref().unwrap().kind,
        HookFailureKind::ShellUnavailable
    );
    assert!(result.failure.as_ref().unwrap().retryable);
}

/// Verifies focused shell hook reports unobserved completion as queued.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn focused_shell_hook_reports_unobserved_completion_as_queued() {
    let hook = HookDefinition {
        id: "queued".to_string(),
        event: HookEvent::SessionStart,
        invocation: HookInvocation::FocusedShell {
            command: "printf queued".to_string(),
        },
        enabled: true,
        required: false,
        agent_hook: true,
        matcher_groups: Vec::new(),
        timeout_ms: None,
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();
    let mut executor = FakeFocusedShell {
        output: Some(FocusedShellHookOutput {
            exit_code: None,
            stdout: "queued".to_string(),
            stderr: String::new(),
            timed_out: false,
            shell_unavailable: false,
            policy_denied: false,
        }),
        ..FakeFocusedShell::default()
    };

    let result = execute_focused_shell_hook(&plan, &mut executor).unwrap();

    assert_eq!(result.status, HookExecutionStatus::Queued);
    assert!(result.failure.is_none());
}

/// Verifies focused shell hook execution can emit audit record.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn focused_shell_hook_execution_can_emit_audit_record() {
    let root = std::env::temp_dir().join(format!("mez-focused-hook-audit-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let audit_path = root.join("audit.jsonl");
    let mut audit_log = AuditLog::new(AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    });
    let hook = HookDefinition {
        id: "notify".to_string(),
        event: HookEvent::AgentTurnStop,
        invocation: HookInvocation::FocusedShell {
            command: "printf done".to_string(),
        },
        enabled: true,
        required: false,
        agent_hook: true,
        matcher_groups: Vec::new(),
        timeout_ms: Some(5000),
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();
    let mut executor = FakeFocusedShell {
        output: Some(FocusedShellHookOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: "busy".to_string(),
            timed_out: false,
            shell_unavailable: true,
            policy_denied: false,
        }),
        ..FakeFocusedShell::default()
    };

    let result = execute_focused_shell_hook_with_audit(
        &plan,
        &mut executor,
        &mut audit_log,
        "$1",
        AuditActor {
            kind: "primary".to_string(),
            id: "c1".to_string(),
        },
    )
    .unwrap();
    let audit = fs::read_to_string(&audit_path).unwrap();

    assert_eq!(result.status, HookExecutionStatus::Failed);
    assert!(audit.contains(r#""action":"execute_focused_shell_hook""#));
    assert!(audit.contains(r#""hook_id":"notify""#));
    assert!(audit.contains(r#""runner":"focused_shell""#));
    assert!(audit.contains(r#""failure_kind":"shell_unavailable""#));

    let _ = fs::remove_dir_all(root);
}

/// Verifies focused shell queue blocks agent hook until shell available.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn focused_shell_queue_blocks_agent_hook_until_shell_available() {
    let hook = HookDefinition {
        id: "pre".to_string(),
        event: HookEvent::PreShellCommand,
        invocation: HookInvocation::FocusedShell {
            command: "printf hook".to_string(),
        },
        enabled: true,
        required: true,
        agent_hook: true,
        matcher_groups: Vec::new(),
        timeout_ms: Some(5000),
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();
    let mut queue = FocusedShellHookQueue::default();
    let mut executor = FakeFocusedShell::default();

    let sequence = queue.enqueue(plan).unwrap();
    let blocked = queue.dispatch_next(false, &mut executor).unwrap().unwrap();

    assert_eq!(sequence, 1);
    assert_eq!(
        blocked.status,
        FocusedShellHookDispatchStatus::BlockedOnShell
    );
    assert_eq!(queue.len(), 1);
    assert!(executor.seen_command.is_none());

    let executed = queue.dispatch_next(true, &mut executor).unwrap().unwrap();
    assert_eq!(executed.status, FocusedShellHookDispatchStatus::Executed);
    assert_eq!(
        executed.result.unwrap().status,
        HookExecutionStatus::Succeeded
    );
    assert!(queue.is_empty());
    assert_eq!(executor.seen_command.as_deref(), Some("printf hook"));
}

/// Verifies focused shell queue sequence overflow is rejected.
///
/// Focused-shell hook sequence numbers identify queued dispatches in traces and
/// audit output. The queue must fail closed at `u64::MAX` instead of assigning
/// duplicate saturated sequence numbers to later hooks.
#[test]
fn focused_shell_queue_rejects_sequence_overflow() {
    let hook = HookDefinition {
        id: "pre".to_string(),
        event: HookEvent::PreShellCommand,
        invocation: HookInvocation::FocusedShell {
            command: "printf hook".to_string(),
        },
        enabled: true,
        required: true,
        agent_hook: true,
        matcher_groups: Vec::new(),
        timeout_ms: Some(5000),
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();
    let mut queue = FocusedShellHookQueue {
        next_sequence: u64::MAX - 1,
        pending: Default::default(),
    };

    assert_eq!(queue.enqueue(plan.clone()).unwrap(), u64::MAX);
    let error = queue.enqueue(plan).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(queue.len(), 1);
}

/// Verifies focused shell queue runs non agent hook without shell gate.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn focused_shell_queue_runs_non_agent_hook_without_shell_gate() {
    let hook = HookDefinition {
        id: "notify".to_string(),
        event: HookEvent::PaneClose,
        invocation: HookInvocation::FocusedShell {
            command: "printf pane".to_string(),
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: None,
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();
    let mut queue = FocusedShellHookQueue::default();
    let mut executor = FakeFocusedShell::default();

    queue.enqueue(plan).unwrap();
    let executed = queue.dispatch_next(false, &mut executor).unwrap().unwrap();

    assert_eq!(executed.status, FocusedShellHookDispatchStatus::Executed);
    assert_eq!(executor.seen_command.as_deref(), Some("printf pane"));
    assert!(queue.is_empty());
}

/// Verifies disabled hook has no execution plan.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn disabled_hook_has_no_execution_plan() {
    let hook = HookDefinition {
        id: "disabled".to_string(),
        event: HookEvent::PaneCreate,
        invocation: HookInvocation::Program {
            command: "true".to_string(),
            args: Vec::new(),
        },
        enabled: false,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: None,
        on_failure: None,
    };

    assert!(plan_hook(&hook).unwrap().is_none());
}

/// Verifies hook validation rejects empty commands.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn hook_validation_rejects_empty_commands() {
    let hook = HookDefinition {
        id: "bad".to_string(),
        event: HookEvent::PaneCreate,
        invocation: HookInvocation::FocusedShell {
            command: String::new(),
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: None,
        on_failure: None,
    };

    let error = plan_hook(&hook).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies event planning filters hooks and carries payload.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn event_planning_filters_hooks_and_carries_payload() {
    let hooks = vec![
        HookDefinition {
            id: "start".to_string(),
            event: HookEvent::SessionStart,
            invocation: HookInvocation::Program {
                command: "logger".to_string(),
                args: Vec::new(),
            },
            enabled: true,
            required: true,
            agent_hook: false,
            matcher_groups: Vec::new(),
            timeout_ms: None,
            on_failure: None,
        },
        HookDefinition {
            id: "other".to_string(),
            event: HookEvent::SessionStop,
            invocation: HookInvocation::Program {
                command: "logger".to_string(),
                args: Vec::new(),
            },
            enabled: true,
            required: false,
            agent_hook: false,
            matcher_groups: Vec::new(),
            timeout_ms: None,
            on_failure: None,
        },
    ];

    let plan = plan_event(&hooks, HookEvent::SessionStart, r#"{"session":"$1"}"#).unwrap();

    assert_eq!(plan.plans.len(), 1);
    assert_eq!(plan.plans[0].hook_id, "start");
    assert_eq!(plan.plans[0].event_payload_json, r#"{"session":"$1"}"#);
    assert_eq!(plan.plans[0].on_failure, HookOnFailure::Block);
}

/// Verifies event planning applies matcher groups to payload.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn event_planning_applies_matcher_groups_to_payload() {
    let hooks = vec![HookDefinition {
        id: "pane-filtered".to_string(),
        event: HookEvent::UserPromptSubmit,
        invocation: HookInvocation::Program {
            command: "logger".to_string(),
            args: Vec::new(),
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: vec![
            HookMatcherGroup {
                predicates: vec![HookMatcherPredicate {
                    path: "pane_id".to_string(),
                    operator: HookMatcherOperator::Prefix("pane-".to_string()),
                }],
            },
            HookMatcherGroup {
                predicates: vec![HookMatcherPredicate {
                    path: "/turn/pane_id".to_string(),
                    operator: HookMatcherOperator::Equals("fallback".to_string()),
                }],
            },
        ],
        timeout_ms: None,
        on_failure: None,
    }];

    let matching = plan_event(
        &hooks,
        HookEvent::UserPromptSubmit,
        r#"{"pane_id":"pane-1"}"#,
    )
    .unwrap();
    let filtered = plan_event(
        &hooks,
        HookEvent::UserPromptSubmit,
        r#"{"pane_id":"other"}"#,
    )
    .unwrap();
    let pointer_match = plan_event(
        &hooks,
        HookEvent::UserPromptSubmit,
        r#"{"turn":{"pane_id":"fallback"}}"#,
    )
    .unwrap();

    assert_eq!(matching.plans.len(), 1);
    assert!(filtered.plans.is_empty());
    assert_eq!(pointer_match.plans.len(), 1);
}

/// Verifies completed events cannot be retroactively blocked.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn completed_events_cannot_be_retroactively_blocked() {
    let hook = HookDefinition {
        id: "snapshot".to_string(),
        event: HookEvent::LayoutLoad,
        invocation: HookInvocation::Program {
            command: "false".to_string(),
            args: Vec::new(),
        },
        enabled: true,
        required: false,
        agent_hook: false,
        matcher_groups: Vec::new(),
        timeout_ms: None,
        on_failure: None,
    };
    let plan = plan_hook(&hook).unwrap().unwrap();
    let failure = HookFailure {
        hook_id: "snapshot".to_string(),
        event: HookEvent::LayoutLoad,
        kind: HookFailureKind::ExitNonZero,
        message: "failed".to_string(),
        retryable: false,
    };

    assert_eq!(
        decide_hook_failure(&plan, &failure, false),
        HookFailureDecision::Block
    );
    assert_eq!(
        decide_hook_failure(&plan, &failure, true),
        HookFailureDecision::Warn
    );
}
