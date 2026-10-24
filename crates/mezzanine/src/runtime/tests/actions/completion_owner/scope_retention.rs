//! Exact one-shot retention frames over genuinely admitted actor groups.
//!
//! These controls do not execute dispatches or qualify PaneRemoval. They retain
//! the existing canonical group and use only synchronous begin/end after fixture
//! admission. Cross-runtime negative frames are real observations, not forged
//! claims or witnesses; differing CIDs mean they do not isolate same-CID ABA.

use super::*;
use crate::runtime::agent::CompletionScope;

/// Reads the actually issued token; it never resets or signals cancellation.
fn assert_claim_not_cancelled(fixture: &CompletionFixture) {
    assert!(!fixture.cancellation.flag().load(Ordering::SeqCst));
}

/// Different nested requested kinds must share the actual outer retention pin.
/// A nested success cannot clear it, and nested/outer errors preserve the first
/// required primary while retaining canonical source. Cleanup is deliberately
/// false: this is retention/error-attribution coverage, not forced retirement.
#[tokio::test]
async fn runtime_completion_scope_nested_kinds_preserve_outer_and_primary() {
    let mut fixture = CompletionFixture::fetch().await;
    let before = fixture.retained();
    let turn_id = fixture.turn.turn_id.clone();
    let outer = fixture
        .service
        .begin_completion_scope(&turn_id, CompletionScope::Callback);
    let nested = fixture
        .service
        .begin_completion_scope(&turn_id, CompletionScope::Finish);
    assert!(
        fixture
            .service
            .completion_scope_is_active_for_tests(&turn_id, CompletionScope::Callback,)
    );
    assert_eq!(
        fixture
            .service
            .end_completion_scope(nested, "scope_nested_success", false, Ok(17))
            .unwrap(),
        17
    );
    assert!(
        fixture
            .service
            .completion_scope_is_active_for_tests(&turn_id, CompletionScope::Callback,)
    );
    let nested = fixture
        .service
        .begin_completion_scope(&turn_id, CompletionScope::Stop);
    let primary = MezError::conflict("first nested retention error");
    let nested_error = fixture
        .service
        .end_completion_scope::<()>(nested, "scope_nested_error", false, Err(primary.clone()))
        .unwrap_err();
    assert_eq!(nested_error.kind(), primary.kind());
    assert_eq!(nested_error.message(), primary.message());
    assert!(
        fixture
            .service
            .completion_scope_is_active_for_tests(&turn_id, CompletionScope::Callback,)
    );
    let outer_error = fixture
        .service
        .end_completion_scope::<()>(
            outer,
            "scope_outer_error",
            false,
            Err(MezError::invalid_state("later outer retention error")),
        )
        .unwrap_err();
    assert_eq!(outer_error.kind(), primary.kind());
    assert_eq!(outer_error.message(), primary.message());
    assert!(
        !fixture
            .service
            .completion_scope_is_active_for_tests(&turn_id, CompletionScope::Callback,)
    );
    assert_eq!(fixture.retained().action_results, before.action_results);
    let (_, _, failure) = fixture
        .service
        .original_completion_source_for_tests(&turn_id, &fixture.group)
        .unwrap();
    assert_eq!(failure.unwrap().message(), primary.message());
    assert_claim_not_cancelled(&fixture);
}

/// A missing group creates no close authority, for success or error, and an
/// exact successful owned close changes no canonical source. This control does
/// not manufacture a group after an absent observation or claim reset/ABA.
#[tokio::test]
async fn runtime_completion_scope_absent_entry_and_matching_success_are_inert() {
    let mut fixture = CompletionFixture::fetch().await;
    let before = fixture.retained();
    for fail in [false, true] {
        let absent = fixture
            .service
            .begin_completion_scope("unadmitted-scope-turn", CompletionScope::Finish);
        let result = if fail {
            Err(MezError::conflict("absent scope operation error"))
        } else {
            Ok(23)
        };
        let closed = fixture
            .service
            .end_completion_scope(absent, "scope_absent", true, result);
        if fail {
            assert_eq!(
                closed.unwrap_err().message(),
                "absent scope operation error"
            );
        } else {
            assert_eq!(closed.unwrap(), 23);
        }
    }
    let turn_id = fixture.turn.turn_id.clone();
    let scope = fixture
        .service
        .begin_completion_scope(&turn_id, CompletionScope::Finish);
    assert!(
        fixture
            .service
            .completion_scope_is_active_for_tests(&turn_id, CompletionScope::Finish,)
    );
    assert_eq!(
        fixture
            .service
            .end_completion_scope(scope, "scope_matching_success", true, Ok(29))
            .unwrap(),
        29
    );
    assert!(
        !fixture
            .service
            .completion_scope_is_active_for_tests(&turn_id, CompletionScope::Finish,)
    );
    assert_eq!(fixture.retained().action_results, before.action_results);
    assert!(
        fixture
            .service
            .original_completion_source_for_tests(&turn_id, &fixture.group)
            .unwrap()
            .2
            .is_none()
    );
    assert_claim_not_cancelled(&fixture);
}

/// Two independently admitted runtimes own distinct real group allocations.
/// Delivering a nested frame to the other runtime must neither end its actual
/// scope nor cancel its real claimed token, even with cleanup_on_error=true.
/// An existing operation error remains primary; an otherwise successful stale
/// close reports mismatch. Different original CIDs are explicit, not forged to
/// claim isolated same-CID/turn/group-label reincarnation coverage.
#[tokio::test]
async fn runtime_completion_scope_foreign_observation_cannot_mutate_current_owner() {
    let mut original = CompletionFixture::fetch().await;
    let mut current = CompletionFixture::fetch().await;
    assert_eq!(original.turn.turn_id, current.turn.turn_id);
    assert_ne!(original.turn.conversation_id, current.turn.conversation_id);
    let before = current.retained();
    let turn_id = current.turn.turn_id.clone();
    let original_outer = original
        .service
        .begin_completion_scope(&original.turn.turn_id, CompletionScope::Callback);
    let current_outer = current
        .service
        .begin_completion_scope(&turn_id, CompletionScope::Callback);
    for fail in [false, true] {
        let foreign = original
            .service
            .begin_completion_scope(&original.turn.turn_id, CompletionScope::Stop);
        let result = if fail {
            Err(MezError::conflict("foreign close keeps operation primary"))
        } else {
            Ok(())
        };
        let error = current
            .service
            .end_completion_scope(foreign, "scope_foreign", true, result)
            .unwrap_err();
        if fail {
            assert_eq!(error.kind(), MezError::conflict("kind").kind());
            assert_eq!(error.message(), "foreign close keeps operation primary");
        } else {
            assert!(error.message().contains("scope owner changed"));
        }
        assert!(
            current
                .service
                .completion_scope_is_active_for_tests(&turn_id, CompletionScope::Callback,)
        );
        assert_eq!(current.retained().action_results, before.action_results);
        assert!(
            current
                .service
                .original_completion_source_for_tests(&turn_id, &current.group)
                .unwrap()
                .2
                .is_none()
        );
        assert_claim_not_cancelled(&current);
        assert!(current.service.approved_external_action_attempt_is_current(
            &turn_id,
            &current.action.id,
            &current.attempt,
        ));
    }
    original
        .service
        .end_completion_scope(original_outer, "scope_original_close", false, Ok(()))
        .unwrap();
    current
        .service
        .end_completion_scope(current_outer, "scope_current_close", false, Ok(()))
        .unwrap();
}
