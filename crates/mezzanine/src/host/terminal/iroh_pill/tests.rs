//! Shared composition preserves immutable base rows and outside-slot styling.
use super::*;
use mez_terminal::{GraphicRendition, TerminalColor};

/// Every health category uses the corresponding theme rendition; disconnected
/// status uses unknown. Crossing spans are split around the pill without changing
/// the cached base, and missing slots/rows leave the original frame unchanged.
#[test]
fn iroh_pill_composition_preserves_base_and_outside_styles() {
    let lines = vec!["雪a    tail".to_string()];
    let base = GraphicRendition {
        bold: true,
        ..Default::default()
    };
    let spans = vec![vec![TerminalStyleSpan {
        start: 0,
        length: 11,
        rendition: base,
    }]];
    let colored = |index| GraphicRendition {
        background: Some(TerminalColor::Indexed(index)),
        ..Default::default()
    };
    let slot = TerminalIrohStatusSlot {
        row: 0,
        column: 3,
        width: 4,
        good: colored(2),
        degraded: colored(3),
        poor: colored(1),
        unknown: colored(8),
    };
    for (quality, index) in [
        (TerminalIrohStatusQuality::Good, 2),
        (TerminalIrohStatusQuality::Degraded, 3),
        (TerminalIrohStatusQuality::Poor, 1),
        (TerminalIrohStatusQuality::Unknown, 8),
    ] {
        let (painted, styles) = compose(&lines, &spans, Some(slot), true, quality);
        assert_eq!(painted, ["雪a up tail"]);
        assert_eq!(
            styles[0]
                .iter()
                .find(|span| span.start == 3)
                .unwrap()
                .rendition,
            colored(index)
        );
        assert!(
            styles[0]
                .iter()
                .any(|span| span.start == 0 && span.length == 3 && span.rendition == base)
        );
        assert!(
            styles[0]
                .iter()
                .any(|span| span.start == 7 && span.length == 4 && span.rendition == base)
        );
    }
    let (painted, styles) = compose(
        &lines,
        &spans,
        Some(slot),
        false,
        TerminalIrohStatusQuality::Good,
    );
    assert_eq!(painted, ["雪a dn tail"]);
    assert_eq!(
        styles[0]
            .iter()
            .find(|span| span.start == 3)
            .unwrap()
            .rendition,
        colored(8)
    );
    assert_eq!(
        compose(&lines, &spans, None, true, TerminalIrohStatusQuality::Good),
        (lines.clone(), spans.clone())
    );
    assert_eq!(
        compose(
            &lines,
            &spans,
            Some(TerminalIrohStatusSlot { row: 1, ..slot }),
            true,
            TerminalIrohStatusQuality::Good
        ),
        (lines.clone(), spans.clone())
    );
    assert_eq!(lines, ["雪a    tail"]);
    assert_eq!(spans[0][0].rendition, base);
}
