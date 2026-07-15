//! Tests for subagent spawn validation and write-scope conflict policy.

use super::SubagentScopeEnforcement;
use mez_agent::{CooperationMode, SubagentScopeDeclaration};

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
