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
    /// the first prompt, unless a model-generated title is already stored.
    ///
    /// Resolution under this policy is the stored generated title first, then the
    /// bounded objective, then the bounded first prompt. The stored generated
    /// title comes from the bounded side-channel request, so a conversation whose
    /// generation failed renders exactly the objective-derived fallback while the
    /// other policies never consult the stored title at all.
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

/// Stable bounded reason one generated title was rejected.
///
/// The reason is recorded for audit and diagnostics only. It never carries
/// provider text, so a rejected generation cannot leak raw model output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTitleRejection {
    /// The provider returned nothing usable after normalization.
    Empty,
    /// The provider returned a control or format character outside whitespace.
    ControlCharacters,
    /// The provider returned far more text than a display title can use.
    Oversize,
    /// The provider returned structured payload text instead of a title.
    Malformed,
}

/// Stable bounded reason one generated-title attempt produced no stored title.
///
/// The reason lives beside the sanitizer because every rejection maps here, and
/// it is the only vocabulary the trace, status, and fallback paths record: no
/// variant carries provider text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTitleFailureReason {
    /// The provider returned an error.
    ProviderError,
    /// The provider request timed out.
    Timeout,
    /// The provider returned nothing usable.
    Empty,
    /// The provider returned control or format characters outside whitespace.
    ControlCharacters,
    /// The provider returned far more text than a display title can use.
    Oversize,
    /// The provider returned structured payload text instead of a title.
    Malformed,
    /// The provider stopped because the request exhausted its output budget.
    OutputLimit,
    /// The bounded title sidecar could not be written.
    StorageUnavailable,
    /// Every allowed attempt failed, so the deterministic fallback is final.
    AttemptsExhausted,
}

impl SessionTitleFailureReason {
    /// Returns the stable bounded reason name.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderError => "provider_error",
            Self::Timeout => "timeout",
            Self::Empty => "empty",
            Self::ControlCharacters => "control_characters",
            Self::Oversize => "oversize",
            Self::Malformed => "malformed",
            Self::OutputLimit => "output_limit",
            Self::StorageUnavailable => "storage_unavailable",
            Self::AttemptsExhausted => "attempts_exhausted",
        }
    }

    /// Maps one sanitizer rejection to its stable failure reason.
    pub(crate) const fn from_rejection(rejection: SessionTitleRejection) -> Self {
        match rejection {
            SessionTitleRejection::Empty => Self::Empty,
            SessionTitleRejection::ControlCharacters => Self::ControlCharacters,
            SessionTitleRejection::Oversize => Self::Oversize,
            SessionTitleRejection::Malformed => Self::Malformed,
        }
    }
}

/// Maximum generated-title characters accepted before the value is rejected.
///
/// The display bound is [`MAX_SESSION_TITLE_CHARS`]; this generous input bound
/// exists so a model that answers with a paragraph is treated as a failed
/// generation instead of being silently truncated into a plausible title.
pub(crate) const MAX_GENERATED_SESSION_TITLE_INPUT_CHARS: usize = 512;

/// Sanitizes one raw provider title into a bounded single-line display title.
///
/// Provider output is untrusted display data. One surrounding Markdown code
/// decoration, whitespace is collapsed, control and format characters are
/// rejected, and the value is held to the shared title bounds so a generated
/// title can never exceed the rules already applied to a user-assigned session
/// name.
pub(crate) fn sanitize_generated_session_title(raw: &str) -> Result<String, SessionTitleRejection> {
    let stripped = strip_title_decoration(raw);
    if stripped
        .chars()
        .any(|character| is_display_format_character(character) && !character.is_whitespace())
    {
        return Err(SessionTitleRejection::ControlCharacters);
    }
    if stripped.chars().count() > MAX_GENERATED_SESSION_TITLE_INPUT_CHARS {
        return Err(SessionTitleRejection::Oversize);
    }
    if looks_like_structured_payload(stripped) {
        return Err(SessionTitleRejection::Malformed);
    }
    bound_session_title(stripped).ok_or(SessionTitleRejection::Empty)
}

