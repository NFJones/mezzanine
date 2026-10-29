//! Trusted native filesystem primitives, independent of shell and OS sandboxes.
//!
//! Resolver evidence describes physical objects but is not a mutation grant.
//! The actor retains trust, permission, planning and commit authorization.
//! Descriptor-relative access prevents symlink redirection; callers must still
//! revalidate authority and object identity before publishing mutations.

#[allow(
    dead_code,
    reason = "native filesystem primitives await the dependent patch dispatcher"
)]
pub(crate) mod capability;
mod identity;
mod instructions;
pub(crate) use instructions::read_scoped_instruction_file;
mod resolution;

pub(crate) use identity::stat_device;
pub(crate) use resolution::host_resolved_path_scopes;
pub(crate) use resolution::resolve_host_path;

#[cfg(test)]
mod tests;
