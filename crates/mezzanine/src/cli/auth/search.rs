//! Nested `mez auth search` local credential management.
//!
//! This owner never imports model/provider-default persistence or an HTTP client.
//! Secret input is masked or explicitly read from bounded stdin/private import;
//! there is no literal token argument. Store work runs on a blocking thread and
//! status reports local metadata rather than remotely authenticated state.

use std::{
    io::{Read, Write},
    path::PathBuf,
};

use secrecy::SecretString;

use super::super::{CliOutputFormat, write_json_or_plain};
use crate::{
    error::{MezError, Result},
    security::auth::search::{SearchAuthStore, SearchProvider, validate_name},
};

/// Existing auth-family child with required, explicit local operation.
#[derive(Debug, Clone, clap::Args)]
pub(super) struct SearchCliArgs {
    /// Local login, status, or exact-identity logout.
    #[command(subcommand)]
    command: SearchCommand,
}

/// No command performs a provider HTTP request or updates search selection.
#[derive(Debug, Clone, clap::Subcommand)]
enum SearchCommand {
    /// Stores a key locally only; does not verify provider acceptance or select it.
    Login(SearchLogin),
    /// Reports local metadata/presence without loading any credential value.
    Status(SearchName),
    /// Revokes only this search identity and deletes its exact stored records.
    Logout(SearchName),
}

/// Exact logical identity, not a provider/profile alias or credential-store URI.
#[derive(Debug, Clone, clap::Args)]
struct SearchName {
    /// Bounded lowercase identity, such as `work` (not `search-auth:work`).
    name: String,
}

/// Non-secret login arguments. A token must never appear in process argv.
#[derive(Debug, Clone, clap::Args)]
struct SearchLogin {
    /// Bounded lowercase search credential identity.
    name: String,
    /// Provider with a fixed code-owned HTTPS origin.
    #[arg(long, value_enum)]
    provider: SearchProvider,
    /// Reads one key from stdin, optionally terminated by LF or CRLF.
    #[arg(long, conflicts_with = "api_key_file")]
    stdin: bool,
    /// Imports a key from an explicit file; never an internal store reference.
    #[arg(long, conflicts_with = "stdin")]
    api_key_file: Option<PathBuf>,
    /// Existing OS or private-file store (default: prefer OS, else private file).
    #[arg(long, value_parser = ["os", "file"])]
    credential_store: Option<String>,
}

/// Executes local storage/input outside runtime actor threads. The only output
/// is safe status or logical logout metadata; no internal reference is exposed.
pub(super) async fn run<W: Write>(
    args: SearchCliArgs,
    store: SearchAuthStore,
    interactive: bool,
    output: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let value = super::run_auth_store_operation(move || match args.command {
        SearchCommand::Status(args) => serde_json::to_value(store.status(&args.name)?).map_err(|_| MezError::invalid_state("search auth status encoding failed")),
        SearchCommand::Logout(args) => Ok(serde_json::json!({ "logged_out": store.logout(&args.name)?, "remotely_verified": false })),
        SearchCommand::Login(args) => {
            validate_name(&args.name)?;
            let secret = read_input(&args, interactive)?;
            serde_json::to_value(store.login(&args.name, args.provider, &secret, args.credential_store.as_deref())?).map_err(|_| MezError::invalid_state("search auth status encoding failed"))
        }
    }).await?;
    let json = serde_json::to_string(&value)
        .map_err(|_| MezError::invalid_state("search auth output encoding failed"))?;
    write_json_or_plain(stdout, output, &json)
}

/// Reads only the explicit input source. Interactive fallback is masked and
/// noninteractive invocation must explicitly choose stdin or a file.
fn read_input(args: &SearchLogin, interactive: bool) -> Result<SecretString> {
    read_selected(
        args,
        interactive,
        || read_bounded(std::io::stdin().lock()),
        |path| {
            let file = std::fs::File::open(path)
                .map_err(|_| MezError::invalid_args("search credential import file unavailable"))?;
            read_bounded(file)
        },
        || {
            rpassword::prompt_password("Search API key (stored locally, not remotely verified): ")
                .map(SecretString::from)
                .map_err(|_| MezError::invalid_state("search credential masked input failed"))
        },
    )
}