/// Removes surrounding Markdown fences and wrapping quotes until nothing changes.
///
/// A model can wrap the same value twice, put a fence inside quotes, or answer in
/// a fence and keep talking after the closing fence. Stripping to a fixed point
/// instead of once keeps every such reply from rendering its decoration as part
/// of the title.
fn strip_title_decoration(raw: &str) -> &str {
    let mut value = raw.trim();
    loop {
        let stripped = strip_wrapping_quotes(strip_code_fence(value));
        if stripped == value {
            return stripped;
        }
        value = stripped;
    }
}

/// Removes one leading Markdown code fence and its optional closing fence.
///
/// The two markers are matched independently: a fenced reply followed by prose
/// still loses both markers, a single-line ```` ```Title``` ```` is handled without
/// a line break, and a value with no leading fence is returned unchanged.
fn strip_code_fence(value: &str) -> &str {
    let Some(rest) = value.strip_prefix("```") else {
        return value;
    };
    // An opening fence may carry an info string, so its body starts after that line.
    let body = match rest.find('\n') {
        Some(index) => &rest[index + 1..],
        None => rest,
    };
    match body.find("```") {
        Some(index) => body[..index].trim(),
        None => body.trim(),
    }
}

/// Strips one pair of wrapping quotes, including typographic and backtick pairs.
fn strip_wrapping_quotes(value: &str) -> &str {
    const WRAPPERS: [(char, char); 5] = [
        ('"', '"'),
        ('\'', '\''),
        ('`', '`'),
        ('\u{201c}', '\u{201d}'),
        ('\u{2018}', '\u{2019}'),
    ];
    for (open, close) in WRAPPERS {
        if let Some(inner) = value.strip_prefix(open)
            && let Some(inner) = inner.strip_suffix(close)
        {
            return inner.trim();
        }
    }
    value
}

/// Reports whether one response is a structured payload rather than a title.
fn looks_like_structured_payload(value: &str) -> bool {
    (value.starts_with('{') && value.ends_with('}'))
        || (value.starts_with('[') && value.ends_with(']'))
        || value.starts_with("mezzanine-action-json")
}

