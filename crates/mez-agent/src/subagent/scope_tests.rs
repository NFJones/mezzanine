//! Tests for subagent spawn validation and write-scope conflict policy.

use super::{
    CooperationMode, DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT, SubagentApprovalProvenance,
    SubagentParentAuthority, SubagentParentFilesystemBounds, SubagentScopeDeclaration,
    SubagentScopeEnforcement,
};

/// Verifies approval provenance, not a requested cooperation mode, decides
/// whether one declaration may authorize an unrestricted descendant.
///
/// A child that merely asks for unrestricted authority must not be treated as
/// approved, and an approval that backs a read-only mode must not be upgraded
/// into unrestricted authority later.
#[test]
fn approval_provenance_gates_unrestricted_descendant_authority() {
    let requested = SubagentScopeDeclaration {
        cooperation_mode: CooperationMode::Unrestricted,
        approval_provenance: SubagentApprovalProvenance::Requested,
        current_directory: "/repo".to_string(),
        read_scopes: Vec::new(),
        write_scopes: Vec::new(),
        permission_preset: None,
    };
    let approved = SubagentScopeDeclaration {
        approval_provenance: SubagentApprovalProvenance::ExplicitUserApproval,
        ..requested.clone()
    };
    let approval_with_narrow_mode = SubagentScopeDeclaration {
        cooperation_mode: CooperationMode::ExploreOnly,
        ..approved.clone()
    };

    assert!(!requested.carries_approved_unrestricted_authority());
    assert!(approved.carries_approved_unrestricted_authority());
    assert!(!approval_with_narrow_mode.carries_approved_unrestricted_authority());
}

/// Verifies synthetic root filesystem bounds carry scope without approval.
///
/// Root bounds exist to narrow a child's filesystem reach. They carry no
/// cooperation mode and no approval provenance, so only a genuine scoped-parent
/// declaration can report unrestricted authority to a later spawn.
#[test]
fn root_filesystem_bounds_supply_scope_without_cooperation_authority() {
    let bounds = SubagentParentAuthority::RootFilesystemBounds(SubagentParentFilesystemBounds {
        current_directory: "/repo".to_string(),
        read_scopes: vec!["/repo".to_string()],
        write_scopes: vec!["/repo/src".to_string()],
    });

    assert!(bounds.scoped_parent_declaration().is_none());
    assert_eq!(bounds.current_directory(), "/repo");
    assert_eq!(bounds.read_scopes().to_vec(), vec!["/repo".to_string()]);
    assert_eq!(
        bounds.write_scopes().to_vec(),
        vec!["/repo/src".to_string()]
    );
    assert_eq!(bounds.permission_preset(), None);

    let declaration = SubagentScopeDeclaration {
        cooperation_mode: CooperationMode::Unrestricted,
        approval_provenance: SubagentApprovalProvenance::ExplicitUserApproval,
        current_directory: "/repo".to_string(),
        read_scopes: vec!["/repo".to_string()],
        write_scopes: vec!["/repo/src".to_string()],
        permission_preset: None,
    };
    let scoped = SubagentParentAuthority::ScopedParentDeclaration(declaration.clone());

    assert_eq!(scoped.scoped_parent_declaration(), Some(&declaration));
    assert!(
        scoped
            .scoped_parent_declaration()
            .is_some_and(SubagentScopeDeclaration::carries_approved_unrestricted_authority)
    );
    assert_eq!(scoped.read_scopes().to_vec(), vec!["/repo".to_string()]);
}

/// Verifies that later shell commands from an explore-only subagent are checked
/// against declared read scopes and still reject classified mutation-shaped
/// commands while leaving unknown effects to the normal approval policy.
#[test]
fn explore_only_scope_declaration_rejects_out_of_scope_or_mutating_commands() {
    let declaration = SubagentScopeDeclaration {
        cooperation_mode: CooperationMode::ExploreOnly,
        approval_provenance: SubagentApprovalProvenance::Requested,
        current_directory: "/repo".to_string(),
        read_scopes: vec!["src".to_string()],
        write_scopes: Vec::new(),
        permission_preset: None,
    };

    assert_eq!(
        DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT
            .shell_command_violation(&declaration, "cat src/lib.rs")
            .unwrap(),
        None
    );
    assert!(
        DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT
            .shell_command_violation(&declaration, "cat ../secret.txt")
            .unwrap()
            .unwrap()
            .contains("outside declared read scopes")
    );
    assert_eq!(
        DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT
            .shell_command_violation(&declaration, "python3 - <<'PY'\nprint('metadata')\nPY",)
            .unwrap(),
        None
    );
    assert!(
        DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT
            .shell_command_violation(&declaration, "rm src/generated.txt")
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
        approval_provenance: SubagentApprovalProvenance::Requested,
        current_directory: "/repo".to_string(),
        read_scopes: vec!["src".to_string()],
        write_scopes: vec!["src/parser".to_string()],
        permission_preset: None,
    };

    assert_eq!(
        DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT
            .shell_command_violation(&declaration, "rm src/parser/generated.rs")
            .unwrap(),
        None
    );
    assert!(
        DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT
            .shell_command_violation(&declaration, "rm src/other/generated.rs")
            .unwrap()
            .unwrap()
            .contains("outside declared write scopes")
    );
}
