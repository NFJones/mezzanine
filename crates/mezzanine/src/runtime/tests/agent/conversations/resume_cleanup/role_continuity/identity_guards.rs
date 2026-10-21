//! Native refresh defaults must not weaken immutable registration validation.
//!
//! Genuine delegated identities supply positive controls. Deliberately invalid
//! lower registrations below are negative metadata-confusion fixtures only,
//! never substitutes for the actual spawn/resume/real-prompt causal regression.
//! No sender label, PID or absent rendered external row grants native authority.

use super::*;
use mez_agent::messaging::{ProjectMembership, SenderIdentity};
use mez_core::ids::{PaneId, WindowId};

/// Snapshots only the canonical message owner; failed reconciliation must not
/// mutate registration, presence, mail, acceptance receipts or subscriptions.
fn message_snapshot(service: &RuntimeSessionService) -> serde_json::Value {
    serde_json::to_value(service.message_service().snapshot_state()).unwrap()
}

/// Generic defaults retain an actual live worker's complete routing identity,
/// while explicit creation/status mismatches still fail atomically even with
/// live lineage. The old lineage-only return must not hide either conflict.
#[test]
fn runtime_native_refresh_preserves_defaults_but_rejects_creation_conflicts() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let id = AgentId::opaque(child.clone()).unwrap();
    let original = fixture
        .service
        .message_service()
        .registered_identity(&id)
        .unwrap()
        .clone();
    let before = message_snapshot(&fixture.service);
    for defaults in [&[][..], &["agent-harness"][..]] {
        let refreshed = fixture
            .service
            .ensure_runtime_message_identity(&child, None, "agent", defaults, 100)
            .unwrap();
        assert_eq!(refreshed.role, original.role);
        assert_eq!(refreshed.capabilities, original.capabilities);
        assert_eq!(message_snapshot(&fixture.service), before);
    }
    let full = original
        .capabilities
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let cases: [(&str, &[&str]); 3] = [
        ("explorer", &full),
        ("worker", &["agent-harness"]),
        ("agent", &full),
    ];
    for (role, capabilities) in cases {
        assert!(
            fixture
                .service
                .ensure_runtime_message_identity(&child, None, role, capabilities, 101)
                .is_err()
        );
        assert_eq!(message_snapshot(&fixture.service), before);
    }
    let pane = PaneId::opaque("%1").unwrap();
    assert!(
        fixture
            .service
            .ensure_runtime_message_identity(&child, Some(pane), "agent", &[], 102)
            .is_err()
    );
    assert_eq!(message_snapshot(&fixture.service), before);

    // The public lower registration guard is independently unchanged.
    let mut conflicting = original.clone();
    conflicting.role = Some("explorer".to_owned());
    assert!(
        fixture
            .service
            .message_service_mut()
            .ensure_agent_identity(conflicting, 103)
            .is_err()
    );
    let mut conflicting = original;
    conflicting
        .capabilities
        .push("not-the-native-vector".into());
    assert!(
        fixture
            .service
            .message_service_mut()
            .ensure_agent_identity(conflicting, 104)
            .is_err()
    );
    assert_eq!(message_snapshot(&fixture.service), before);
}

