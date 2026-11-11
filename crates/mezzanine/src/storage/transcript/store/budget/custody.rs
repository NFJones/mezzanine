//! Conversation-local lifetime custody for cooperating native budget bindings.
//!
//! The pinned private inode is never unlinked or replaced by this protocol.
//! Every live binding and issued worker retains a clone; only last-close releases
//! the kernel lock. Acquiring that SAME inode proves those original holders have
//! retired, not that remote billing or native effects completed. Durable intent
//! brackets the existing SQLite owner CAS. No age, PID, checkpoint label or saved
//! JSON alone grants liveness, teardown or inference authority. All I/O is worker
//! owned; admission is nonblocking and unrelated conversations remain independent.

use super::*;
use std::os::unix::fs::OpenOptionsExt;

const LOCK_FILE: &str = "budget-binding.lock";
const STAGE_FILE: &str = ".budget-binding-v2.json.tmp";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LockIdentity {
    device: u64,
    inode: u64,
}

/// Durable transfer intent. `database_owner` is the exact pre-CAS observation;
/// either it or `owner` is possible until `bound` is durably acknowledged. A
/// completed record requires the current DB owner, never absent replacement data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeMirror {
    version: u64,
    owner: String,
    membership: BudgetMembership,
    lock: LockIdentity,
    database_owner: Option<String>,
    bound: bool,
}

/// Actual native lifetime custody, not deserializable and not constructible from
/// public identifiers. Cloning retains the same open-file description and lock.
#[derive(Debug, Clone)]
pub(crate) struct BudgetBindingLease(Arc<Lease>);

#[derive(Debug)]
struct Lease {
    file: std_fs::File,
    path: PathBuf,
    member: BudgetMembership,
    owner: String,
    identity: LockIdentity,
}

impl BudgetBindingLease {
    /// Pure subject check suitable for the actor. It is not a disk revalidation
    /// or reusable inference permit; worker publication checks the inode again.
    pub(crate) fn matches(&self, member: &BudgetMembership, owner: &str) -> bool {
        self.0.member == *member && self.0.owner == owner
    }

    /// Revalidates the retained pinned inode against its current private path.
    /// Missing/replaced lock files fail closed instead of creating another lock.
    pub(crate) fn revalidate(&self) -> Result<()> {
        let owned = self.0.file.metadata()?;
        validate_lock_file(&owned)?;
        let current = std_fs::symlink_metadata(&self.0.path)?;
        validate_lock_file(&current)?;
        if identity(&owned) != self.0.identity || identity(&current) != self.0.identity {
            return Err(MezError::conflict("budget lifetime lock identity changed"));
        }
        Ok(())
    }
}

fn identity(metadata: &std_fs::Metadata) -> LockIdentity {
    LockIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn validate_lock_file(metadata: &std_fs::Metadata) -> Result<()> {
    if !metadata.is_file()
        || metadata.len() != 0
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(MezError::invalid_state(
            "budget lifetime lock must be an owned private empty single-link file",
        ));
    }
    Ok(())
}

fn validate_owner(owner: &str) -> Result<()> {
    mez_agent::token_budget::BudgetReceiptId::new(owner)
        .map(|_| ())
        .map_err(|_| MezError::invalid_args("invalid native budget binding owner"))
}

fn read_native(path: &Path) -> Result<Option<NativeMirror>> {
    let Some(bytes) = read_binding_bytes(path)? else {
        return Ok(None);
    };
    let mirror: NativeMirror = serde_json::from_slice(&bytes).map_err(|_| {
        MezError::invalid_state("native budget binding requires valid version-two custody")
    })?;
    if mirror.version != 2 {
        return Err(MezError::invalid_state(
            "unsupported native budget binding version",
        ));
    }
    mirror.membership.validate()?;
    validate_owner(&mirror.owner)?;
    if let Some(owner) = &mirror.database_owner {
        validate_owner(owner)?;
    }
    if mirror.bound && mirror.database_owner.as_deref() != Some(&mirror.owner) {
        return Err(MezError::conflict(
            "native budget binding completion identity mismatch",
        ));
    }
    Ok(Some(mirror))
}

/// Atomic private publication with one bounded exact residue slot. Only a fully
/// decoded identical residue can complete a previous uncertain publication.
fn write_native(directory: &Path, expected: &NativeMirror) -> Result<()> {
    let bytes = serde_json::to_vec(expected)
        .map_err(|_| MezError::invalid_state("native budget binding encoding failed"))?;
    if bytes.len() > 4096 {
        return Err(MezError::invalid_args(
            "native budget binding exceeds bound",
        ));
    }
    let stage = directory.join(STAGE_FILE);
    let file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK)
                .bits() as i32,
        )
        .open(&stage)
    {
        Ok(mut file) => {
            // On failure the exact original residue is retained; no arbitrary
            // partial/foreign staging path is deleted as a repair strategy.
            file.write_all(&bytes)?;
            file.sync_all()?;
            file
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_native(&stage)?.as_ref() != Some(expected) {
                return Err(MezError::conflict(
                    "native budget binding staging record changed",
                ));
            }
            let descriptor = rustix::fs::open(
                &stage,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?;
            let file = std_fs::File::from(descriptor);
            file.sync_all()?;
            file
        }
        Err(error) => return Err(error.into()),
    };
    let current = std_fs::symlink_metadata(&stage)?;
    let pinned = file.metadata()?;
    if !current.is_file() || current.nlink() != 1 || identity(&current) != identity(&pinned) {
        return Err(MezError::conflict(
            "native budget binding stage inode changed",
        ));
    }
    std_fs::rename(&stage, directory.join(BINDING_FILE))?;
    std_fs::File::open(directory)?.sync_all()?;
    Ok(())
}

