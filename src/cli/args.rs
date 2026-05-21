//! Typed process command-line argument helpers.
//!
//! This module keeps `clap` invocation details localized at the process CLI
//! boundary. Interactive prompt languages continue to use their own parsers;
//! these helpers only parse argv supplied to the `mez` binary.

#[cfg(test)]
use super::{Args, MezError, Result};
#[cfg(test)]
use clap::FromArgMatches;

/// Parses a module-local `clap` argument group from an already-sliced process
/// argv.
///
/// # Parameters
/// - `program`: The synthetic program name shown in parse diagnostics.
/// - `args`: The command arguments that follow the already-dispatched command.
#[cfg(test)]
pub(super) fn parse_cli_arg_group<T>(program: &'static str, args: &[String]) -> Result<T>
where
    T: Args + FromArgMatches,
{
    let argv = std::iter::once(program.to_string()).chain(args.iter().cloned());
    let command = T::augment_args(clap::Command::new(program));
    let matches = command
        .try_get_matches_from(argv)
        .map_err(clap_error_to_invalid_args)?;
    T::from_arg_matches(&matches).map_err(clap_error_to_invalid_args)
}

/// Converts a `clap` parse error into the repository's user-facing error type.
///
/// # Parameters
/// - `error`: The `clap` parser error to report.
#[cfg(test)]
pub(super) fn clap_error_to_invalid_args(error: clap::Error) -> MezError {
    MezError::invalid_args(error.to_string())
}
