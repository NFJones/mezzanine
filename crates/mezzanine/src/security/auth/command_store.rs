//! Command-backed operating-system credential store implementation.
//!
//! This backend talks to helper programs such as `secret-tool` through an
//! injectable command runner so availability and command behavior stay testable.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use secrecy::{ExposeSecret, SecretString};

use crate::error::{MezError, Result};

use super::fs::{is_executable_file, secret_from_command_stdout, validate_safe_name};
use super::types::{
    CredentialStore, CredentialStoreAvailability, CredentialStoreKind, OS_CREDENTIAL_SERVICE,
    SECRET_TOOL_BACKEND, SECRET_TOOL_PROGRAM,
};

/// Output from a credential-store helper command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialCommandOutput {
    /// Stores the success value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub success: bool,
    /// Stores the stdout value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stdout: Vec<u8>,
}

impl CredentialCommandOutput {
    /// Runs the success operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn success(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            success: true,
            stdout: stdout.into(),
        }
    }

    /// Runs the failure operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn failure() -> Self {
        Self {
            success: false,
            stdout: Vec::new(),
        }
    }
}

/// Runs credential-store helper commands behind an injectable boundary.
pub trait CredentialCommandRunner {
    /// Runs the command available operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn command_available(&self, executable: &str) -> bool;
    /// Runs the run command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn run_command(
        &self,
        executable: &str,
        args: &[String],
        stdin: Option<&str>,
    ) -> Result<CredentialCommandOutput>;
}

/// Command runner that executes helper programs on the local Unix host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemCredentialCommandRunner;

impl CredentialCommandRunner for SystemCredentialCommandRunner {
    /// Runs the command available operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn command_available(&self, executable: &str) -> bool {
        let executable_path = Path::new(executable);
        if executable_path.components().count() > 1 {
            return is_executable_file(executable_path);
        }

        let Some(paths) = std::env::var_os("PATH") else {
            return false;
        };

        std::env::split_paths(&paths).any(|directory| {
            let candidate = directory.join(executable);
            is_executable_file(&candidate)
        })
    }

    /// Runs the run command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn run_command(
        &self,
        executable: &str,
        args: &[String],
        stdin: Option<&str>,
    ) -> Result<CredentialCommandOutput> {
        let mut command = Command::new(executable);
        command
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = command.spawn()?;
        if let Some(stdin_value) = stdin
            && let Some(mut child_stdin) = child.stdin.take()
        {
            child_stdin.write_all(stdin_value.as_bytes())?;
        }

        let output = child.wait_with_output()?;
        Ok(CredentialCommandOutput {
            success: output.status.success(),
            stdout: output.stdout,
        })
    }
}

/// Supported command-backed OS credential-store backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandCredentialBackend {
    /// Represents the Secret Tool case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SecretTool {
        /// Stores the executable value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        executable: String,
        /// Stores the service value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        service: String,
    },
}

impl CommandCredentialBackend {
    /// Runs the secret tool operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn secret_tool() -> Self {
        Self::SecretTool {
            executable: SECRET_TOOL_PROGRAM.to_string(),
            service: OS_CREDENTIAL_SERVICE.to_string(),
        }
    }

    /// Runs the backend name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn backend_name(&self) -> &'static str {
        match self {
            Self::SecretTool { .. } => SECRET_TOOL_BACKEND,
        }
    }

    /// Runs the executable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn executable(&self) -> &str {
        match self {
            Self::SecretTool { executable, .. } => executable,
        }
    }

    /// Runs the service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn service(&self) -> &str {
        match self {
            Self::SecretTool { service, .. } => service,
        }
    }

    /// Runs the plan service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn plan_service(&self) -> String {
        format!("{}/{}", self.backend_name(), self.service())
    }

    /// Runs the reference for provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn reference_for_provider(&self, provider: &str) -> Result<String> {
        validate_safe_name(provider, "provider name is not credential-store safe")?;
        Ok(format!(
            "{}{}/{}/{}",
            CredentialStoreKind::OperatingSystem.reference_prefix(),
            self.backend_name(),
            self.service(),
            provider
        ))
    }

    /// Runs the provider from reference operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_from_reference(&self, reference: &str) -> Result<Option<String>> {
        let Some(body) =
            reference.strip_prefix(CredentialStoreKind::OperatingSystem.reference_prefix())
        else {
            return Ok(None);
        };

        let mut parts = body.split('/');
        let backend = parts.next();
        let service = parts.next();
        let provider = parts.next();
        if parts.next().is_some()
            || backend != Some(self.backend_name())
            || service != Some(self.service())
        {
            return Err(MezError::config(
                "unsupported operating-system credential reference",
            ));
        }

        let Some(provider) = provider else {
            return Err(MezError::config(
                "malformed operating-system credential reference",
            ));
        };
        validate_safe_name(provider, "provider name is not credential-store safe")?;
        Ok(Some(provider.to_string()))
    }

    /// Runs the availability args operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn availability_args(&self) -> Vec<String> {
        vec![
            "search".to_string(),
            "application".to_string(),
            self.service().to_string(),
            "provider".to_string(),
            "__mezzanine_availability_check__".to_string(),
        ]
    }

    /// Runs the store args operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn store_args(&self, provider: &str) -> Vec<String> {
        let mut args = vec![
            "store".to_string(),
            format!("--label=Mezzanine {provider} credential"),
        ];
        args.extend(self.attribute_args(provider));
        args
    }

    /// Runs the lookup args operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn lookup_args(&self, provider: &str) -> Vec<String> {
        let mut args = vec!["lookup".to_string()];
        args.extend(self.attribute_args(provider));
        args
    }

    /// Runs the clear args operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn clear_args(&self, provider: &str) -> Vec<String> {
        let mut args = vec!["clear".to_string()];
        args.extend(self.attribute_args(provider));
        args
    }

    /// Runs the attribute args operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn attribute_args(&self, provider: &str) -> Vec<String> {
        vec![
            "application".to_string(),
            self.service().to_string(),
            "provider".to_string(),
            provider.to_string(),
        ]
    }
}

