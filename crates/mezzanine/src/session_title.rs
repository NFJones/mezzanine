//! Shared policy-derived session-title resolution and bounds.
//!
//! A saved-session title is distinct from the durable user-assigned
//! `NamedAgentSession` name. Every surface that mirrors a saved-session row
//! title resolves it here so the precedence order is identical everywhere:
//! manual name, then the policy-derived title, then the first prompt, then no
//! title at all.
//!
//! Objectives, prompts, and names are untrusted display data. They are bounded
//! here, collapsed to one line, and never used to authorize anything.

use crate::storage::transcript::SavedAgentSession;

/// Maximum accepted session-title length in Unicode scalar values.
///
/// This mirrors the durable session-name bound so a derived title can never
/// exceed the rules already applied to user-assigned names.
pub(crate) const MAX_SESSION_TITLE_CHARS: usize = 80;

/// Configured source used to derive a saved-session display title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SessionTitlePolicy {
    /// Derive the deterministic default display title from the objective, else
    /// the first prompt.
    ///
    /// This is the documented deterministic default, and it is currently
    /// exact: the value is the bounded objective whenever an objective exists,
    /// so a `generated` row and an `objective` row render identical text. A
    /// distinct model-generated short title depends on the separate
    /// generated-title work (c6166ea2) and is not implemented here.
    #[default]
    Generated,
    /// Mirror the published agent objective under the title bounds.
    Objective,
    /// Mirror the latest user prompt under the title bounds.
    LastPrompt,
    /// Mirror the first user prompt under the title bounds.
    FirstPrompt,
}

impl SessionTitlePolicy {
    /// Every accepted `agents.session_title_policy` value.
    pub(crate) const ALL: [Self; 4] = [
        Self::Generated,
        Self::Objective,
        Self::LastPrompt,
        Self::FirstPrompt,
    ];

    /// Returns the stable configuration representation of this policy.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::Objective => "objective",
            Self::LastPrompt => "last_prompt",
            Self::FirstPrompt => "first_prompt",
        }
    }

    /// Parses one configured policy value, rejecting unknown spellings.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|policy| policy.as_str() == value)
    }
}

/// Source that supplied one resolved session title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTitleSource {
    /// Durable user-assigned name, which always wins.
    Name,
    /// Bounded deterministic generated display title.
    Generated,
    /// Persisted mirror of the published agent objective.
    Objective,
    /// Latest user prompt from the conversation summary.
    LastPrompt,
    /// First user prompt from the conversation summary.
    FirstPrompt,
}

/// One resolved row title together with the source that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionTitle {
    /// Bounded single-line display title.
    pub(crate) text: String,
    /// Source that supplied the title.
    pub(crate) source: SessionTitleSource,
}

