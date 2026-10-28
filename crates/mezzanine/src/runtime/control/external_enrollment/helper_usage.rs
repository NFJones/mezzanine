//! Request origins for direct-child helpers of already-qualified native sources.
//!
//! Public selectors narrow an existing declared creator; they never mint source
//! authority. Native helper observation runs first on the existing bounded
//! worker, then this capture reobserves the retained creator and qualifies its
//! actual cwd. The code-owned Codex model adapter instead qualifies the native
//! hook writer cwd, since its executor applies a per-request working directory.
//! This never changes generic helper origins or accepts a payload path. Actor
//! settlement must still fence original root/source/identity
//! and a one-shot published durable origin before returning readiness. No
//! expense, lease, presentation, namespace allocation or general RPC occurs.

use super::*;
use crate::storage::token_usage::{
    AccountingProjectRecord, ExternalRequestOrigin, TokenUsageStore, accounting_origin_for_root,
};

/// Canonical ordinary adapters supported by the shared request contract. A name
/// only narrows existing native authority; it never creates or recovers a source.
pub(super) fn supported_harness(harness: &str) -> bool {
    matches!(
        harness,
        "pi" | "opencode" | "claude" | "codex" | "copilot" | "cursor"
    )
}

/// Frozen bounded request metadata, with worker-only publication evidence.
#[derive(Debug, Clone)]
pub(super) struct HelperUsageBegin {
    pub(super) owner: String,
    marker: String,
    producer: ProducerEvidence,
    harness: String,
    /// Only the code-owned Codex model boundary can select native hook cwd.
    native_hook_cwd: bool,
    trust: Option<Arc<crate::security::project::ProjectTrustStore>>,
    inventory: Option<Arc<Vec<AccountingProjectRecord>>>,
    store: TokenUsageStore,
    now: u64,
    published: Arc<OnceLock<ExternalRequestOrigin>>,
}

impl HelperUsageBegin {
    /// Overrides only this unpublished fixture's captured request clock. Native
    /// ancestry, origin qualification, store publication and actor fences still
    /// execute normally; no production caller or RPC can access this test seam.
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn fixture_capture_at(&mut self, now: u64) {
        assert!(now > 0 && self.published.get().is_none());
        self.now = now;
    }

    /// Captures only an existing durably mapped native producer namespace;
    /// unsupported sources and malformed request selectors fail before work.
    pub(super) fn capture(
        service: &RuntimeSessionService,
        binding: &LaunchBinding,
        marker: String,
    ) -> Result<Self> {
        let source = binding
            .enrollment
            .as_ref()
            .filter(|source| source.usage_owner_is_durable)
            .ok_or_else(|| MezError::forbidden("curated request source unavailable"))?;
        if !supported_harness(&binding.harness)
            || marker.len() != 64
            || !marker
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(MezError::invalid_args(
                "curated request selector unavailable",
            ));
        }
        Ok(Self {
            owner: binding.accounting_owner.clone(),
            marker,
            producer: source.producer.clone(),
            harness: binding.harness.clone(),
            native_hook_cwd: false,
            trust: service
                .integration
                .project_trust_store()
                .filter(|store| store.records().take(4097).count() <= 4096)
                .cloned()
                .map(Arc::new),
            inventory: service
                .persistence
                .accounting_projects()
                .filter(|rows| rows.len() <= 4096)
                .map(|rows| Arc::new(rows.to_vec())),
            store: service
                .persistence
                .cloned_token_usage_store()
                .ok_or_else(|| {
                    MezError::invalid_state("curated request durable store unavailable")
                })?,
            now: current_unix_seconds(),
            published: Arc::new(OnceLock::new()),
        })
    }

    /// Captures the code-owned Codex model boundary, whose native hook executor
    /// uses per-request cwd. Public generic begin calls retain creator semantics.
    pub(super) fn capture_codex_hook(
        service: &RuntimeSessionService,
        binding: &LaunchBinding,
        marker: String,
    ) -> Result<Self> {
        if binding.harness != "codex" {
            return Err(MezError::forbidden("Codex request source unavailable"));
        }
        let mut begin = Self::capture(service, binding, marker)?;
        begin.native_hook_cwd = true;
        Ok(begin)
    }

    /// Invoked only after this work's actual native direct-parent ancestry was
    /// published. Payload cwd/session hints never select expense origin. Only
    /// code-owned Codex model hooks use the exact native writer working directory.
    pub(super) fn observe(&self, work: &ExternalEnrollmentWork) -> Result<()> {
        if Instant::now() >= work.deadline
            || !work.ancestry.get().is_some_and(|ancestry| {
                ancestry.is_live()
                    && ancestry.source_matches(self.producer.uid(), self.producer.identity())
            })
        {
            return Err(MezError::forbidden(
                "curated request native ancestry unavailable",
            ));
        }
        let identity = self
            .producer
            .reobserve()
            .map_err(|_| MezError::forbidden("curated request creator unavailable"))?;
        let hook_identity = if self.native_hook_cwd {
            Some(work.origin.reobserve().map_err(|_| {
                MezError::forbidden("Codex native hook unavailable during origin capture")
            })?)
        } else {
            None
        };
        let cwd_pid = hook_identity.map_or(identity.process_id, |hook| hook.process_id);
        let cwd = mez_mux::process::current_working_directory_for_pid(cwd_pid)
            .and_then(|cwd| std::fs::canonicalize(cwd).ok());
        let root = cwd.as_ref().and_then(|cwd| {
            self.trust.as_ref().and_then(|trust| {
                crate::security::project::resolve_canonical_project_trust_provenance(trust, cwd)
                    .trusted_root()
                    .map(std::path::Path::to_path_buf)
            })
        });
        let origin = accounting_origin_for_root(
            root.as_deref(),
            self.inventory.as_ref().map(|rows| rows.as_slice()),
        );
        self.producer.reobserve().map_err(|_| {
            MezError::forbidden("curated request creator changed during origin capture")
        })?;
        if let Some(hook) = hook_identity
            && work.origin.reobserve().ok() != Some(hook)
        {
            return Err(MezError::forbidden(
                "Codex native hook changed during origin capture",
            ));
        }
        if Instant::now() >= work.deadline {
            return Err(MezError::conflict("curated request observation expired"));
        }
        let origin = self.store.begin_external_request(
            &self.owner,
            &self.harness,
            &self.marker,
            origin.project_id(),
            self.now,
        )?;
        self.published
            .set(origin)
            .map_err(|_| MezError::conflict("curated request origin already observed"))
    }

    /// Returns only the actual worker-published first origin, never a supplied
    /// successful Result, and rechecks the immutable source owner/creator.
    pub(super) fn origin(&self, binding: &LaunchBinding) -> Result<ExternalRequestOrigin> {
        if binding.accounting_owner != self.owner
            || binding.harness != self.harness
            || binding.enrollment.as_ref().is_none_or(|source| {
                !source.usage_owner_is_durable || !source.producer.same_owner(&self.producer)
            })
        {
            return Err(MezError::forbidden("curated request namespace changed"));
        }
        self.published
            .get()
            .cloned()
            .ok_or_else(|| MezError::forbidden("curated request durable origin unpublished"))
    }
}
