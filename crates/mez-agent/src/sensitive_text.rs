//! Secret screening for hidden model-authored continuation notes.
//!
//! Hidden notes may enter durable transcript context, traces, or verbose
//! presentation. This module provides one bounded fail-closed projection so
//! those consumers never need to trust provider-authored text directly.

/// Maximum characters retained from one non-sensitive hidden continuation note.
const MAX_HIDDEN_NOTE_CHARS: usize = 2_048;

/// Returns a bounded hidden note, or drops it when it resembles secret material.
///
/// Empty notes and values containing credential markers, secret assignments,
/// private-key material, or JWT-like tokens are rejected. Callers should use
/// this projection independently at every persistence, replay, log, or display
/// boundary because internal tests and adapters may construct batches without
/// passing through MAAP parsing.
pub fn sanitize_hidden_model_note(note: &str) -> Option<String> {
    let note = note.trim();
    if note.is_empty() || hidden_note_contains_secret(note) {
        return None;
    }
    let mut sanitized = note.chars().take(MAX_HIDDEN_NOTE_CHARS).collect::<String>();
    if note.chars().count() > MAX_HIDDEN_NOTE_CHARS {
        sanitized.push_str("...");
    }
    Some(sanitized)
}

/// Reports whether free-form hidden text contains recognizable secret material.
fn hidden_note_contains_secret(note: &str) -> bool {
    if note.contains("-----BEGIN") {
        return true;
    }
    note.split_whitespace()
        .any(hidden_note_token_is_secret_like)
        || note.lines().any(hidden_note_line_assigns_secret)
}

/// Reports whether one whitespace-delimited token resembles a credential.
fn hidden_note_token_is_secret_like(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            ',' | ';' | ':' | '.' | '!' | '?' | ')' | '(' | '[' | ']' | '{' | '}' | '"' | '\''
        )
    });
    let lower = token.to_ascii_lowercase();
    lower == "bearer"
        || lower.starts_with("bearer=")
        || lower.starts_with("sk-")
        || lower.starts_with("sk_")
        || lower.starts_with("xoxb-")
        || lower.starts_with("ghp_")
        || hidden_note_token_is_jwt_like(token)
}

/// Reports whether one line assigns a value to a conventionally sensitive key.
fn hidden_note_line_assigns_secret(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "access_token",
        "refresh_token",
        "authorization",
        "client_secret",
        "password",
        "private_key",
        "secret",
        "token",
    ]
    .iter()
    .any(|key| {
        lower.contains(&format!("{key}="))
            || lower.contains(&format!("{key} ="))
            || lower.contains(&format!("{key}:"))
    })
}

/// Reports whether one token has the three base64url segments of a JWT.
fn hidden_note_token_is_jwt_like(token: &str) -> bool {
    let segments = token.split('.').collect::<Vec<_>>();
    segments.len() == 3
        && segments.iter().all(|segment| {
            segment.len() >= 8
                && segment.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        })
}

#[cfg(test)]
mod tests {
    use super::sanitize_hidden_model_note;

    #[test]
    /// Verifies hidden notes retain benign continuity while dropping common
    /// credential forms before any durable or diagnostic consumer sees them.
    fn hidden_model_note_sanitizer_drops_secrets_and_bounds_benign_text() {
        assert_eq!(
            sanitize_hidden_model_note(" Active issue: iss-42 ").as_deref(),
            Some("Active issue: iss-42")
        );
        assert!(sanitize_hidden_model_note("api_key = opaque-secret").is_none());
        assert!(sanitize_hidden_model_note("Bearer sk-project-secret").is_none());
        let long = "a".repeat(2_100);
        let sanitized = sanitize_hidden_model_note(&long).unwrap();
        assert!(sanitized.ends_with("..."));
        assert_eq!(sanitized.chars().count(), 2_051);
    }
}