/// Resolves one row title with the shared precedence order.
///
/// A manual `name` always wins verbatim so existing named rows keep rendering
/// exactly as before. Otherwise the configured policy supplies the title, the
/// first prompt is the final fallback, and an unnamed row with no usable text
/// resolves to no title at all.
///
/// Under `generated` the stored model-generated title wins over the
/// objective-derived fallback, and that stored title is never consulted for any
/// other policy: switching the policy away from `generated` stops using it
/// immediately.
pub(crate) fn resolve_session_title(
    name: Option<&str>,
    policy: SessionTitlePolicy,
    generated: Option<&str>,
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
        SessionTitlePolicy::Generated => bound_session_title(generated.unwrap_or_default())
            .map(|text| SessionTitle {
                text,
                source: SessionTitleSource::Generated,
            })
            .or_else(|| {
                bound_session_title(objective.unwrap_or_default()).map(|text| SessionTitle {
                    text,
                    source: SessionTitleSource::Generated,
                })
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
        session.generated_title.as_deref(),
        session.objective_title.as_deref(),
        session.summary.initial_prompt.as_deref(),
        session.summary.latest_user_prompt.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_GENERATED_SESSION_TITLE_INPUT_CHARS, MAX_SESSION_TITLE_CHARS, SessionTitlePolicy,
        SessionTitleRejection, SessionTitleSource, bound_session_title,
        is_display_format_character, resolve_session_title, sanitize_generated_session_title,
    };

    /// Verifies the manual name wins for every policy and source combination.
    #[test]
    fn manual_name_wins_over_every_policy_source() {
        for policy in SessionTitlePolicy::ALL {
            let resolved = resolve_session_title(
                Some("  operator name  "),
                policy,
                None,
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
                None,
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
            let resolved =
                resolve_session_title(None, policy, None, None, Some("first prompt"), None)
                    .expect("first prompt is the final fallback");
            assert_eq!(resolved.text, "first prompt");
            assert_eq!(resolved.source, SessionTitleSource::FirstPrompt);
        }
    }

    /// Verifies an unnamed conversation with no usable text has no title.
    #[test]
    fn unnamed_row_without_text_has_no_title() {
        for policy in SessionTitlePolicy::ALL {
            assert_eq!(
                resolve_session_title(None, policy, None, None, None, None),
                None
            );
            assert_eq!(
                resolve_session_title(Some("   "), policy, None, None, None, None),
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

    /// Verifies a sentence-shaped title is preserved intact and an over-long
    /// sentence truncates at a word boundary inside the bound.
    #[test]
    fn title_bounds_keep_a_sentence_and_truncate_an_over_long_sentence() {
        let sentence = "Refactor the generated title path";
        assert_eq!(bound_session_title(sentence), Some(sentence.to_string()));

        let long_sentence = "Refactor the generated title path so every resolved row renders one complete sentence instead of a terse fragment";
        let bounded = bound_session_title(long_sentence).expect("long sentence bounds");
        assert!(bounded.chars().count() <= MAX_SESSION_TITLE_CHARS);
        assert!(long_sentence.starts_with(&bounded));
        assert!(!bounded.ends_with(' '));
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
                None,
                Some(objective),
                Some("first prompt"),
                None,
            )
            .expect("generated title");
            let mirrored = resolve_session_title(
                None,
                SessionTitlePolicy::Objective,
                Some("Stored generated title"),
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

    /// Verifies the sanitizer strips decoration and collapses multiline output.
    #[test]
    fn sanitizer_strips_decoration_and_collapses_multiline_output() {
        let cases = [
            ("\"Inspect the backlog\"", "Inspect the backlog"),
            ("'Inspect the backlog'", "Inspect the backlog"),
            ("\u{201c}Inspect the backlog\u{201d}", "Inspect the backlog"),
            ("\u{2018}Inspect the backlog\u{2019}", "Inspect the backlog"),
            ("`Inspect the backlog`", "Inspect the backlog"),
            ("```\nInspect the backlog\n```", "Inspect the backlog"),
            ("```text\nInspect the backlog\n```", "Inspect the backlog"),
            ("```Title```", "Title"),
            (
                "```\nInspect the backlog\n```\nHope that helps",
                "Inspect the backlog",
            ),
            ("```Title``` and that is the whole answer", "Title"),
            ("```\nInspect the backlog", "Inspect the backlog"),
            ("\"```\nInspect the backlog\n```\"", "Inspect the backlog"),
            ("\"\"Inspect the backlog\"\"", "Inspect the backlog"),
            (
                "\"\u{201c}Inspect the backlog\u{201d}\"",
                "Inspect the backlog",
            ),
            ("Inspect\nthe\nbacklog", "Inspect the backlog"),
            ("  Inspect   the  backlog  ", "Inspect the backlog"),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                sanitize_generated_session_title(raw),
                Ok(expected.to_string()),
                "{raw:?}"
            );
        }
    }

    /// Verifies an over-long accepted title truncates at a word boundary.
    #[test]
    fn sanitizer_truncates_oversize_titles_on_a_word_boundary() {
        let raw = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike";
        let sanitized = sanitize_generated_session_title(raw).expect("bounded title");
        assert!(sanitized.chars().count() <= MAX_SESSION_TITLE_CHARS);
        assert!(raw.starts_with(&sanitized));
        assert!(!sanitized.ends_with(' '));

        let unbroken = "x".repeat(MAX_SESSION_TITLE_CHARS * 2);
        let sanitized = sanitize_generated_session_title(&unbroken).expect("bounded title");
        assert_eq!(sanitized.chars().count(), MAX_SESSION_TITLE_CHARS);
    }

    /// Verifies unusable provider output is rejected with a bounded reason.
    #[test]
    fn sanitizer_rejects_unusable_output_with_a_bounded_reason() {
        let runaway = "alpha ".repeat(MAX_GENERATED_SESSION_TITLE_INPUT_CHARS);
        let cases: Vec<(&str, SessionTitleRejection)> = vec![
            ("", SessionTitleRejection::Empty),
            ("   ", SessionTitleRejection::Empty),
            ("\"\"", SessionTitleRejection::Empty),
            ("```\n```", SessionTitleRejection::Empty),
            ("\"```\"", SessionTitleRejection::Empty),
            ("\u{7}\u{7}", SessionTitleRejection::ControlCharacters),
            (
                "Inspect\u{202e}the backlog",
                SessionTitleRejection::ControlCharacters,
            ),
            (
                "Inspect\nthe\u{200b} backlog",
                SessionTitleRejection::ControlCharacters,
            ),
            (runaway.as_str(), SessionTitleRejection::Oversize),
            ("{\"title\": \"Inspect\"}", SessionTitleRejection::Malformed),
            (
                "mezzanine-action-json\n[]",
                SessionTitleRejection::Malformed,
            ),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                sanitize_generated_session_title(raw),
                Err(expected),
                "{raw:?}"
            );
        }
    }

    /// Verifies `generated` prefers a stored generated title over its fallbacks.
    #[test]
    fn generated_policy_prefers_the_stored_generated_title() {
        let resolved = resolve_session_title(
            None,
            SessionTitlePolicy::Generated,
            Some("Generated title"),
            Some("Objective text"),
            Some("first prompt"),
            Some("latest prompt"),
        )
        .expect("stored generated title resolves");
        assert_eq!(resolved.text, "Generated title");
        assert_eq!(resolved.source, SessionTitleSource::Generated);

        let objective_fallback = resolve_session_title(
            None,
            SessionTitlePolicy::Generated,
            Some("\u{7}"),
            Some("Objective text"),
            Some("first prompt"),
            None,
        )
        .expect("objective fallback resolves");
        assert_eq!(objective_fallback.text, "Objective text");
        assert_eq!(objective_fallback.source, SessionTitleSource::Generated);

        let prompt_fallback = resolve_session_title(
            None,
            SessionTitlePolicy::Generated,
            None,
            None,
            Some("first prompt"),
            None,
        )
        .expect("first-prompt fallback resolves");
        assert_eq!(prompt_fallback.text, "first prompt");
        assert_eq!(prompt_fallback.source, SessionTitleSource::FirstPrompt);
    }

    /// Verifies a stored generated title never outranks a manual name and is
    /// ignored by every policy other than `generated`.
    #[test]
    fn stored_generated_title_never_outranks_a_name_or_another_policy() {
        let named = resolve_session_title(
            Some("operator name"),
            SessionTitlePolicy::Generated,
            Some("Generated title"),
            None,
            None,
            None,
        )
        .expect("manual name resolves");
        assert_eq!(named.text, "operator name");
        assert_eq!(named.source, SessionTitleSource::Name);

        for (policy, expected) in [
            (SessionTitlePolicy::Objective, "Objective text"),
            (SessionTitlePolicy::LastPrompt, "latest prompt"),
            (SessionTitlePolicy::FirstPrompt, "first prompt"),
        ] {
            let resolved = resolve_session_title(
                None,
                policy,
                Some("Generated title"),
                Some("Objective text"),
                Some("first prompt"),
                Some("latest prompt"),
            )
            .expect("policy resolves its own source");
            assert_eq!(resolved.text, expected, "policy {}", policy.as_str());
        }
    }
}
