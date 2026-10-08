//! Native parent-backed ancestry facts, deliberately without enrollment rights.
//!
//! A genuine socket-origin helper selects its native direct parent. Its original
//! parent fence, not a payload PID, survives helper exit. Shared ancestry capture
//! retains that parent's exact duplicate plus root chain, proving lifetime/drop
//! boundaries without treating the parent as an authorized vendor producer.

use super::helper_presentation::{child, release};
use super::*;

/// Captures the exact adapter-root native record for ancestry evidence; a pane
/// hint or an arbitrary supplied PID cannot create this production root owner.
fn root(fixture: &Fixture) -> mez_mux::process::ProcessParentIdentity {
    let root = fixture.service.pane_process_identity("%1").unwrap();
    let native = mez_mux::process::process_parent_identity_for_pid(root.process_id).unwrap();
    assert_eq!(native.start_token, root.start_token);
    native
}

/// Parent capture occurs while the real helper lives, but its own ancestry may
/// be captured later after helper exit. Dropping the original parent descriptor
/// cannot invalidate the witness duplicate. Parent exit invalidates it even while
/// the pane root survives; last witness drop releases all shared reservations.
#[tokio::test(flavor = "current_thread")]
async fn external_parent_ancestry_survives_helper_but_not_verified_source_exit() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (mut socket, connection) = child(&mut fixture).await;
    let origin = connection.unix_origin().unwrap().clone();
    let parent = tokio::task::spawn_blocking(move || origin.capture_parent())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        parent.identity,
        fixture.connection.unix_origin().unwrap().identity
    );
    release(&mut socket, &connection).await;
    assert!(!connection.unix_origin().unwrap().is_live());
    let native_root = root(&fixture);
    let budget = Arc::new(UnixAncestryBudget::default());
    let worker_budget = budget.clone();
    let witness = tokio::task::spawn_blocking(move || {
        let witness = parent
            .capture_ancestry(native_root, &worker_budget)
            .unwrap();
        drop(parent);
        witness
    })
    .await
    .unwrap();
    assert!(witness.is_live());
    assert_eq!(
        budget.reserved(),
        2,
        "source duplicate and root must both be charged"
    );
    release(&mut fixture.socket, &fixture.connection).await;
    assert!(fixture.service.pane_process_identity("%1").is_ok());
    assert!(!witness.is_live());
    assert_eq!(
        budget.reserved(),
        2,
        "death cannot release still-owned descriptors"
    );
    drop(witness);
    assert_eq!(budget.reserved(), 0);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
}

/// Whole-chain parent-backed polling rejects an orphaned source even when its
/// own identity and root remain live and unchanged. A later capture also rejects
/// the now-unrelated tree, without retaining partial reservations or authority.
#[tokio::test(flavor = "current_thread")]
async fn external_parent_ancestry_intermediate_exit_fences_live_endpoints() {
    let Some(mut fixture) = fixture("tree").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let origin = connection.unix_origin().unwrap().clone();
    let parent = tokio::task::spawn_blocking(move || origin.capture_parent())
        .await
        .unwrap()
        .unwrap();
    let native_root = root(&fixture);
    let budget = Arc::new(UnixAncestryBudget::default());
    let worker_budget = budget.clone();
    let (parent, witness) = tokio::task::spawn_blocking(move || {
        let witness = parent
            .capture_ancestry(native_root, &worker_budget)
            .unwrap();
        (parent, witness)
    })
    .await
    .unwrap();
    assert_eq!(budget.reserved(), 4);
    super::ancestry::orphan_middle(&fixture).await;
    assert!(parent.is_live());
    assert!(!witness.is_live());
    drop(witness);
    assert_eq!(budget.reserved(), 0);
    let worker_budget = budget.clone();
    assert!(
        tokio::task::spawn_blocking(move || parent.capture_ancestry(native_root, &worker_budget))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(budget.reserved(), 0);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
}

/// A verified parent still cannot select itself as root, borrow another pane's
/// root, replace a start token, or bypass aggregate capacity. Rejected captures
/// allocate no identity and release only their own budget while the caller's
/// held full-capacity reservation remains intact.
#[tokio::test(flavor = "current_thread")]
async fn external_parent_ancestry_rejects_root_and_capacity_substitution() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let origin = connection.unix_origin().unwrap().clone();
    let parent = tokio::task::spawn_blocking(move || origin.capture_parent())
        .await
        .unwrap()
        .unwrap();
    let native_root = root(&fixture);
    let Some(other) = super::fixture("hold").await else {
        return;
    };
    let other_root = root(&other);
    let budget = Arc::new(UnixAncestryBudget::default());
    let worker_budget = budget.clone();
    let parent = tokio::task::spawn_blocking(move || {
        assert!(
            parent
                .capture_ancestry(parent.identity, &worker_budget)
                .is_err()
        );
        assert!(parent.capture_ancestry(other_root, &worker_budget).is_err());
        let mut stale = native_root;
        stale.start_token = stale.start_token.wrapping_add(1);
        assert!(parent.capture_ancestry(stale, &worker_budget).is_err());
        parent
    })
    .await
    .unwrap();
    assert_eq!(budget.reserved(), 0);
    let held = budget.reserve_for_tests(512).unwrap();
    let worker_budget = budget.clone();
    let error =
        tokio::task::spawn_blocking(move || parent.capture_ancestry(native_root, &worker_budget))
            .await
            .unwrap()
            .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(budget.reserved(), 512);
    drop(held);
    assert_eq!(budget.reserved(), 0);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
}
