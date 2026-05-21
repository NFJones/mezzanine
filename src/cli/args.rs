//! Typed process command-line argument helpers.
//!
//! This module keeps `clap` invocation details localized at the process CLI
//! boundary. Interactive prompt languages continue to use their own parsers;
//! these helpers only parse argv supplied to the `mez` binary.

use super::{MezError, Parser, Result};

/// Parses a module-local `clap` command from the already-sliced process argv.
///
/// # Parameters
/// - `program`: The synthetic program name shown in parse diagnostics.
/// - `args`: The command arguments that follow the already-dispatched command.
pub(super) fn parse_cli_args<T>(program: &'static str, args: &[String]) -> Result<T>
where
    T: Parser,
{
    let argv = std::iter::once(program.to_string()).chain(args.iter().cloned());
    T::try_parse_from(argv).map_err(clap_error_to_invalid_args)
}

/// Converts a `clap` parse error into the repository's user-facing error type.
///
/// # Parameters
/// - `error`: The `clap` parser error to report.
pub(super) fn clap_error_to_invalid_args(error: clap::Error) -> MezError {
    MezError::invalid_args(error.to_string())
}

/// Reports whether an argv slice asks for command-local help.
///
/// # Parameters
/// - `args`: The command arguments that follow the already-dispatched command.
pub(super) fn is_cli_help_request(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("help" | "-h" | "--help")
    )
}
