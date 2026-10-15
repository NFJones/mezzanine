//! Exact search-domain adapters over the existing credential stores.
//!
//! Backend selection is a closed enum, not a service/path template. Persisted
//! metadata cannot supply a raw file or OS reference. Inference's AuthStore
//! cannot load these sibling-file or separate-service records.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::super::{
    command_store::CommandBackedCredentialStore,
    file_store::PrivateFileCredentialStore,
    secret_service::NativeSecretServiceCredentialStore,
    types::{CredentialStore, CredentialStoreAvailability},
};
use super::{MezError, OS_SERVICE, Result};

/// Closed persisted backend identity; never deserialized as an arbitrary URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Backend {
    /// Existing private-file implementation in a disjoint sibling directory.
    File,
    /// Existing platform-native implementation in the fixed search service.
    Native,
    /// Existing helper implementation in the fixed search service.
    Helper,
}

impl Backend {
    /// Constructs one existing backend without reading a credential value.
    pub(super) fn store(self, root: &Path) -> Box<dyn CredentialStore> {
        match self {
            Self::File => Box::new(PrivateFileCredentialStore::new(
                root.join("search-auth-secrets"),
            )),
            Self::Native => Box::new(NativeSecretServiceCredentialStore::search()),
            Self::Helper => Box::new(CommandBackedCredentialStore::search()),
        }
    }

    /// Reconstructs the only reference permitted for one internal account.
    pub(super) fn reference(self, root: &Path, account: &str) -> String {
        match self {
            Self::File => format!(
                "file:{}",
                root.join("search-auth-secrets")
                    .join(format!("{account}.secret"))
                    .display()
            ),
            Self::Native => format!("os-keyring:secret-service/{OS_SERVICE}/{account}"),
            Self::Helper => format!("os-keyring:secret-tool/{OS_SERVICE}/{account}"),
        }
    }

    /// Selects an explicitly requested store, or the established OS-first/file
    /// preference. Availability probing never loads a stored credential.
    pub(super) fn select(requested: Option<&str>) -> Result<Self> {
        if requested == Some("file") {
            return Ok(Self::File);
        }
        if requested.is_some_and(|value| value != "os") {
            return Err(MezError::invalid_args("invalid search credential store"));
        }
        if matches!(
            NativeSecretServiceCredentialStore::search().availability(),
            CredentialStoreAvailability::Available
        ) {
            return Ok(Self::Native);
        }
        if matches!(
            CommandBackedCredentialStore::search().availability(),
            Ok(CredentialStoreAvailability::Available)
        ) {
            return Ok(Self::Helper);
        }
        if requested == Some("os") {
            Err(MezError::invalid_state(
                "search credential store unavailable",
            ))
        } else {
            Ok(Self::File)
        }
    }
}

/// Fake runner coverage of the existing helper adapter, never a real keyring.
#[cfg(test)]
mod tests {
    use super::super::super::command_store::{
        CommandCredentialBackend, CredentialCommandOutput, CredentialCommandRunner,
    };
    use super::*;
    use secrecy::SecretString;
    use std::{cell::RefCell, rc::Rc};

    /// Journal shared by cloned synthetic helper runners.
    #[derive(Clone, Default)]
    struct Runner {
        state: Rc<RefCell<State>>,
    }

    /// Lookup is deliberately unsuccessful, as for an already-absent record.
    #[derive(Default)]
    struct State {
        calls: Vec<Vec<String>>,
        fail_clear: bool,
        fail_availability: bool,
    }

    impl CredentialCommandRunner for Runner {
        fn command_available(&self, _: &str) -> bool {
            true
        }
        fn run_command(
            &self,
            _: &str,
            args: &[String],
            _: Option<&str>,
        ) -> Result<CredentialCommandOutput> {
            let mut state = self.state.borrow_mut();
            state.calls.push(args.to_vec());
            Ok(match args[0].as_str() {
                "lookup" => CredentialCommandOutput::failure(),
                "search" if state.fail_availability => CredentialCommandOutput::failure(),
                "clear" if state.fail_clear => CredentialCommandOutput::failure(),
                _ => CredentialCommandOutput::success(Vec::new()),
            })
        }
    }

    /// Search helper operations use the fixed service attributes; forged
    /// inference/MCP/lookalike refs fail before any command. Missing lookup is
    /// not swallowed as absence; deletion instead invokes exact idempotent clear
    /// without a secret lookup. Unsuccessful clear/availability remain errors.
    #[test]
    fn search_helper_namespace_and_cleanup_use_existing_adapter_without_lookup() {
        let runner = Runner::default();
        let search = CommandBackedCredentialStore::new(
            CommandCredentialBackend::SecretTool {
                executable: "fake-secret-tool".into(),
                service: OS_SERVICE.into(),
            },
            runner.clone(),
        );
        let reference = search
            .store_secret(
                "search-work",
                &SecretString::from("synthetic-helper-canary"),
            )
            .unwrap();
        assert_eq!(
            reference,
            "os-keyring:secret-tool/mez-search-auth/search-work"
        );
        assert!(runner.state.borrow().calls.iter().all(|args| {
            args.windows(2)
                .any(|pair| pair == ["application", OS_SERVICE])
        }));
        let before = runner.state.borrow().calls.len();
        for invalid in [
            "os-keyring:secret-tool/mezzanine/search-work",
            "os-keyring:secret-tool/mez-search-auth.evil/search-work",
            "os-keyring:secret-tool/mez-search-auth/../work",
            "os-keyring:secret-service/mez-search-auth/search-work",
        ] {
            assert!(search.load_secret(invalid).is_err());
            assert!(search.delete_secret(invalid).is_err());
        }
        assert_eq!(runner.state.borrow().calls.len(), before);
        assert!(search.load_secret(&reference).is_err());
        runner.state.borrow_mut().calls.clear();
        assert!(search.delete_secret(&reference).unwrap());
        assert!(search.delete_secret(&reference).unwrap());
        assert!(
            runner
                .state
                .borrow()
                .calls
                .iter()
                .all(|args| args[0] != "lookup")
        );
        runner.state.borrow_mut().fail_clear = true;
        assert!(search.delete_secret(&reference).is_err());
        runner.state.borrow_mut().fail_clear = false;
        runner.state.borrow_mut().fail_availability = true;
        assert!(search.delete_secret(&reference).is_err());
        let inference = CommandBackedCredentialStore::secret_tool_with_runner(runner.clone());
        let before = runner.state.borrow().calls.len();
        assert!(inference.load_secret(&reference).is_err());
        assert!(inference.delete_secret(&reference).is_err());
        assert_eq!(runner.state.borrow().calls.len(), before);
    }
}