/// Keeps source selection testable with named, lazy input readers; it never
/// falls back to another source after an explicitly selected reader fails.
fn read_selected(
    args: &SearchLogin,
    interactive: bool,
    stdin: impl FnOnce() -> Result<SecretString>,
    import: impl FnOnce(&std::path::Path) -> Result<SecretString>,
    masked: impl FnOnce() -> Result<SecretString>,
) -> Result<SecretString> {
    if args.stdin {
        return stdin();
    }
    if let Some(path) = &args.api_key_file {
        return import(path);
    }
    if interactive {
        return masked();
    }
    Err(MezError::invalid_args(
        "search login requires --stdin, --api-key-file PATH, or an interactive masked prompt; never pass a key in argv",
    ))
}

/// Bounds import size and accepts one conventional terminal newline only.
/// Temporary import text is zeroized on success and on every error path.
fn read_bounded(reader: impl Read) -> Result<SecretString> {
    let mut value = zeroize::Zeroizing::new(String::new());
    reader
        .take(8195)
        .read_to_string(&mut value)
        .map_err(|_| MezError::invalid_args("search credential input unavailable or invalid"))?;
    if value.len() > 8194 {
        return Err(MezError::invalid_args("search credential input too large"));
    }
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    Ok(SecretString::from(value.as_str()))
}

/// Synthetic input regressions; no process stdin, HOME, or keyring access.
#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    /// The existing nested command grammar advertises exactly local operations,
    /// rejects literal token flags and conflicting sources, and covers help for
    /// every new command without constructing a runtime or credential backend.
    #[test]
    fn search_auth_command_coverage_and_input_grammar() {
        use clap::{CommandFactory, Parser};
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            args: SearchCliArgs,
        }
        let mut command = Cli::command();
        assert_eq!(
            command
                .get_subcommands()
                .map(|command| command.get_name())
                .collect::<Vec<_>>(),
            ["login", "status", "logout"]
        );
        let help = command.render_long_help().to_string();
        assert!(help.contains("login") && help.contains("status") && help.contains("logout"));
        assert!(
            Cli::try_parse_from([
                "search",
                "login",
                "work",
                "--provider",
                "brave",
                "--stdin",
                "--api-key-file",
                "synthetic-file"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "search",
                "login",
                "work",
                "--provider",
                "brave",
                "--token",
                "synthetic-secret"
            ])
            .is_err()
        );
        for operation in ["login", "status", "logout"] {
            assert_eq!(
                Cli::try_parse_from(["search", operation, "--help"])
                    .err()
                    .unwrap()
                    .kind(),
                clap::error::ErrorKind::DisplayHelp
            );
        }
    }

    /// Input selection invokes only its chosen fake reader and never falls back
    /// on failure. Missing noninteractive input must not access masked stdin.
    #[test]
    fn search_input_source_selection_is_explicit_and_lazy() {
        let mut args = SearchLogin {
            name: "work".into(),
            provider: SearchProvider::Brave,
            stdin: false,
            api_key_file: None,
            credential_store: Some("file".into()),
        };
        assert!(
            read_selected(
                &args,
                false,
                || panic!("no stdin"),
                |_| panic!("no file"),
                || panic!("no prompt")
            )
            .is_err()
        );
        assert_eq!(
            read_selected(
                &args,
                true,
                || panic!("no stdin"),
                |_| panic!("no file"),
                || Ok(SecretString::from("masked-canary"))
            )
            .unwrap()
            .expose_secret(),
            "masked-canary"
        );
        args.api_key_file = Some("synthetic-import".into());
        assert_eq!(
            read_selected(
                &args,
                true,
                || panic!("no stdin"),
                |path| {
                    assert_eq!(path, std::path::Path::new("synthetic-import"));
                    Ok(SecretString::from("file-canary"))
                },
                || panic!("no prompt")
            )
            .unwrap()
            .expose_secret(),
            "file-canary"
        );
        args.api_key_file = None;
        args.stdin = true;
        assert!(
            read_selected(
                &args,
                true,
                || Err(MezError::invalid_args("synthetic-read-failure")),
                |_| panic!("no fallback file"),
                || panic!("no fallback prompt")
            )
            .is_err()
        );
    }

    /// A single newline is an input convention, not whitespace trimming or
    /// interpolation. The raw owned token remains absent from debug output.
    #[test]
    fn search_stdin_input_is_bounded_and_redacted() {
        let secret = read_bounded(&b"synthetic-search-canary\r\n"[..]).unwrap();
        assert_eq!(secret.expose_secret(), "synthetic-search-canary");
        assert!(!format!("{secret:?}").contains("synthetic-search-canary"));
        assert!(read_bounded(&vec![b'a'; 8195][..]).is_err());
        assert!(read_bounded(&[0xff][..]).is_err());
    }
}