/// Trusted native metadata must still match canonical project, pane and window;
/// even a same-role foreign harness cannot use empty refresh defaults as a
/// wildcard that silently adopts it. Every rejection retains the whole owner.
#[test]
fn runtime_native_refresh_rejects_project_pane_window_and_harness_confusion() {
    for confusion in ["project", "pane", "window", "foreign", "mixed"] {
        let mut fixture = CleanupFixture::new();
        let pane = PaneId::opaque("%1").unwrap();
        let descriptor = fixture.service.find_pane_descriptor("%1").unwrap();
        let membership = ProjectMembership::from_canonical_root(fixture.root.clone());
        fixture
            .service
            .agent_shell_store_mut()
            .install_project_scope("%1", membership.clone())
            .unwrap();
        let mut registered = SenderIdentity {
            agent_id: AgentId::opaque("agent-%1").unwrap(),
            project_scope: Some(membership.scope_id()),
            pane_id: Some(pane.clone()),
            window_id: Some(descriptor.window_id),
            role: Some("worker".into()),
            capabilities: vec![
                "agent-harness".into(),
                "subagent".into(),
                "explore-only".into(),
            ],
            objective: None,
        };
        let expected = match confusion {
            "project" => {
                let other = fixture.root.join("other-project");
                fs::create_dir(&other).unwrap();
                registered.project_scope = Some(
                    ProjectMembership::from_canonical_root(other.canonicalize().unwrap())
                        .scope_id(),
                );
                "project scope"
            }
            "pane" => {
                registered.pane_id = Some(PaneId::opaque("%99").unwrap());
                "pane id"
            }
            "window" => {
                registered.window_id = Some(WindowId::parse('@', "@999999").unwrap());
                "window id"
            }
            "foreign" => {
                registered.role = Some("agent".into());
                registered.capabilities = vec!["external-harness".into()];
                "harness"
            }
            "mixed" => {
                registered.role = Some("agent".into());
                registered.capabilities.push("external-harness".into());
                "harness"
            }
            _ => unreachable!("confusion cases are fixed fixture labels"),
        };
        fixture
            .service
            .message_service_mut()
            .ensure_agent_identity(registered, 1)
            .unwrap();
        let before = message_snapshot(&fixture.service);
        let error = fixture
            .service
            .ensure_runtime_message_identity("agent-%1", Some(pane), "agent", &[], 2)
            .unwrap_err();
        assert!(error.message().contains(expected), "{confusion}: {error}");
        assert_eq!(message_snapshot(&fixture.service), before, "{confusion}");
    }
}

/// Missing native qualification is not a blanket registration error. Ordinary
/// same-role/empty refresh preserves arbitrary routing labels, while a role
/// change or populated capability replacement still fails through lower atomic
/// reconciliation. Labels do not become native qualification or an exemption.
#[test]
fn runtime_native_refresh_without_qualification_keeps_ordinary_lower_rules() {
    for role in ["agent", "worker"] {
        let mut fixture = CleanupFixture::new();
        let created = fixture
            .service
            .ensure_runtime_message_identity(
                "agent-%1",
                None,
                role,
                &["reviewer-route", "local-audience"],
                10,
            )
            .unwrap();
        let before = message_snapshot(&fixture.service);
        let refreshed =
            fixture
                .service
                .ensure_runtime_message_identity("agent-%1", None, "agent", &[], 11);
        if role == "agent" {
            assert_eq!(refreshed.unwrap(), created);
        } else {
            let error = refreshed.unwrap_err();
            assert!(error.message().contains("role cannot change"));
        }
        assert_eq!(message_snapshot(&fixture.service), before);
        let error = fixture
            .service
            .ensure_runtime_message_identity("agent-%1", None, role, &["agent-harness"], 12)
            .unwrap_err();
        assert!(error.message().contains("capabilities cannot change"));
        assert_eq!(message_snapshot(&fixture.service), before);
    }
}

/// Metadata refresh must not add a kernel-readiness requirement: the same
/// actor-owned pane/session registration remains valid while its physical
/// process is absent. This proves routing continuity, NOT execution authority.
#[test]
fn runtime_native_refresh_keeps_registration_without_a_physical_process() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap();
    let id = AgentId::opaque(child.clone()).unwrap();
    let original = fixture
        .service
        .message_service()
        .registered_identity(&id)
        .unwrap()
        .clone();
    fixture.service.terminate_all_pane_processes().unwrap();
    assert!(
        fixture
            .service
            .primary_pid_for_live_pane_process(pane)
            .is_none()
    );
    assert!(fixture.service.find_pane_descriptor(pane).is_some());
    assert!(fixture.service.agent_shell_store().get(pane).is_some());
    let before = message_snapshot(&fixture.service);
    for defaults in [&[][..], &["agent-harness"][..]] {
        let identity = fixture
            .service
            .ensure_runtime_message_identity(&child, None, "agent", defaults, 3)
            .unwrap();
        assert_eq!(identity.role, original.role);
        assert_eq!(identity.capabilities, original.capabilities);
        assert_eq!(message_snapshot(&fixture.service), before);
    }
}

