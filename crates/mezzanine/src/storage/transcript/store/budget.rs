//! Exact accounting membership publication under the existing conversation lock.
//! The metadata reference and binding companion are distinct durable writes, not
//! a cross-store transaction or native liveness authority. Workers must keep their
//! original operation through partial failure; none of these methods runs a turn.

use super::*;
use crate::storage::token_usage::budget::membership::BudgetMembership;

const BINDING_FILE: &str = "budget-binding.json";

/// Version-one active checkpoint companion, qualified by original runtime owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingMirror {
    version: u64,
    owner: String,
    membership: BudgetMembership,
}

impl AgentTranscriptStore {
    /// Checks an already-published mirror without creating replacement evidence.
    /// False is absence only; foreign, future or corrupt data always refuses.
    pub(crate) fn has_budget_binding_mirror(
        &self,
        member: &BudgetMembership,
        owner: &str,
    ) -> Result<bool> {
        member.validate()?;
        let _lease = self.acquire_conversation_lock(&member.conversation)?;
        let path = self
            .session_dir_for(&member.conversation)?
            .join(BINDING_FILE);
        let Some(actual) = read_binding(&path)? else {
            return Ok(false);
        };
        if actual
            != (BindingMirror {
                version: 1,
                owner: owner.into(),
                membership: member.clone(),
            })
        {
            return Err(MezError::conflict("budget binding mirror changed"));
        }
        Ok(true)
    }

    /// Reads exact immutable provenance. Legacy absence is unknown, not a root.
    pub(crate) fn conversation_budget_membership(
        &self,
        conversation: &str,
    ) -> Result<Option<BudgetMembership>> {
        validate_conversation_id(conversation)?;
        let _lease = self.acquire_conversation_lock(conversation)?;
        Ok(self
            .read_conversation_metadata_locked(conversation)?
            .budget_membership)
    }

    /// Publishes one immutable reference, retaining unrelated sidecar settings.
    /// After this succeeds, even an unrelated later metadata write must preserve
    /// it. A changed root/namespace/operation is never an update or repair.
    pub(crate) fn publish_conversation_budget_membership(
        &self,
        member: &BudgetMembership,
    ) -> Result<()> {
        member.validate()?;
        let _lease = self.acquire_conversation_lock(&member.conversation)?;
        let mut metadata = self.read_conversation_metadata_locked(&member.conversation)?;
        if let Some(original) = &metadata.budget_membership {
            if original != member {
                return Err(MezError::conflict("conversation budget reference changed"));
            }
        } else {
            metadata.version = SESSION_METADATA_VERSION;
            metadata.budget_membership = Some(member.clone());
            validate_conversation_metadata_contract(&metadata)?;
            self.write_conversation_metadata_locked(&member.conversation, &metadata)?;
        }
        // The existing metadata writer syncs its temporary file. This operation
        // additionally requires the published file and directory before DB phase
        // advancement, including recovery after rename-before-lost-reply.
        std_fs::File::open(self.conversation_metadata_path_for(&member.conversation)?)?
            .sync_all()?;
        std_fs::File::open(self.session_dir_for(&member.conversation)?)?.sync_all()?;
        std_fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    /// Mirrors an already database-reserved binding. Same owner/references are
    /// idempotent; another owner cannot overwrite a stale mirror by age or name.
    pub(crate) fn publish_budget_binding_mirror(
        &self,
        member: &BudgetMembership,
        owner: &str,
    ) -> Result<()> {
        member.validate()?;
        mez_agent::token_budget::BudgetReceiptId::new(owner)
            .map_err(|_| MezError::invalid_args("invalid budget binding owner"))?;
        let _lease = self.acquire_conversation_lock(&member.conversation)?;
        if self
            .read_conversation_metadata_locked(&member.conversation)?
            .budget_membership
            .as_ref()
            != Some(member)
        {
            return Err(MezError::conflict(
                "budget binding metadata reference mismatch",
            ));
        }
        let directory = self.session_dir_for(&member.conversation)?;
        let path = directory.join(BINDING_FILE);
        let expected = BindingMirror {
            version: 1,
            owner: owner.into(),
            membership: member.clone(),
        };
        match read_binding(&path)? {
            Some(actual) => {
                if actual != expected {
                    return Err(MezError::conflict("budget binding mirror changed"));
                }
            }
            None => {
                // One bounded residue slot per conversation. An interrupted
                // complete write may resume only its exact original record;
                // partial/foreign residue refuses without deletion or overwrite.
                let temporary = directory.join(".budget-binding.json.tmp");
                if let Some(prior) = read_binding(&temporary)? {
                    if prior != expected {
                        return Err(MezError::conflict(
                            "budget binding temporary record changed",
                        ));
                    }
                    std_fs::File::open(&temporary)?.sync_all()?;
                    std_fs::rename(&temporary, &path)?;
                    std_fs::File::open(&path)?.sync_all()?;
                    std_fs::File::open(&directory)?.sync_all()?;
                    return Ok(());
                }
                let mut options = OpenOptions::new();
                options.create_new(true).write(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&temporary)?;
                let result = (|| {
                    let body = serde_json::to_vec(&expected)
                        .map_err(|_| MezError::invalid_args("invalid budget binding record"))?;
                    if body.len() > 4096 {
                        return Err(MezError::invalid_args(
                            "budget binding record exceeds bound",
                        ));
                    }
                    file.write_all(&body)?;
                    file.sync_all()?;
                    std_fs::rename(&temporary, &path)?;
                    Ok(())
                })();
                drop(file);
                if temporary.exists() {
                    std_fs::remove_file(&temporary)?;
                }
                result?;
            }
        }
        std_fs::File::open(path)?.sync_all()?;
        std_fs::File::open(directory)?.sync_all()?;
        Ok(())
    }
}

/// Pin the exact regular file, reject symlinks, and bound reads even if another
/// writer grows it after metadata inspection. No private record bytes in errors.
fn read_binding(path: &Path) -> Result<Option<BindingMirror>> {
    let descriptor = match rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    let file = std_fs::File::from(descriptor);
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err(MezError::invalid_state(
            "budget binding mirror is not a bounded file",
        ));
    }
    let mut body = Vec::new();
    file.take(4097).read_to_end(&mut body)?;
    if body.len() > 4096 {
        return Err(MezError::invalid_state(
            "budget binding mirror exceeds bound",
        ));
    }
    let mirror: BindingMirror = serde_json::from_slice(&body)
        .map_err(|_| MezError::invalid_state("invalid budget binding mirror"))?;
    if mirror.version != 1 {
        return Err(MezError::invalid_state(
            "unsupported budget binding mirror version",
        ));
    }
    mirror.membership.validate()?;
    Ok(Some(mirror))
}
