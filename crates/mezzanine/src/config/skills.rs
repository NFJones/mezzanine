//! Primary-user model-discovery policy extraction and validation.
//!
//! Optional booleans preserve absence separately from the global false veto.
//! Project overlays cannot change this policy; trust and winning source selection
//! remain catalog responsibilities. This module does not expose skill actions.

use super::{ConfigDiagnostic, EffectiveConfig};
use mez_agent::skill_discovery::SkillDiscoveryPolicy;

/// Validates the narrow skills policy table with original JSON scalar types.
pub(super) fn validate_skills_config(root: &serde_json::Value) -> Vec<ConfigDiagnostic> {
    let mut diagnostics = Vec::new();
    let Some(skills) = root.get("skills") else {
        return diagnostics;
    };
    let Some(object) = skills.as_object() else {
        return vec![ConfigDiagnostic {
            path: "skills".to_string(),
            message: "skills must be a table".to_string(),
        }];
    };
    if object
        .get("discovery")
        .is_some_and(|value| !value.is_boolean())
    {
        diagnostics.push(ConfigDiagnostic {
            path: "skills.discovery".to_string(),
            message: "skills.discovery must be boolean".to_string(),
        });
    }
    if let Some(overrides) = object.get("overrides") {
        if let Some(overrides) = overrides.as_object() {
            for (name, policy) in overrides {
                let path = format!("skills.overrides.{name}");
                if !mez_agent::is_valid_skill_name(name) {
                    diagnostics.push(ConfigDiagnostic {
                        path: path.clone(),
                        message: "skill override name is invalid".to_string(),
                    });
                }
                let Some(policy) = policy.as_object() else {
                    diagnostics.push(ConfigDiagnostic {
                        path,
                        message: "skill override must be a table".to_string(),
                    });
                    continue;
                };
                if policy
                    .get("discovery")
                    .is_some_and(|value| !value.is_boolean())
                {
                    diagnostics.push(ConfigDiagnostic {
                        path: format!("{path}.discovery"),
                        message: "skill discovery override must be boolean".to_string(),
                    });
                }
            }
        } else {
            diagnostics.push(ConfigDiagnostic {
                path: "skills.overrides".to_string(),
                message: "skills.overrides must be a table".to_string(),
            });
        }
    }
    diagnostics
}

/// Extracts validated presence-preserving policy without materializing defaults.
pub(crate) fn skill_discovery_policy(config: &EffectiveConfig) -> SkillDiscoveryPolicy {
    let mut policy = SkillDiscoveryPolicy {
        global: config
            .get("skills.discovery")
            .and_then(|value| value.parse().ok()),
        ..Default::default()
    };
    for (path, value) in config.values() {
        if let Some(name) = path
            .strip_prefix("skills.overrides.")
            .and_then(|path| path.strip_suffix(".discovery"))
            && mez_agent::is_valid_skill_name(name)
            && let Ok(discovery) = value.value.parse::<bool>()
        {
            policy.overrides.insert(name.to_string(), discovery);
        }
    }
    policy
}