impl AgentTranscriptStore {
    /// Caller holds the conversation lock before the active-checkpoint lock.
    /// A plain metadata save cannot erase or replace published accounting
    /// provenance. This checks descriptive evidence, not live custody or usage.
    pub(crate) fn validate_budget_checkpoint_locked(
        &self,
        record: &mez_agent::transcript::AgentSessionMetadata,
    ) -> Result<()> {
        let membership = self
            .read_conversation_metadata_locked(&record.conversation_id)?
            .budget_membership;
        match (membership.as_ref(), record.budget_reference.as_ref()) {
            (None, None) => Ok(()),
            // Publication may finish before the actor receives its outcome.
            // That actor cannot invent an acknowledgement. The checkpoint
            // owner separately forbids dropping a previously captured reference;
            // restore refuses this unresolved interval until exact recovery.
            (Some(_), None) => Ok(()),
            (Some(member), Some(reference)) => {
                let directory = self.session_dir_for(&record.conversation_id)?;
                let mirror = read_native(&directory.join(BINDING_FILE))?.ok_or_else(|| {
                    MezError::conflict("budget checkpoint binding mirror missing")
                })?;
                if member.namespace != reference.namespace.as_str()
                    || member.root != reference.root
                    || member.conversation != reference.conversation
                    || member.operation != reference.operation.as_str()
                    || !mirror.bound
                    || mirror.membership != *member
                    || mirror.owner != reference.owner.as_str()
                {
                    return Err(MezError::conflict("budget checkpoint provenance changed"));
                }
                Ok(())
            }
            _ => Err(MezError::conflict(
                "budget checkpoint provenance missing or unexpected",
            )),
        }
    }

