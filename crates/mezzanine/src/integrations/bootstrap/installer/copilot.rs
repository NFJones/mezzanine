//! Captured Copilot default selection over immutable ownership variants.
//!
//! A matching authored boolean is never ownership. Only recognized private or
//! legacy receipts retain an existing claim; pending recovery is virtually
//! completed before selecting current intent. Selection observations join the
//! normal publication fence, including settings skipped due to authored policy.

use super::super::{copilot_artifact, jsonc};
use super::*;

/// Recognizes exact previous ownership; unknown receipts never guide a default.
fn owned(bytes: Option<&[u8]>, manifest: &Manifest, history: &[Manifest]) -> Result<bool> {
    let Some(bytes) = bytes else {
        return Ok(false);
    };
    let receipt: Receipt = serde_json::from_slice(bytes)
        .map_err(|_| MezError::conflict("bootstrap Copilot receipt unavailable"))?;
    if receipt.schema != 1
        || receipt.manifest.harness != "copilot"
        || (receipt.manifest != *manifest
            && !history.contains(&receipt.manifest)
            && !super::super::compiled_variant_matches(&receipt.manifest, manifest))
    {
        return Err(MezError::conflict(
            "bootstrap Copilot receipt outside compiled authority",
        ));
    }
    Ok(receipt.manifest.entries.iter().any(|entry| {
        matches!(
            entry.artifact,
            super::super::reconciliation::Artifact::JsoncBoolEntry { .. }
        )
    }))
}

/// Computes a content-free policy label; no setting is adopted or overwritten.
fn decision(bytes: Option<&[u8]>, previous_owned: bool, install: bool) -> (bool, &'static str) {
    if !install && previous_owned {
        return (true, "removed-owned-default");
    }
    let value = match bytes.map(jsonc::document).transpose() {
        Ok(value) => value.unwrap_or_else(|| serde_json::json!({})),
        Err(_) => return (previous_owned, "unavailable-invalid-settings"),
    };
    let Some(object) = value.as_object() else {
        return (previous_owned, "unavailable-invalid-settings");
    };
    let flags = match object.get("enabledFeatureFlags") {
        None => None,
        Some(serde_json::Value::Object(flags)) => Some(flags),
        _ => return (previous_owned, "unavailable-invalid-settings"),
    };
    if flags.is_some_and(|flags| flags.values().any(|value| !value.is_boolean())) {
        return (previous_owned, "unavailable-invalid-settings");
    }
    let flag = flags.and_then(|flags| flags.get("EXTENSIONS"));
    if flag.is_some_and(|flag| !flag.is_boolean()) {
        return (previous_owned, "unavailable-invalid-settings");
    }
    let extensions = match object.get("extensions") {
        None => None,
        Some(serde_json::Value::Object(extensions)) => Some(extensions),
        _ => return (previous_owned, "unavailable-invalid-settings"),
    };
    if extensions
        .and_then(|extensions| extensions.get("mode"))
        .is_some_and(|value| {
            !matches!(
                value.as_str(),
                Some("disabled" | "load_only" | "load_and_augment")
            )
        })
        || extensions
            .and_then(|extensions| extensions.get("disabledExtensions"))
            .is_some_and(|value| {
                value
                    .as_array()
                    .is_none_or(|values| values.iter().any(|value| !value.is_string()))
            })
        || object
            .get("experimental")
            .is_some_and(|value| !value.is_boolean())
    {
        return (previous_owned, "unavailable-invalid-settings");
    }
    if extensions.and_then(|extensions| extensions.get("mode"))
        == Some(&serde_json::json!("disabled"))
        || extensions
            .and_then(|extensions| extensions.get("disabledExtensions"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some("user:mezzanine")))
    {
        return (previous_owned, "vendor-policy-disabled");
    }
    match flag.and_then(serde_json::Value::as_bool) {
        Some(false) => (previous_owned, "authored-disabled"),
        Some(true) => (
            previous_owned,
            if previous_owned {
                "owned-default-enabled"
            } else {
                "authored-enabled"
            },
        ),
        None if object.get("experimental") == Some(&serde_json::Value::Bool(false)) => {
            (previous_owned, "authored-experiments-disabled")
        }
        None if install => (true, "owned-default-enabled"),
        None => (false, "not-installed"),
    }
}