/// Bounds one untrusted title value for display.
///
/// Control and format characters become spaces, whitespace is collapsed, the
/// value is held to [`MAX_SESSION_TITLE_CHARS`], and an over-long value is
/// truncated at a word boundary. An empty value, or a value made only of
/// neutralized characters, yields no title.
pub(crate) fn bound_session_title(value: &str) -> Option<String> {
    let flattened = value
        .chars()
        .map(|character| {
            if is_display_format_character(character) {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let collapsed = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(truncate_title_on_word_boundary(&collapsed))
}

/// Returns whether one character is unusable in a displayed title or name.
///
/// Control characters (Cc) are unusable because they can emit terminal escapes,
/// and Unicode format characters (Cf) are unusable because they reorder or hide
/// bounded display text without being visible themselves: U+202E
/// RIGHT-TO-LEFT OVERRIDE, U+2066 LEFT-TO-RIGHT ISOLATE, and U+200B ZERO WIDTH
/// SPACE all survive a printable-only check. Titles neutralize these characters
/// and session-name validation rejects them, from this one shared predicate, so
/// the two bounds cannot drift apart.
pub(crate) fn is_display_format_character(character: char) -> bool {
    character.is_control() || is_unicode_format_character(character)
}

/// Returns whether one character belongs to the Unicode Cf category.
///
/// Rust exposes only the Cc predicate, so the Cf ranges are listed explicitly.
fn is_unicode_format_character(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

/// Truncates one collapsed title at the last word boundary within the bound.
fn truncate_title_on_word_boundary(value: &str) -> String {
    let mut characters = value.chars();
    let head = characters
        .by_ref()
        .take(MAX_SESSION_TITLE_CHARS)
        .collect::<String>();
    if characters.next().is_none() {
        return head;
    }
    match head.rfind(' ') {
        Some(boundary) if boundary > 0 => head[..boundary].trim_end().to_string(),
        _ => head.trim_end().to_string(),
    }
}

/// Resolves one row title with the shared precedence order.
///
/// A manual `name` always wins verbatim so existing named rows keep rendering
/// exactly as before. Otherwise the configured policy supplies the title, the
/// first prompt is the final fallback, and an unnamed row with no usable text
/// resolves to no title at all.
pub(crate) fn resolve_session_title(
    name: Option<&str>,
    policy: SessionTitlePolicy,
    objective: Option<&str>,
    initial_prompt: Option<&str>,
    latest_user_prompt: Option<&str>,
) -> Option<SessionTitle> {
    if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
        return Some(SessionTitle {
            text: name.to_string(),
            source: SessionTitleSource::Name,
        });
    }
    let policy_title = match policy {
        SessionTitlePolicy::Generated => bound_session_title(objective.unwrap_or_default())
            .map(|text| SessionTitle {
                text,
                source: SessionTitleSource::Generated,
            })
            .or_else(|| {
                bound_session_title(initial_prompt.unwrap_or_default()).map(|text| SessionTitle {
                    text,
                    source: SessionTitleSource::FirstPrompt,
                })
            }),
        SessionTitlePolicy::Objective => {
            bound_session_title(objective.unwrap_or_default()).map(|text| SessionTitle {
                text,
                source: SessionTitleSource::Objective,
            })
        }
        SessionTitlePolicy::LastPrompt => {
            bound_session_title(latest_user_prompt.unwrap_or_default()).map(|text| SessionTitle {
                text,
                source: SessionTitleSource::LastPrompt,
            })
        }
        SessionTitlePolicy::FirstPrompt => bound_session_title(initial_prompt.unwrap_or_default())
            .map(|text| SessionTitle {
                text,
                source: SessionTitleSource::FirstPrompt,
            }),
    };
    policy_title.or_else(|| {
        bound_session_title(initial_prompt.unwrap_or_default()).map(|text| SessionTitle {
            text,
            source: SessionTitleSource::FirstPrompt,
        })
    })
}

/// Resolves one durable saved-session row title from its merged metadata.
///
/// The persisted objective mirror is a cache of published discovery state: a
/// missing or unreadable mirror degrades to the prompt and conversation-id
/// rendering instead of failing.
pub(crate) fn resolve_saved_session_title(
    session: &SavedAgentSession,
    policy: SessionTitlePolicy,
) -> Option<SessionTitle> {
    resolve_session_title(
        session.name.as_deref(),
        policy,
        session.objective_title.as_deref(),
        session.summary.initial_prompt.as_deref(),
        session.summary.latest_user_prompt.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_SESSION_TITLE_CHARS, SessionTitlePolicy, SessionTitleSource, bound_session_title,
        is_display_format_character, resolve_session_title,
    };

    /// Verifies the manual name wins for every policy and source combination.
    #[test]
    fn manual_name_wins_over_every_policy_source() {
        for policy in SessionTitlePolicy::ALL {
            let resolved = resolve_session_title(
                Some("  operator name  "),
                policy,
                Some("Objective text"),
                Some("first prompt"),
                Some("latest prompt"),
            )
            .expect("manual name resolves a title");
            assert_eq!(resolved.text, "operator name");
            assert_eq!(resolved.source, SessionTitleSource::Name);
        }
    }

    /// Verifies each policy selects its documented source when unnamed.
    #[test]
    fn unnamed_rows_resolve_each_policy_source() {
        let cases = [
            (
                SessionTitlePolicy::Generated,
                "Objective text",
                SessionTitleSource::Generated,
            ),
            (
                SessionTitlePolicy::Objective,
                "Objective text",
                SessionTitleSource::Objective,
            ),
            (
                SessionTitlePolicy::LastPrompt,
                "latest prompt",
                SessionTitleSource::LastPrompt,
            ),
            (
                SessionTitlePolicy::FirstPrompt,
                "first prompt",
                SessionTitleSource::FirstPrompt,
            ),
        ];
        for (policy, expected, source) in cases {
            let resolved = resolve_session_title(
                None,
                policy,
                Some("Objective text"),
                Some("first prompt"),
                Some("latest prompt"),
            )
            .expect("policy resolves a title");
            assert_eq!(resolved.text, expected, "policy {}", policy.as_str());
            assert_eq!(resolved.source, source, "policy {}", policy.as_str());
        }
    }

    /// Verifies a missing policy source falls back to the first prompt.
    #[test]
    fn missing_policy_source_falls_back_to_the_first_prompt() {
        for policy in [
            SessionTitlePolicy::Generated,
            SessionTitlePolicy::Objective,
            SessionTitlePolicy::LastPrompt,
        ] {
            let resolved = resolve_session_title(None, policy, None, Some("first prompt"), None)
                .expect("first prompt is the final fallback");
            assert_eq!(resolved.text, "first prompt");
            assert_eq!(resolved.source, SessionTitleSource::FirstPrompt);
        }
    }

    /// Verifies an unnamed conversation with no usable text has no title.
    #[test]
    fn unnamed_row_without_text_has_no_title() {
        for policy in SessionTitlePolicy::ALL {
            assert_eq!(resolve_session_title(None, policy, None, None, None), None);
            assert_eq!(
                resolve_session_title(Some("   "), policy, None, None, None),
                None
            );
        }
    }

    /// Verifies whitespace and control characters collapse to one safe line.
    #[test]
    fn title_bounds_collapse_whitespace_and_control_characters() {
        assert_eq!(
            bound_session_title("  Inspect\tthe\nbacklog \u{7} now  "),
            Some("Inspect the backlog now".to_string())
        );
        assert_eq!(bound_session_title("\u{7}\u{7}"), None);
        assert_eq!(bound_session_title("   "), None);
    }

    /// Verifies an over-long title truncates at a word boundary inside the bound.
    #[test]
    fn title_bounds_truncate_on_a_word_boundary() {
        let words = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike";
        let bounded = bound_session_title(words).expect("long title bounds");
        assert!(bounded.chars().count() <= MAX_SESSION_TITLE_CHARS);
        assert!(bounded.starts_with("alpha bravo"));
        assert!(!bounded.ends_with(' '));
        assert!(words.starts_with(&bounded));

        let unbroken = "x".repeat(MAX_SESSION_TITLE_CHARS * 2);
        let bounded = bound_session_title(&unbroken).expect("unbroken title bounds");
        assert_eq!(bounded.chars().count(), MAX_SESSION_TITLE_CHARS);
    }

    /// Verifies format (Cf) characters cannot reach a rendered title.
    #[test]
    fn title_bounds_neutralize_format_characters() {
        assert!(is_display_format_character('\u{202e}'));
        assert!(is_display_format_character('\u{200b}'));
        assert!(is_display_format_character('\u{2066}'));
        assert!(is_display_format_character('\u{7}'));
        assert!(!is_display_format_character('a'));

        for value in [
            "Inspect\u{202e}the backlog",
            "Inspect\u{200b}the backlog",
            "Inspect\u{2066}the backlog",
        ] {
            let bounded = bound_session_title(value).expect("format characters bound");
            assert_eq!(bounded, "Inspect the backlog", "{value:?}");
            assert!(
                !bounded.chars().any(is_display_format_character),
                "{bounded:?}"
            );
        }

        assert_eq!(bound_session_title("\u{202e}\u{200b}"), None);
    }

    /// Verifies `generated` is exactly the bounded objective, so the documented
    /// deterministic default is precise until model-generated titles exist.
    #[test]
    fn generated_policy_matches_the_objective_policy_bounds() {
        for objective in ["Objective text", "  objective\ttext  "] {
            let generated = resolve_session_title(
                None,
                SessionTitlePolicy::Generated,
                Some(objective),
                Some("first prompt"),
                None,
            )
            .expect("generated title");
            let mirrored = resolve_session_title(
                None,
                SessionTitlePolicy::Objective,
                Some(objective),
                Some("first prompt"),
                None,
            )
            .expect("objective title");
            assert_eq!(generated.text, mirrored.text);
            assert_eq!(generated.source, SessionTitleSource::Generated);
            assert_eq!(mirrored.source, SessionTitleSource::Objective);
        }
    }
}
