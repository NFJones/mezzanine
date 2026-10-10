//! Request origins for already-qualified curated creator helpers.
//!
//! Public selectors narrow an existing declared creator; they never mint source
//! authority. Native helper observation runs first on the existing bounded
//! worker, then this capture reobserves the retained creator and qualifies its
//! actual cwd. Actor settlement must still fence original root/source/identity
//! and a one-shot published durable origin before returning readiness. No
//! expense, lease, presentation, namespace allocation or general RPC occurs.

use super::*;
use crate::storage::token_usage::{
    AccountingProjectRecord, ExternalRequestOrigin, TokenUsageStore, accounting_origin_for_root,
};

/// Frozen bounded request metadata, with worker-only publication evidence.
#[derive(Debug, Clone)]
pub(super) struct HelperUsageBegin {
    pub(super) owner: String,
    marker: String,
    producer: ProducerEvidence,
    harness: String,
    trust: Option<Arc<crate::security::project::ProjectTrustStore>>,
    inventory: Option<Arc<Vec<AccountingProjectRecord>>>,
    store: TokenUsageStore,
    now: u64,
    published: Arc<OnceLock<ExternalRequestOrigin>>,
}

impl HelperUsageBegin {
    /// Captures only an existing durably mapped declared parent namespace;
    /// unsupported sources and malformed request selectors fail before work.
    pub(super) fn capture(
        service: &RuntimeSessionService,
        binding: &LaunchBinding,
        marker: String,
    ) -> Result<Self> {
        let source = binding
            .enrollment
            .as_ref()
            .filter(|source| {
                source.usage_owner_is_durable
                    && source.curated_observer.is_some()
                    && matches!(source.producer, ProducerEvidence::VerifiedParent(_))
            })
            .ok_or_else(|| MezError::forbidden("curated request source unavailable"))?;
        if binding.harness != "claude"
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

    /// Invoked only after this work's actual native direct-parent ancestry was
    /// published. The helper cwd/session hints never select expense origin.
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
        let cwd = mez_mux::process::current_working_directory_for_pid(identity.process_id)
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