/// Selects only a compiled graph from captured virtual preimages, then joins all
/// policy/receipt observations to the existing atomic publication race fence.
pub(super) fn plan(
    publisher: Publisher,
    manifest: &Manifest,
    operation: Operation,
    history: &[Manifest],
) -> Result<Plan> {
    let pending = publisher.inspect_pending_authorized(|version, value, changes| {
        authorize_recovery(&publisher, manifest, history, version, value, changes)
    })?;
    let read = |path: &str| -> Result<Option<Vec<u8>>> {
        match pending
            .as_ref()
            .and_then(|pending| pending.changes.iter().find(|change| change.path == path))
        {
            Some(change) => Ok(change.after.clone()),
            None => publisher.read(path),
        }
    };
    let mut observations = Vec::new();
    let mut previous_owned = false;
    for path in [
        private_receipt_path("copilot")?,
        "mez-bootstrap-ownership-copilot.json".into(),
    ] {
        let bytes = read(&path)?;
        previous_owned |= owned(bytes.as_deref(), manifest, history)?;
        observations.push((path, bytes));
    }
    let settings = read("settings.json")?;
    let (own, status) = decision(
        settings.as_deref(),
        previous_owned,
        matches!(operation, Operation::Install),
    );
    observations.push(("settings.json".into(), settings));
    let selected = copilot_artifact::activation_manifest(own)
        .ok_or_else(|| MezError::invalid_state("bootstrap Copilot manifest unavailable"))?;
    let mut plan = plan_with_publisher(publisher, &selected, operation, history)?;
    if plan.pending != pending {
        return Err(MezError::conflict(
            "bootstrap Copilot selection recovery changed",
        ));
    }
    for observation in observations {
        if let Some((_, before)) = plan
            .observed
            .iter()
            .find(|(path, _)| path == &observation.0)
        {
            if before != &observation.1 {
                return Err(MezError::conflict(
                    "bootstrap Copilot selection preimage changed",
                ));
            }
        } else {
            plan.observed.push(observation);
        }
    }
    plan.copilot_activation = Some(status);
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::bootstrap::{fixtures, state_directory::StateDirectory};
    use std::path::PathBuf;

    /// Isolated native roots and private receipts; no real user/vendor is read.
    struct Fixture {
        base: PathBuf,
        home: PathBuf,
        root: PathBuf,
    }
    impl Fixture {
        /// Creates only new private directories and synthetic optional settings.
        fn new(settings: Option<&[u8]>) -> Self {
            let base = std::env::temp_dir().join(format!(
                "mez-copilot-default-{}",
                crate::storage::token_usage::new_token_usage_event_id()
            ));
            fixtures::create_private_directory(&base).unwrap();
            let home = base.join("home");
            let root = base.join("vendor");
            fixtures::create_private_directory(&home).unwrap();
            fixtures::create_private_directory(&root).unwrap();
            if let Some(bytes) = settings {
                std::fs::write(root.join("settings.json"), bytes).unwrap();
            }
            Self { base, home, root }
        }
        /// Captures the actual current private installer and immutable variants.
        fn plan(&self, operation: Operation) -> Result<Plan> {
            plan_private(
                &self.root,
                &self.home,
                &copilot_artifact::manifest().unwrap(),
                operation,
            )
        }
        /// Reads exact private ownership rather than inferring from user text.
        fn owns_flag(&self) -> bool {
            let bytes = fixture_private_receipt(&self.root, &self.home, "copilot").unwrap();
            owned(
                bytes.as_deref(),
                &copilot_artifact::manifest().unwrap(),
                &super::super::super::compiled_history(&copilot_artifact::manifest().unwrap()),
            )
            .unwrap()
        }
        /// Resolves the accepted journal through actual native root identity.
        fn journal(&self) -> PathBuf {
            StateDirectory::inspect(&self.home, &std::fs::File::open(&self.root).unwrap())
                .unwrap()
                .fixture_path()
                .join("journal.json")
        }
    }
    impl Drop for Fixture {
        /// Removes only this unique test-owned tree, including failed fixtures.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    /// Missing defaults become exact leaf ownership; repeat is inert and
    /// uninstall removes only the owned leaf after unrelated authored edits.
    #[test]
    fn bootstrap_copilot_defaults_are_owned_and_uninstall_preserves_siblings() {
        let f = Fixture::new(None);
        let plan = f.plan(Operation::Install).unwrap();
        assert_eq!(plan.copilot_activation(), Some("owned-default-enabled"));
        assert!(!f.root.join("settings.json").exists());
        plan.apply().unwrap();
        assert!(f.owns_flag());
        assert!(
            f.plan(Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
        std::fs::write(f.root.join("settings.json"), br#"{/*authored*/"enabledFeatureFlags":{"EXTENSIONS":true,"OTHER":false},"theme":"dark"}"#).unwrap();
        f.plan(Operation::Uninstall).unwrap().apply().unwrap();
        let bytes = std::fs::read(f.root.join("settings.json")).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("/*authored*/"));
        assert_eq!(
            jsonc::document(&bytes).unwrap(),
            serde_json::json!({"enabledFeatureFlags":{"OTHER":false},"theme":"dark"})
        );
        assert!(!f.owns_flag());
    }

    /// Existing authored true/false is never adopted. Disabled or malformed
    /// policy remains exact bytes and reports unavailable without flag writes.
    #[test]
    fn bootstrap_copilot_authored_settings_are_not_owned_or_overridden() {
        for (bytes, status) in [
            (
                br#"{/*keep*/"enabledFeatureFlags":{"EXTENSIONS":true}}"#.as_slice(),
                "authored-enabled",
            ),
            (
                br#"{"enabledFeatureFlags":{"EXTENSIONS":false}}"#,
                "authored-disabled",
            ),
            (
                br#"{"extensions":{"mode":"disabled"}}"#,
                "vendor-policy-disabled",
            ),
            (
                br#"{"extensions":{"disabledExtensions":["user:mezzanine"]}}"#,
                "vendor-policy-disabled",
            ),
            (
                br#"{"experimental":false}"#,
                "authored-experiments-disabled",
            ),
            (b"AUTHORED INVALID SETTINGS", "unavailable-invalid-settings"),
            (
                br#"{"extensions":{"mode":"unknown"}}"#,
                "unavailable-invalid-settings",
            ),
            (
                br#"{"extensions":{"disabledExtensions":"user:mezzanine"}}"#,
                "unavailable-invalid-settings",
            ),
            (
                br#"{"experimental":"false"}"#,
                "unavailable-invalid-settings",
            ),
            (
                br#"{"enabledFeatureFlags":{"OTHER":0}}"#,
                "unavailable-invalid-settings",
            ),
            (
                br#"{"enabledFeatureFlags":{},"enabledFeatureFlags":{}}"#,
                "unavailable-invalid-settings",
            ),
            (
                br#"{"enabledFeatureFlags":{"EXTENSIONS":"true"}}"#,
                "unavailable-invalid-settings",
            ),
        ] {
            let f = Fixture::new(Some(bytes));
            let plan = f.plan(Operation::Install).unwrap();
            assert_eq!(plan.copilot_activation(), Some(status));
            plan.apply().unwrap();
            assert!(!f.owns_flag());
            assert!(
                f.plan(Operation::Install)
                    .unwrap()
                    .changed_paths()
                    .is_empty()
            );
            f.plan(Operation::Uninstall).unwrap().apply().unwrap();
            assert_eq!(std::fs::read(f.root.join("settings.json")).unwrap(), bytes);
        }
    }

    /// Selection captures skipped settings too, so late authored edits refuse
    /// publication. A later explicit false editing our owned true never resets.
    #[test]
    fn bootstrap_copilot_default_selection_refuses_races_and_owned_edits() {
        let f = Fixture::new(Some(br#"{"enabledFeatureFlags":{"EXTENSIONS":true}}"#));
        let plan = f.plan(Operation::Install).unwrap();
        let changed = br#"{"enabledFeatureFlags":{"EXTENSIONS":false}}"#;
        std::fs::write(f.root.join("settings.json"), changed).unwrap();
        assert!(plan.apply().is_err());
        assert!(!f.root.join("hooks/mezzanine.json").exists());
        assert_eq!(
            std::fs::read(f.root.join("settings.json")).unwrap(),
            changed
        );
        let g = Fixture::new(None);
        g.plan(Operation::Install).unwrap().apply().unwrap();
        std::fs::write(g.root.join("settings.json"), changed).unwrap();
        assert!(g.plan(Operation::Install).is_err());
        assert!(g.plan(Operation::Uninstall).is_err());
        assert_eq!(
            std::fs::read(g.root.join("settings.json")).unwrap(),
            changed
        );
    }

    /// New boolean intent needs journal9; relabelled older versions cannot gain
    /// that authority. Virtual recovery then preserves the original exact claim.
    #[test]
    fn bootstrap_copilot_default_journal_recovers_exact_boolean_authority() {
        let f = Fixture::new(None);
        let plan = f.plan(Operation::Install).unwrap();
        plan.fixture_interrupt_after(1);
        assert!(plan.apply().is_err());
        let original = std::fs::read(f.journal()).unwrap();
        let journal: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(journal["version"], 9);
        for version in [2, 3, 4, 5, 6, 7, 8, 10] {
            let mut bad = journal.clone();
            bad["version"] = version.into();
            let bytes = serde_json::to_vec(&bad).unwrap();
            std::fs::write(f.journal(), &bytes).unwrap();
            assert!(f.plan(Operation::Install).is_err());
            assert_eq!(std::fs::read(f.journal()).unwrap(), bytes);
        }
        std::fs::write(f.journal(), original).unwrap();
        let recovered = f.plan(Operation::Install).unwrap();
        assert!(recovered.recovery_pending());
        recovered.apply().unwrap();
        assert!(f.owns_flag());
        assert!(!f.journal().exists());
        assert!(
            f.plan(Operation::Install)
                .unwrap()
                .changed_paths()
                .is_empty()
        );
    }
    /// Same-version compiled variants may move from an unowned authored value
    /// to a newly missing default, without adopting the authored old true. A
    /// missing previously owned leaf can also be repaired without changing peers.
    #[test]
    fn bootstrap_copilot_default_variants_transition_without_adoption() {
        let f = Fixture::new(Some(br#"{"enabledFeatureFlags":{"EXTENSIONS":true}}"#));
        f.plan(Operation::Install).unwrap().apply().unwrap();
        assert!(!f.owns_flag());
        let other = br#"{/*authored*/"enabledFeatureFlags":{"OTHER":false}}"#;
        std::fs::write(f.root.join("settings.json"), other).unwrap();
        f.plan(Operation::Install).unwrap().apply().unwrap();
        assert!(f.owns_flag());
        std::fs::write(f.root.join("settings.json"), other).unwrap();
        f.plan(Operation::Install).unwrap().apply().unwrap();
        let bytes = std::fs::read(f.root.join("settings.json")).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("/*authored*/"));
        assert_eq!(
            jsonc::document(&bytes).unwrap(),
            serde_json::json!({"enabledFeatureFlags":{"OTHER":false,"EXTENSIONS":true}})
        );
    }

    /// Disabled IDs for another extension and a load-only policy do not disable
    /// Mezzanine. Defaulting our missing feature leaf preserves those exact
    /// authored values and unrelated flags rather than granting management.
    #[test]
    fn bootstrap_copilot_default_preserves_unrelated_extension_policy() {
        let value = serde_json::json!({"extensions":{"mode":"load_only","disabledExtensions":["user:other"]},"enabledFeatureFlags":{"OTHER":false}});
        let f = Fixture::new(Some(&serde_json::to_vec(&value).unwrap()));
        f.plan(Operation::Install).unwrap().apply().unwrap();
        assert!(f.owns_flag());
        let mut expected = value;
        expected["enabledFeatureFlags"]["EXTENSIONS"] = true.into();
        assert_eq!(
            jsonc::document(&std::fs::read(f.root.join("settings.json")).unwrap()).unwrap(),
            expected
        );
    }
}
