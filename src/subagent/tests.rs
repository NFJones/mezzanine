//! Tests for subagent spawn validation and write-scope conflict policy.

use super::{
    BuiltinSubagentRole, CooperationMode, ScopeRegistry, SubagentScopeDeclaration,
    SubagentSpawnRequest, builtin_role_name, builtin_subagent_profiles,
};

/// Runs the request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn request(mode: CooperationMode) -> SubagentSpawnRequest {
    SubagentSpawnRequest {
        parent_agent_id: "a1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: mode,
        cooperation_mode_defaulted: false,
        read_scopes: vec!["src".to_string()],
        read_scopes_defaulted: false,
        write_scopes: vec!["src/parser".to_string()],
        write_scopes_defaulted: false,
        task_prompt: "implement parser".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: false,
    }
}

/// Verifies that explore-only requests reject write scopes but pass validation
/// when the write scope list is empty.
#[test]
fn explore_only_must_not_write() {
    let mut request = request(CooperationMode::ExploreOnly);

    let error = request.validate().unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    request.write_scopes.clear();
    request.validate().unwrap();
}

/// Verifies that unrestricted writes require explicit approval and report that
/// approval requirement to callers.
#[test]
fn unrestricted_requires_user_approval() {
    let mut request = request(CooperationMode::Unrestricted);

    let error = request.validate().unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    request.explicit_user_approval = true;
    request.validate().unwrap();
    assert!(request.requires_user_approval());
}

/// Verifies that write-capable spawn requests may omit child scopes because the
/// runtime derives enforceable scope from the parent agent.
#[test]
fn write_capable_modes_do_not_require_child_scopes() {
    let mut request = request(CooperationMode::OwnedWrite);
    request.read_scopes.clear();
    request.write_scopes.clear();

    request.validate().unwrap();
}

/// Verifies that overlapping owned-write scopes conflict before the second
/// writer is registered.
#[test]
fn overlapping_owned_write_scopes_conflict() {
    let mut registry = ScopeRegistry::new();
    registry
        .register(
            "a2",
            CooperationMode::OwnedWrite,
            &["src".to_string()],
            None,
        )
        .unwrap();

    let error = registry
        .register(
            "a3",
            CooperationMode::OwnedWrite,
            &["src/parser".to_string()],
            None,
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
}

/// Verifies that serial-write registrations sharing the same lock may overlap
/// because callers have opted into serialized mutation.
#[test]
fn serial_write_scopes_can_share_same_lock() {
    let mut registry = ScopeRegistry::new();
    registry
        .register(
            "a2",
            CooperationMode::SerialWrite,
            &["src".to_string()],
            Some("lock-1".to_string()),
        )
        .unwrap();

    registry
        .register(
            "a3",
            CooperationMode::SerialWrite,
            &["src/parser".to_string()],
            Some("lock-1".to_string()),
        )
        .unwrap();
}

/// Verifies that built-in subagent roles keep the stable names consumed by the
/// subagent harness.
#[test]
fn builtin_roles_have_stable_names() {
    assert_eq!(builtin_role_name(BuiltinSubagentRole::Default), "default");
    assert_eq!(builtin_role_name(BuiltinSubagentRole::Worker), "worker");
    assert_eq!(builtin_role_name(BuiltinSubagentRole::Explorer), "explorer");
    let profiles = builtin_subagent_profiles();
    assert!(profiles.contains_key("default"));
    assert!(profiles.contains_key("worker"));
    assert!(profiles.contains_key("explorer"));
}

/// Verifies that later shell commands from an explore-only subagent are checked
/// against declared read scopes and still reject classified mutation-shaped
/// commands while leaving unknown effects to the normal approval policy.
#[test]
fn explore_only_scope_declaration_rejects_out_of_scope_or_mutating_commands() {
    let declaration = SubagentScopeDeclaration {
        cooperation_mode: CooperationMode::ExploreOnly,
        current_directory: "/repo".to_string(),
        read_scopes: vec!["src".to_string()],
        write_scopes: Vec::new(),
        permission_preset: None,
    };

    assert_eq!(
        declaration
            .shell_command_violation("cat src/lib.rs")
            .unwrap(),
        None
    );
    assert!(
        declaration
            .shell_command_violation("cat ../secret.txt")
            .unwrap()
            .unwrap()
            .contains("outside declared read scopes")
    );
    assert_eq!(
        declaration
            .shell_command_violation("python3 - <<'PY'\nprint('metadata')\nPY")
            .unwrap(),
        None
    );
    assert!(
        declaration
            .shell_command_violation("rm src/generated.txt")
            .unwrap()
            .unwrap()
            .contains("cannot write path")
    );
}

/// Verifies that write-capable subagents still reject effects outside their
/// declared write roots. This is the post-spawn enforcement that complements
/// the active write-scope conflict registry.
#[test]
fn write_scope_declaration_rejects_out_of_scope_write_effects() {
    let declaration = SubagentScopeDeclaration {
        cooperation_mode: CooperationMode::OwnedWrite,
        current_directory: "/repo".to_string(),
        read_scopes: vec!["src".to_string()],
        write_scopes: vec!["src/parser".to_string()],
        permission_preset: None,
    };

    assert_eq!(
        declaration
            .shell_command_violation("rm src/parser/generated.rs")
            .unwrap(),
        None
    );
    assert!(
        declaration
            .shell_command_violation("rm src/other/generated.rs")
            .unwrap()
            .unwrap()
            .contains("outside declared write scopes")
    );
}
