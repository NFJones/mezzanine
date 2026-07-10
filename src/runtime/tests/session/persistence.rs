//! Runtime tests for session persistence behavior.

use super::*;

/// Verifies runtime service kill requires force and plans registry removal.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_kill_requires_force_and_plans_registry_removal() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let error = service.kill_session(&primary, false).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    service.kill_session(&primary, true).unwrap();

    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Killed);
    assert!(service.session().windows().is_empty());
    assert!(matches!(
        service.registry_update_plan(),
        RuntimeRegistryUpdatePlan::Remove { .. }
    ));

    let error = service
        .attach_primary("late", true, Size::new(80, 24).unwrap(), 200)
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
}

/// Verifies the interactive `:exit` command shuts down the runtime service.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_exit_command_kills_session_and_plans_registry_removal() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    service.execute_terminal_command(&primary, "exit").unwrap();

    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Killed);
    assert!(service.session().windows().is_empty());
    assert!(matches!(
        service.registry_update_plan(),
        RuntimeRegistryUpdatePlan::Remove { .. }
    ));
}
