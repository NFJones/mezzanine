//! Shared skill human-consumer parity through actual deferred command capture.
//!
//! Trusted pane state supplies both discovery roots; the same winner must reach
//! off-actor listing and explicit invocation without new conversation authority.

use super::*;

/// Shared-only and native-shadowed catalogs must have byte-identical deferred
/// and inline displays. Explicit invocation selects that same winner, while
/// pending, rejected, revoked and deeper-denied trust withhold shared placement.
#[test]
fn shared_project_skill_deferred_listing_matches_explicit_winner_and_trust() {
    for native in [false, true] {
        let root = temp_root("shared-consumer-parity");
        let project = root.join("project");
        let shared = project.join(".agents/skills/review");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(project.join(".git")).unwrap();
        fs::write(
            shared.join("SKILL.md"),
            "---\nname: review\ndescription: Shared workflow\n---\nSHARED_BODY\n",
        )
        .unwrap();
        if native {
            let directory = project.join(".mezzanine/skills/review");
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join("SKILL.md"),
                "---\nname: review\ndescription: Native workflow\n---\nNATIVE_BODY\n",
            )
            .unwrap();
        }
        let mut service = test_runtime_service();
        service.set_config_root(root.join("config"));
        service.set_pane_current_working_directory("%1", project.clone());
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        assert!(
            service
                .effective_skill_catalog_for_pane("%1")
                .get("review")
                .is_none()
        );
        let mut trust = ProjectTrustStore::default();
        trust
            .decide(project.clone(), TrustDecision::Trusted, None)
            .unwrap();
        service.set_project_trust_store(trust, None);
        service
            .execute_agent_shell_command(&primary, "/list-skills")
            .unwrap();
        let dispatch = service.take_pending_deferred_agent_commands().remove(0);
        let work = service
            .claim_agent_command_work(
                &dispatch.primary_client_id,
                &dispatch.pane_id,
                &dispatch.command,
                &dispatch.input,
                dispatch.claim_generation,
                &dispatch.conversation_id,
            )
            .unwrap()
            .unwrap();
        let crate::runtime::RuntimeAgentCommandAsyncOutcome::Response { body } =
            RuntimeSessionService::execute_deferred_agent_command(&work)
        else {
            panic!("catalog response required");
        };
        let inline = crate::runtime::runtime_agent_shell_command_response_json(
            "%1",
            "/list-skills",
            Some(&crate::runtime::AgentShellCommandOutcome::Display {
                command: "list-skills".into(),
                body: crate::runtime::commands::lists::runtime_agent_skill_catalog_body(
                    &service.effective_skill_catalog_for_pane("%1"),
                ),
            }),
        );
        assert_eq!(body, inline);
        let winner = if native { "NATIVE_BODY" } else { "SHARED_BODY" };
        let explicit = service
            .agent_context_for_pane_prompt("%1", "$review", 0)
            .unwrap();
        assert!(
            explicit
                .blocks()
                .iter()
                .any(|block| block.label == "explicit skill review"
                    && block.content.contains(winner))
        );
        for decision in [TrustDecision::Rejected, TrustDecision::Revoked] {
            let mut trust = ProjectTrustStore::default();
            trust.decide(project.clone(), decision, None).unwrap();
            service.set_project_trust_store(trust, None);
            assert!(
                service
                    .effective_skill_catalog_for_pane("%1")
                    .get("review")
                    .is_none()
            );
        }
        let nested = project.join("nested");
        fs::create_dir(&nested).unwrap();
        service.set_pane_current_working_directory("%1", nested.clone());
        let mut trust = ProjectTrustStore::default();
        trust
            .decide(project.clone(), TrustDecision::Trusted, None)
            .unwrap();
        trust.decide(nested, TrustDecision::Rejected, None).unwrap();
        service.set_project_trust_store(trust, None);
        assert!(
            service
                .effective_skill_catalog_for_pane("%1")
                .get("review")
                .is_none()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
