//! Pure model-discovery policy over an already resolved, trust-filtered catalog.
//!
//! Source precedence is owned by SkillCatalog before this projection. An explicit
//! global false vetoes every declaration; otherwise per-name operator policy,
//! winning-document metadata and global policy are consulted in that order.
//! Absence defaults off. This does not authorize loading or widen action schemas.

use crate::SkillCatalog;
use std::collections::BTreeMap;

/// Eligible model metadata only; paths, diagnostics and document bodies are absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiscoveryMetadata {
    /// Exact winning name for a later separately authorized load.
    pub name: String,
    /// Winning document's bounded usage description.
    pub description: String,
    /// Attribution only, not a source-trust grant.
    pub source: crate::SkillSource,
}

/// Presence-preserving operator policy, separate from document trust and actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillDiscoveryPolicy {
    /// Explicit false is a kill switch; absence allows selective document opt-in.
    pub global: Option<bool>,
    /// Operator decisions keyed by exact winning skill name.
    pub overrides: BTreeMap<String, bool>,
}

impl SkillDiscoveryPolicy {
    /// Resolves discovery eligibility only; it cannot grant source trust.
    pub fn eligible(&self, name: &str, document: Option<bool>) -> bool {
        if self.global == Some(false) {
            return false;
        }
        self.overrides
            .get(name)
            .copied()
            .or(document)
            .or(self.global)
            .unwrap_or(false)
    }

    /// Projects eligible winning metadata without diagnostics, paths or bodies.
    /// The full human catalog remains unchanged and hidden names never appear in
    /// filtered diagnostics. No lower-priority shadow can opt its winner in.
    pub fn project(&self, catalog: &SkillCatalog) -> Vec<SkillDiscoveryMetadata> {
        catalog
            .skills
            .iter()
            .filter(|skill| self.eligible(&skill.name, skill.discovery))
            .map(|skill| SkillDiscoveryMetadata {
                name: skill.name.clone(),
                description: skill.description.clone(),
                source: skill.source,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A trusted winner must be resolved before policy: an opted-in shadow
    /// cannot expose its opted-out winner, and projection leaves human evidence
    /// unchanged while returning no diagnostic paths or bodies.
    #[test]
    fn skill_discovery_filters_winners_without_shadow_fallback() {
        let mut catalog = SkillCatalog::default();
        for (source, discovery) in [
            (crate::SkillSource::User, Some(true)),
            (crate::SkillSource::Project, Some(false)),
        ] {
            catalog.insert(crate::SkillSummary {
                name: "review".to_string(),
                description: "Review workflow".to_string(),
                discovery,
                source,
                path: std::path::PathBuf::from("/private/review/SKILL.md"),
            });
        }
        let original = catalog.clone();
        let policy = SkillDiscoveryPolicy::default();
        assert!(policy.project(&catalog).is_empty());
        assert_eq!(catalog, original);
        let opted_in = SkillDiscoveryPolicy {
            global: None,
            overrides: BTreeMap::from([("review".to_string(), true)]),
        };
        let projected = opted_in.project(&catalog);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].source, crate::SkillSource::Project);
        assert_eq!(
            SkillDiscoveryPolicy {
                global: Some(false),
                ..opted_in
            }
            .project(&catalog),
            Vec::new()
        );
    }

    /// All 27 presence combinations preserve the global veto and documented
    /// operator/document precedence, rather than treating absence as false.
    #[test]
    fn skill_discovery_all_presence_combinations() {
        for global in [None, Some(false), Some(true)] {
            for configured in [None, Some(false), Some(true)] {
                for document in [None, Some(false), Some(true)] {
                    let policy = SkillDiscoveryPolicy {
                        global,
                        overrides: configured
                            .map(|v| BTreeMap::from([("review".to_string(), v)]))
                            .unwrap_or_default(),
                    };
                    let expected = global != Some(false)
                        && configured.or(document).or(global).unwrap_or(false);
                    assert_eq!(
                        policy.eligible("review", document),
                        expected,
                        "{global:?}/{configured:?}/{document:?}"
                    );
                }
            }
        }
    }
}
