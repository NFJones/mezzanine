//! Shared project skill invocation and model receipt ownership controls.
//!
//! These use the existing live trust/policy reducer and explicit prompt builder,
//! not an injected filesystem capability or a model-selected path argument.

use super::*;

/// Creates a shared project winner under the existing user-policy fixture.
fn shared_fixture() -> (RuntimeSessionService, AgentTurnRecord, PathBuf, PathBuf) {
    let (mut service, turn, base) = fixture(true);
    let project = base.join("project");
    std::fs::create_dir_all(project.join(".agents/skills/review")).unwrap();
    std::fs::write(
        project.join(".agents/skills/review/SKILL.md"),
        "---\nname: review\ndescription: Shared review\ndiscovery: true\n---\nSHARED_BODY\n",
    )
    .unwrap();
    service.set_pane_current_working_directory("%1", project.clone());
    let mut trust = crate::security::project::ProjectTrustStore::default();
    trust
        .decide(
            project.clone(),
            crate::security::project::TrustDecision::Trusted,
            None,
        )
        .unwrap();
    service.set_project_trust_store(trust, None);
    (service, turn, base, project)
}

/// Human catalogs, explicit invocation and model loading must agree on the
/// shared project winner, not list it while authorization reconstructs only
/// `.mezzanine`. Bodies and paths stay absent from selected metadata; live
/// global veto still denies model access without hiding the human catalog.
#[test]
fn shared_project_skill_runtime_invocation_and_model_loading_agree() {
    let (mut service, turn, base, _) = shared_fixture();
    let catalog = service.effective_skill_catalog_for_pane("%1");
    assert_eq!(catalog.get("review").unwrap().source, SkillSource::Project);
    assert_eq!(catalog.get("review").unwrap().description, "Shared review");
    let explicit = service
        .agent_context_for_pane_prompt("%1", "$review extra", 0)
        .unwrap();
    assert!(explicit.blocks().iter().any(
        |block| block.label == "explicit skill review" && block.content.contains("SHARED_BODY")
    ));
    let mut context = SkillActionContext::default();
    let selected = action(
        &mut service,
        &turn,
        mez_agent::AgentActionPayload::RequestSkills,
        &mut context,
    );
    assert!(!selected.is_error);
    let metadata = selected.structured_content_json.unwrap();
    assert!(metadata.contains("Shared review"));
    assert!(!metadata.contains("SHARED_BODY"));
    assert!(!metadata.contains(".agents"));
    let loaded = action(
        &mut service,
        &turn,
        mez_agent::AgentActionPayload::CallSkill {
            name: "review".into(),
            additional_context: None,
        },
        &mut context,
    );
    assert!(!loaded.is_error);
    assert!(loaded.content_text().contains("SHARED_BODY"));
    configure(&mut service, false);
    assert_eq!(
        service
            .effective_skill_catalog_for_pane("%1")
            .get("review")
            .unwrap()
            .source,
        SkillSource::Project
    );
    std::fs::remove_dir_all(base).unwrap();
}

/// A selected shared winner is fenced by native insertion (even identical
/// bytes), body mutation, live trust revocation, cwd movement and operator
/// veto. None can authorize an old receipt from the summary's parent path.
#[test]
fn shared_project_skill_receipts_revalidate_native_shadow_and_live_authority() {
    for change in ["native", "body", "trust", "cwd", "policy"] {
        let (mut service, turn, base, project) = shared_fixture();
        let mut context = SkillActionContext::default();
        let selected = action(
            &mut service,
            &turn,
            mez_agent::AgentActionPayload::RequestSkills,
            &mut context,
        );
        assert!(selected.content_text().contains("Shared review"));
        let path = project.join(".agents/skills/review/SKILL.md");
        match change {
            "native" => {
                std::fs::create_dir_all(project.join(".mezzanine/skills/review")).unwrap();
                std::fs::copy(&path, project.join(".mezzanine/skills/review/SKILL.md")).unwrap();
            }
            "body" => {
                let text = std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("SHARED_BODY", "CHANGED_BODY");
                std::fs::write(path, text).unwrap();
            }
            "trust" => {
                let mut trust = crate::security::project::ProjectTrustStore::default();
                trust
                    .decide(
                        project.clone(),
                        crate::security::project::TrustDecision::Revoked,
                        None,
                    )
                    .unwrap();
                service.set_project_trust_store(trust, None);
            }
            "cwd" => {
                service.set_pane_current_working_directory("%1", base.clone());
            }
            "policy" => configure(&mut service, false),
            _ => unreachable!(),
        }
        let rejected = action(
            &mut service,
            &turn,
            mez_agent::AgentActionPayload::CallSkill {
                name: "review".into(),
                additional_context: None,
            },
            &mut context,
        );
        assert!(rejected.is_error, "{change}");
        assert!(!rejected.content_text().contains("SHARED_BODY"));
        assert!(!rejected.content_text().contains(".agents"));
        std::fs::remove_dir_all(base).unwrap();
    }
}