/// A canonical-looking ID without its actor-owned shell session cannot qualify
/// an established worker role for generic normalization. An arbitrary opaque ID
/// also remains ordinary immutable reconciliation, never a native alias.
#[test]
fn runtime_native_refresh_does_not_trust_identifier_or_directory_shape_alone() {
    let root = std::env::temp_dir().join(format!(
        "mez-c433-metadata-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let mut service = RuntimeServiceFixture::new()
        .control_socket(root.join("c.sock"))
        .build();
    let window = service.find_pane_descriptor("%1").unwrap().window_id;
    for agent in ["agent-%1", "independent-opaque-peer"] {
        let identity = SenderIdentity {
            agent_id: AgentId::opaque(agent).unwrap(),
            project_scope: None,
            pane_id: Some(PaneId::opaque("%1").unwrap()),
            window_id: Some(window.clone()),
            role: Some("worker".into()),
            capabilities: vec!["agent-harness".into()],
            objective: None,
        };
        service
            .message_service_mut()
            .ensure_agent_identity(identity, 1)
            .unwrap();
        let before = message_snapshot(&service);
        assert!(
            service
                .ensure_runtime_message_identity(
                    agent,
                    Some(PaneId::opaque("%1").unwrap()),
                    "agent",
                    &[],
                    2
                )
                .is_err()
        );
        assert_eq!(message_snapshot(&service), before);
    }
    assert!(service.agent_shell_store().get("%1").is_none());
    fs::remove_dir_all(root).unwrap();
}

/// A sparse early placeholder is still filled through lower reconciliation.
/// No physical process or lineage is required, and failed explicit mutation
/// cannot partially replace its matching presence projection.
#[test]
fn runtime_native_refresh_preserves_sparse_prestart_enrichment() {
    let mut fixture = CleanupFixture::without_parent_process();
    // Actual supported pane-mode entry established %1 before native startup;
    // refreshing metadata must not itself require or start a physical process.
    let id = AgentId::opaque("agent-%1").unwrap();
    let existing = fixture
        .service
        .message_service()
        .registered_identity(&id)
        .unwrap()
        .clone();
    assert!(
        fixture
            .service
            .primary_pid_for_live_pane_process("%1")
            .is_none()
    );
    let refreshed = fixture
        .service
        .ensure_runtime_message_identity("agent-%1", None, "agent", &["agent-harness"], 4)
        .unwrap();
    assert_eq!(refreshed, existing);

    // The existing standalone sparse regression remains a positive companion;
    // this separate identity has no native alias and exercises lower fill only.
    let sparse_id = AgentId::opaque("c433-sparse-placeholder").unwrap();
    fixture
        .service
        .message_service_mut()
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: sparse_id.clone(),
                project_scope: None,
                pane_id: None,
                window_id: None,
                role: None,
                capabilities: Vec::new(),
                objective: None,
            },
            5,
        )
        .unwrap();
    let filled = fixture
        .service
        .ensure_runtime_message_identity(
            "c433-sparse-placeholder",
            None,
            "agent",
            &["agent-harness"],
            6,
        )
        .unwrap();
    assert_eq!(filled.role.as_deref(), Some("agent"));
    assert_eq!(filled.capabilities, vec!["agent-harness"]);
    assert_eq!(
        fixture
            .service
            .message_service()
            .registered_identity(&sparse_id),
        Some(&filled)
    );
    let before = message_snapshot(&fixture.service);
    let mut conflict = filled;
    conflict.role = Some("worker".into());
    assert!(
        fixture
            .service
            .message_service_mut()
            .ensure_agent_identity(conflict, 7)
            .is_err()
    );
    assert_eq!(message_snapshot(&fixture.service), before);
}