    /// Reacquires the exact completed original binding without rewriting any
    /// record. Used by restore preflight before changing a live pane. A saved
    /// owner alone is never authority: the original private inode must be free,
    /// or the invoking pane must retain that exact descriptor. Both paths
    /// revalidate the completed mirror, immutable membership and pinned inode.
    pub(crate) fn reacquire_native_budget_binding(
        &self,
        reference: &mez_agent::token_budget::BudgetCheckpointReference,
        retained: Option<&BudgetBindingLease>,
    ) -> Result<(BudgetMembership, BudgetBindingLease)> {
        reference.validate()?;
        let _conversation = self.acquire_conversation_lock(&reference.conversation)?;
        let directory = self.session_dir_for(&reference.conversation)?;
        let mirror = read_native(&directory.join(BINDING_FILE))?
            .ok_or_else(|| MezError::conflict("budget restore binding mirror missing"))?;
        let member = &mirror.membership;
        if !mirror.bound
            || member.namespace != reference.namespace.as_str()
            || member.root != reference.root
            || member.conversation != reference.conversation
            || member.operation != reference.operation.as_str()
            || mirror.owner != reference.owner.as_str()
            || self
                .read_conversation_metadata_locked(&reference.conversation)?
                .budget_membership
                .as_ref()
                != Some(member)
        {
            return Err(MezError::conflict("budget restore provenance mismatch"));
        }
        let path = directory.join(LOCK_FILE);
        if let Some(retained) = retained {
            if !retained.matches(member, &mirror.owner)
                || retained.0.path != path
                || retained.0.identity != mirror.lock
            {
                return Err(MezError::conflict(
                    "budget restore retained custody changed",
                ));
            }
            retained.revalidate()?;
            return Ok((mirror.membership, retained.clone()));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC)
                    .bits() as i32,
            )
            .open(&path)?;
        let metadata = file.metadata()?;
        validate_lock_file(&metadata)?;
        if identity(&metadata) != mirror.lock {
            return Err(MezError::conflict("budget restore lifetime inode changed"));
        }
        flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            if error == rustix::io::Errno::WOULDBLOCK {
                MezError::conflict("budget restore original binding still live")
            } else {
                std::io::Error::from(error).into()
            }
        })?;
        let lease = BudgetBindingLease(Arc::new(Lease {
            file,
            path,
            member: member.clone(),
            owner: mirror.owner.clone(),
            identity: mirror.lock,
        }));
        lease.revalidate()?;
        Ok((mirror.membership, lease))
    }

    /// Acquires or revalidates native lifetime custody and durably publishes its
    /// transfer intent BEFORE SQLite changes owner. Legacy or missing expected
    /// custody refuses; original holders must release the same pinned inode.
    pub(crate) fn prepare_native_budget_binding(
        &self,
        member: &BudgetMembership,
        owner: &str,
        database_owner: Option<&str>,
        active: bool,
        retained: Option<&BudgetBindingLease>,
    ) -> Result<BudgetBindingLease> {
        member.validate()?;
        validate_owner(owner)?;
        let _conversation = self.acquire_conversation_lock(&member.conversation)?;
        if self
            .read_conversation_metadata_locked(&member.conversation)?
            .budget_membership
            .as_ref()
            != Some(member)
        {
            return Err(MezError::conflict(
                "native budget binding membership mismatch",
            ));
        }
        let directory = self.session_dir_for(&member.conversation)?;
        let prior = read_native(&directory.join(BINDING_FILE))?;
        if let Some(prior) = &prior {
            if prior.membership != *member
                || !(database_owner == Some(prior.owner.as_str())
                    || (!prior.bound && database_owner == prior.database_owner.as_deref()))
                || (active && database_owner.is_none())
            {
                return Err(MezError::conflict(
                    "native budget binding original database custody changed",
                ));
            }
        } else if active || database_owner.is_some() {
            return Err(MezError::conflict(
                "native budget binding original custody is missing",
            ));
        }
        let lease = if let Some(retained) = retained {
            if !retained.matches(member, owner) {
                return Err(MezError::conflict(
                    "native budget binding retained owner changed",
                ));
            }
            retained.revalidate()?;
            if prior
                .as_ref()
                .is_none_or(|prior| prior.lock != retained.0.identity || prior.owner != owner)
            {
                return Err(MezError::conflict(
                    "native budget binding retained mirror changed",
                ));
            }
            retained.clone()
        } else {
            let path = directory.join(LOCK_FILE);
            let mut options = OpenOptions::new();
            options.read(true).write(true).mode(0o600).custom_flags(
                (rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC)
                    .bits() as i32,
            );
            let file = if prior.is_some() {
                options.open(&path)?
            } else {
                match options.create_new(true).open(&path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        options.create_new(false).open(&path)?
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            let metadata = file.metadata()?;
            validate_lock_file(&metadata)?;
            let lock = identity(&metadata);
            if prior.as_ref().is_some_and(|prior| prior.lock != lock) {
                return Err(MezError::conflict(
                    "native budget lifetime lock was replaced",
                ));
            }
            flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
                if error == rustix::io::Errno::WOULDBLOCK {
                    MezError::conflict("original native budget binding still has live holders")
                } else {
                    std::io::Error::from(error).into()
                }
            })?;
            let lease = BudgetBindingLease(Arc::new(Lease {
                file,
                path,
                member: member.clone(),
                owner: owner.to_owned(),
                identity: lock,
            }));
            lease.revalidate()?;
            lease.0.file.sync_all()?;
            std_fs::File::open(&directory)?.sync_all()?;
            lease
        };
        // A final record may have been fully synced before rename failed. Its
        // exact intent and committed DB owner prove which publication can
        // finish. Never rewrite it back to a pre-CAS intent or delete residue.
        if let Some(original) = &prior {
            let mut completed = original.clone();
            completed.bound = true;
            completed.database_owner = Some(completed.owner.clone());
            if !original.bound
                && database_owner == Some(completed.owner.as_str())
                && read_native(&directory.join(STAGE_FILE))?.as_ref() == Some(&completed)
            {
                write_native(&directory, &completed)?;
            }
        }
        let intent = NativeMirror {
            version: 2,
            owner: owner.to_owned(),
            membership: member.clone(),
            lock: lease.0.identity,
            database_owner: database_owner.map(str::to_owned),
            bound: false,
        };
        if !prior
            .as_ref()
            .is_some_and(|prior| prior.owner == owner && prior.bound)
        {
            write_native(&directory, &intent)?;
        }
        Ok(lease)
    }

    /// Completes the exact intent after the owner's SQLite CAS. Lost replies
    /// recover this same membership/expense, never an autonomous execution.
    pub(crate) fn complete_native_budget_binding(&self, lease: &BudgetBindingLease) -> Result<()> {
        let _conversation = self.acquire_conversation_lock(&lease.0.member.conversation)?;
        lease.revalidate()?;
        if self
            .read_conversation_metadata_locked(&lease.0.member.conversation)?
            .budget_membership
            .as_ref()
            != Some(&lease.0.member)
        {
            return Err(MezError::conflict(
                "native budget completion membership changed",
            ));
        }
        let directory = self.session_dir_for(&lease.0.member.conversation)?;
        let mut mirror = read_native(&directory.join(BINDING_FILE))?
            .ok_or_else(|| MezError::conflict("native budget completion intent missing"))?;
        if mirror.membership != lease.0.member
            || mirror.owner != lease.0.owner
            || mirror.lock != lease.0.identity
        {
            return Err(MezError::conflict(
                "native budget completion intent changed",
            ));
        }
        if !mirror.bound {
            mirror.bound = true;
            mirror.database_owner = Some(mirror.owner.clone());
            write_native(&directory, &mirror)?;
        }
        std_fs::File::open(directory.join(BINDING_FILE))?.sync_all()?;
        std_fs::File::open(directory)?.sync_all()?;
        Ok(())
    }
}
