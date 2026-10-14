//! Product-only, pure occurrence-time decoration of canonical terminal rows.
//!
//! Terminal parsing, snapshots, persistence and source reconstruction never
//! consume these visual rows. Geometry travels alongside each projected row
//! into copy/search snapshots; neither timestamp-looking text nor whitespace
//! is provenance. Formatting is UTC, bounded and based solely on the captured
//! instant. No current clock, timezone lookup, file, environment or provider
//! operation belongs here. Attached clients receive already formatted rows.

use chrono::Timelike;
use mez_core::presentation::{
    LogFrame, LogFramePolicy, LogTimestampFormat, LogTimestampToken, PresentationEntry,
};
use mez_terminal::{GraphicRendition, TerminalStyleSpan, TerminalStyledLine};

/// One explicit visual projection and its matching content-coordinate map.
pub(crate) struct ProjectedLogRows {
    /// Rows suitable for visual frames, never canonical restoration.
    pub rows: Vec<TerminalStyledLine>,
    /// Owned frame geometry; absent for opaque/process or disabled rows.
    pub frames: Vec<Option<LogFrame>>,
}

/// Returns the validated number of ASCII UTC cells, or truthful blank cells.
/// Unknown/incoherent/out-of-range evidence and original continuations are
/// unavailable, not epoch zero. Legacy record evidence keeps coarse seconds.
/// No token is emitted before provenance and representability checks, even
/// for literal-only formats. The compiled capture is never parsed during paint.
pub(crate) fn occurrence_timestamp_cells(
    entry: &PresentationEntry,
    format: &LogTimestampFormat,
) -> String {
    if !entry.first_row || !entry.is_valid() {
        return " ".repeat(format.width());
    }
    let Some(instant) = entry
        .occurrence
        .unix_millis
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
    else {
        return " ".repeat(format.width());
    };
    let mut output = String::with_capacity(format.width());
    for token in format.tokens() {
        let value = match token {
            LogTimestampToken::Literal(byte) => {
                output.push(char::from(*byte));
                continue;
            }
            LogTimestampToken::Hour => instant.hour(),
            LogTimestampToken::Minute => instant.minute(),
            LogTimestampToken::Second => instant.second(),
        };
        // Chrono's captured UTC numeric components are always two-digit
        // values here, including leap-second evidence (no fraction exposed).
        output.push(char::from(b'0' + (value / 10) as u8));
        output.push(char::from(b'0' + (value % 10) as u8));
    }
    output
}

/// Original default-format oracle retained for existing occurrence regressions.
#[cfg(test)]
pub(crate) fn occurrence_clock_cells(entry: &PresentationEntry) -> String {
    occurrence_timestamp_cells(entry, &LogTimestampFormat::default())
}