/// Concrete OS credential store implemented through an external helper command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBackedCredentialStore<R = SystemCredentialCommandRunner> {
    /// Stores the backend value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    backend: CommandCredentialBackend,
    /// Stores the runner value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    runner: R,
}

impl CommandBackedCredentialStore<SystemCredentialCommandRunner> {
    /// Runs the secret tool operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn secret_tool() -> Self {
        Self::secret_tool_with_runner(SystemCredentialCommandRunner)
    }
}

impl<R: CredentialCommandRunner> CommandBackedCredentialStore<R> {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(backend: CommandCredentialBackend, runner: R) -> Self {
        Self { backend, runner }
    }

    /// Runs the secret tool with runner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn secret_tool_with_runner(runner: R) -> Self {
        Self::new(CommandCredentialBackend::secret_tool(), runner)
    }

    /// Runs the availability operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn availability(&self) -> Result<CredentialStoreAvailability> {
        if !self.runner.command_available(self.backend.executable()) {
            return Ok(CredentialStoreAvailability::Unavailable {
                reason: "credential-store command is not installed".to_string(),
            });
        }

        let output = self.runner.run_command(
            self.backend.executable(),
            &self.backend.availability_args(),
            None,
        )?;
        if output.success {
            Ok(CredentialStoreAvailability::Available)
        } else {
            Ok(CredentialStoreAvailability::Unavailable {
                reason: "credential-store command could not reach an OS secret service".to_string(),
            })
        }
    }

    /// Runs the ensure available operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn ensure_available(&self) -> Result<()> {
        match self.availability()? {
            CredentialStoreAvailability::Available => Ok(()),
            CredentialStoreAvailability::Unavailable { reason } => Err(MezError::invalid_state(
                format!("operating system credential store unavailable: {reason}"),
            )),
        }
    }

    /// Runs the plan service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn plan_service(&self) -> String {
        self.backend.plan_service()
    }
}

impl<R: CredentialCommandRunner> CredentialStore for CommandBackedCredentialStore<R> {
    /// Runs the kind operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn kind(&self) -> CredentialStoreKind {
        CredentialStoreKind::OperatingSystem
    }

    /// Runs the store secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn store_secret(&self, provider: &str, secret: &SecretString) -> Result<String> {
        validate_safe_name(provider, "provider name is not credential-store safe")?;
        if secret.expose_secret().is_empty() {
            return Err(MezError::invalid_args("auth secret must not be empty"));
        }

        self.ensure_available()?;
        let output = self.runner.run_command(
            self.backend.executable(),
            &self.backend.store_args(provider),
            Some(secret.expose_secret()),
        )?;
        if !output.success {
            return Err(MezError::invalid_state(
                "operating system credential store command failed",
            ));
        }

        self.backend.reference_for_provider(provider)
    }

    /// Runs the load secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn load_secret(&self, reference: &str) -> Result<Option<SecretString>> {
        let Some(provider) = self.backend.provider_from_reference(reference)? else {
            return Ok(None);
        };

        self.ensure_available()?;
        let output = self.runner.run_command(
            self.backend.executable(),
            &self.backend.lookup_args(&provider),
            None,
        )?;
        if !output.success {
            return Err(MezError::invalid_state(
                "operating system credential lookup command failed",
            ));
        }

        secret_from_command_stdout(output.stdout)
    }

    /// Runs the delete secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn delete_secret(&self, reference: &str) -> Result<bool> {
        let Some(provider) = self.backend.provider_from_reference(reference)? else {
            return Ok(false);
        };
        if self.load_secret(reference)?.is_none() {
            return Ok(false);
        }

        let output = self.runner.run_command(
            self.backend.executable(),
            &self.backend.clear_args(&provider),
            None,
        )?;
        if !output.success {
            return Err(MezError::invalid_state(
                "operating system credential delete command failed",
            ));
        }
        Ok(true)
    }
}
