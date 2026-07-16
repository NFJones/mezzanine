//! Local browser callback parsing and HTTP response handling.

use super::claims::oauth_error_message;
#[cfg(test)]
use super::login_page::{login_page_theme_tokens, write_http_response_with_tokens};
use super::pkce::parse_query;
use super::{MezError, Result};
#[cfg(test)]
use mez_mux::theme::UiTheme;
#[cfg(test)]
use std::io::Write;

/// Parses the provider callback request and validates its state token.
pub(super) fn parse_callback_request(request: &str, expected_state: &str) -> Result<String> {
    let request_line = request
        .lines()
        .next()
        .ok_or_else(|| MezError::invalid_state("OpenAI browser callback was empty"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" {
        return Err(MezError::invalid_state(
            "OpenAI browser callback used an unsupported HTTP method",
        ));
    }
    let Some((path, query)) = target.split_once('?') else {
        return Err(MezError::invalid_state(
            "OpenAI browser callback did not include authorization data",
        ));
    };
    if path != "/auth/callback" {
        return Err(MezError::invalid_state(
            "OpenAI browser callback used an unexpected path",
        ));
    }
    let query = parse_query(query)?;
    if query.get("state").map(String::as_str) != Some(expected_state) {
        return Err(MezError::invalid_state(
            "OpenAI browser callback state did not match the login request",
        ));
    }
    if let Some(error) = query.get("error") {
        let description = query.get("error_description").map(String::as_str);
        return Err(MezError::forbidden(format!(
            "OpenAI browser login failed: {}",
            oauth_error_message(error, description)
        )));
    }
    query
        .get("code")
        .filter(|code| !code.trim().is_empty())
        .cloned()
        .ok_or_else(|| MezError::invalid_state("OpenAI browser callback did not include a code"))
}

/// Runs the write http response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn write_http_response(
    stream: &mut impl Write,
    status: u16,
    body: &str,
) -> std::io::Result<()> {
    let tokens = login_page_theme_tokens(&UiTheme::default());
    write_http_response_with_tokens(stream, status, body, &tokens)
}