/// Projects proven row metadata without changing canonical/copy source bytes.
/// The caller supplies rows already reflowed at matching canonical geometry.
/// No guessed entry is created for opaque ANSI, process output or resume chrome.
pub(crate) fn project_log_rows(
    mut rows: Vec<TerminalStyledLine>,
    policy: LogFramePolicy,
    columns: usize,
    timestamp_rendition: GraphicRendition,
) -> ProjectedLogRows {
    let frame = policy.frame(columns);
    let mut frames = Vec::with_capacity(rows.len());
    for row in &mut rows {
        let entry = row.presentation.as_ref().filter(|entry| entry.is_valid());
        if frame.margin_width() == 0 || entry.is_none() {
            frames.push(None);
            continue;
        }
        let stamp = entry
            .map(|entry| occurrence_timestamp_cells(entry, &policy.timestamp_format))
            .unwrap_or_else(|| " ".repeat(policy.timestamp_format.width()));
        row.text.insert_str(0, &format!("{stamp} "));
        for span in &mut row.style_spans {
            span.start += frame.margin_width();
        }
        row.style_spans.insert(
            0,
            TerminalStyleSpan {
                start: 0,
                length: frame.margin_width(),
                rendition: timestamp_rendition,
            },
        );
        frames.push(Some(frame.clone()));
    }
    ProjectedLogRows { rows, frames }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mez_core::presentation::{PresentationOccurrence, PresentationOccurrenceBasis};

    /// Decoration is based solely on owned metadata. Unknown and cropped
    /// rows reserve blank cells, opaque timestamp-looking text stays literal,
    /// and category/body spans are shifted without inheriting dim or background.
    #[test]
    fn log_timestamp_projection_preserves_source_style_and_cropped_boundaries() {
        let theme = mez_mux::theme::UiTheme::default();
        let original_style = GraphicRendition {
            bold: true,
            ..GraphicRendition::default()
        };
        let mut first = TerminalStyledLine::plain("│ mez> 01:02:03 authored");
        first.copy_text = Some("exact raw source".into());
        first.style_spans.push(TerminalStyleSpan {
            start: 0,
            length: 7,
            rendition: original_style,
        });
        first.presentation = Some(PresentationEntry {
            id: "owned".into(),
            occurrence: PresentationOccurrence::new(
                Some(3_723_000),
                PresentationOccurrenceBasis::LocalAcceptance,
            ),
            first_row: true,
        });
        let mut cropped = first.clone();
        cropped.presentation.as_mut().unwrap().first_row = false;
        let mut unknown = first.clone();
        unknown.presentation.as_mut().unwrap().occurrence = PresentationOccurrence::default();
        let opaque = TerminalStyledLine::plain("01:02:03 │ literal PTY");
        let canonical = vec![first, cropped, unknown, opaque];
        let off = project_log_rows(
            canonical.clone(),
            LogFramePolicy {
                timestamps: false,
                wrap_cap: 80,
                ..LogFramePolicy::default()
            },
            80,
            theme.agent_framing_rendition(),
        );
        assert_eq!(off.rows, canonical);
        assert!(off.frames.iter().all(Option::is_none));
        let on = project_log_rows(
            canonical.clone(),
            LogFramePolicy {
                timestamps: true,
                wrap_cap: 80,
                ..LogFramePolicy::default()
            },
            80,
            theme.agent_framing_rendition(),
        );
        assert!(on.rows[0].text.starts_with("01:02:03 │ "));
        assert!(on.rows[1].text.starts_with("         │ "));
        assert!(on.rows[2].text.starts_with("         │ "));
        assert_eq!(on.rows[3], canonical[3]);
        for row in &on.rows[..3] {
            assert_eq!(row.copy_text.as_deref(), Some("exact raw source"));
            assert_eq!(
                row.style_spans[0].rendition,
                theme.agent_framing_rendition()
            );
            assert_eq!(row.style_spans[0].length, 9);
            assert_eq!(row.style_spans[0].rendition.background, None);
            assert_eq!(row.style_spans[1].start, 9);
            assert_eq!(row.style_spans[1].rendition, original_style);
        }
    }

    /// Signed pre-epoch, epoch, rollover, equal/backward and unrepresentable
    /// values format without clock sampling. Sequence identity and first-row
    /// provenance, not clock text, determine whether a stamp is shown.
    #[test]
    fn log_timestamp_utc_formatter_is_bounded_and_truthful() {
        for (millis, expected) in [
            (-1, "23:59:59"),
            (0, "00:00:00"),
            (3_723_000, "01:02:03"),
            (86_400_000, "00:00:00"),
            (3_723_000, "01:02:03"),
            (1_000, "00:00:01"),
            (i64::MIN, "        "),
            (i64::MAX, "        "),
        ] {
            let mut entry = PresentationEntry {
                id: "owned-entry".into(),
                occurrence: PresentationOccurrence::new(
                    Some(millis),
                    PresentationOccurrenceBasis::LocalObservation,
                ),
                first_row: true,
            };
            assert_eq!(occurrence_clock_cells(&entry), expected);
            assert_eq!(occurrence_clock_cells(&entry).len(), 8);
            entry.first_row = false;
            assert_eq!(occurrence_clock_cells(&entry), "        ");
        }
        let entry = PresentationEntry {
            id: "unknown".into(),
            occurrence: PresentationOccurrence::default(),
            first_row: true,
        };
        assert_eq!(occurrence_clock_cells(&entry), "        ");
    }

    /// Custom formats have invariant ASCII width across signed instants,
    /// rollover and chrono years outside four-digit dates. No date directive
    /// means no artificial year restriction. Coarse legacy seconds remain
    /// truthful, while unknown/cropped/incoherent inputs blank even literals.
    #[test]
    fn log_timestamp_format_numeric_edges_and_unavailable_literals_are_truthful() {
        for (source, expected) in [
            ("%H:%M", "01:02"),
            ("[%S-%M-%H]", "[03-02-01]"),
            ("UTC %H%M%S %%", "UTC 010203 %"),
            ("literal", "literal"),
        ] {
            let format = LogTimestampFormat::compile(source).unwrap();
            let mut entry = PresentationEntry {
                id: "custom-owned".into(),
                occurrence: PresentationOccurrence::new(
                    Some(3_723_000),
                    PresentationOccurrenceBasis::LocalAcceptance,
                ),
                first_row: true,
            };
            assert_eq!(occurrence_timestamp_cells(&entry, &format), expected);
            for millis in [
                -86_400_001,
                -1,
                0,
                59_999,
                60_000,
                3_599_999,
                3_600_000,
                86_399_999,
                86_400_000,
                253_402_300_800_000,
                -62_198_755_200_000,
            ] {
                entry.occurrence.unix_millis = Some(millis);
                let output = occurrence_timestamp_cells(&entry, &format);
                assert_eq!(output.len(), format.width());
                assert!(output.is_ascii());
                if source == "%H:%M" {
                    let instant =
                        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis).unwrap();
                    assert_eq!(output, instant.format("%H:%M").to_string());
                }
            }
            entry.occurrence = PresentationOccurrence::legacy_record(3_723);
            assert_eq!(occurrence_timestamp_cells(&entry, &format), expected);
            for millis in [i64::MIN, i64::MAX] {
                entry.occurrence.unix_millis = Some(millis);
                assert_eq!(
                    occurrence_timestamp_cells(&entry, &format),
                    " ".repeat(format.width())
                );
            }
            entry.occurrence = PresentationOccurrence::default();
            assert_eq!(
                occurrence_timestamp_cells(&entry, &format),
                " ".repeat(format.width())
            );
            entry.occurrence.unix_millis = Some(3_723_000);
            assert_eq!(
                occurrence_timestamp_cells(&entry, &format),
                " ".repeat(format.width())
            );
            entry.occurrence = PresentationOccurrence::legacy_record(3_723);
            entry.first_row = false;
            assert_eq!(
                occurrence_timestamp_cells(&entry, &format),
                " ".repeat(format.width())
            );
        }
    }
}
