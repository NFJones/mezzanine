//! Shared status-slot interpretation, without terminal or transport effects.
use super::*;

/// Coordinates and all rendition categories retain existing decoding, including
/// null default colors. Missing coordinates/renditions and invalid color bytes
/// reject rather than inventing slot or health evidence.
#[test]
fn wire_status_slot_preserves_coordinates_and_renditions() {
    let original = serde_json::json!({"row":1,"column":4,"width":4,
        "good":{"bold":true,"foreground":{"kind":"indexed","index":2}},
        "degraded":{"background":{"kind":"rgb","red":1,"green":2,"blue":3}},
        "poor":{"inverse":true},"unknown":{"foreground":null,"background":null}});
    let slot = parse_terminal_iroh_status_slot(&original).unwrap();
    assert_eq!((slot.row, slot.column, slot.width), (1, 4, 4));
    assert!(slot.good.bold);
    assert_eq!(
        slot.good.foreground,
        Some(mez_terminal::TerminalColor::Indexed(2))
    );
    assert_eq!(
        slot.degraded.background,
        Some(mez_terminal::TerminalColor::Rgb(1, 2, 3))
    );
    assert!(slot.poor.inverse);
    assert_eq!(slot.unknown.foreground, None);
    for field in [
        "row", "column", "width", "good", "degraded", "poor", "unknown",
    ] {
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            parse_terminal_iroh_status_slot(&missing).is_err(),
            "{field}"
        );
    }
    let mut invalid = original;
    invalid["good"]["foreground"]["index"] = serde_json::json!(256);
    assert!(parse_terminal_iroh_status_slot(&invalid).is_err());
}
