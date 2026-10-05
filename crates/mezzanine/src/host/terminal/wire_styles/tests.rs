//! Shared rendition interpretation, deliberately independent of viewport policy.
use super::*;

/// Extraction retains optional flag defaults, explicit colors and cell offsets;
/// attributes are not confused with text bytes or remote presentation authority.
#[test]
fn wire_styles_preserve_optional_attributes_and_cell_offsets() {
    let row = serde_json::json!([{"start":2,"length":3,"rendition":{
        "bold":true,"italic":true,"hidden":true,"underline":"not-a-bool",
        "foreground":{"kind":"rgb","red":1,"green":2,"blue":255},
        "background":{"kind":"indexed","index":7}}}]);
    let spans = parse_terminal_style_span_row(&row).unwrap();
    assert_eq!((spans[0].start, spans[0].length), (2, 3));
    assert!(spans[0].rendition.bold && spans[0].rendition.italic && spans[0].rendition.hidden);
    assert!(!spans[0].rendition.underline);
    assert_eq!(
        spans[0].rendition.foreground,
        Some(TerminalColor::Rgb(1, 2, 255))
    );
    assert_eq!(
        spans[0].rendition.background,
        Some(TerminalColor::Indexed(7))
    );
    assert_eq!(
        parse_terminal_graphic_rendition(&serde_json::json!({"foreground":null})).unwrap(),
        GraphicRendition::default()
    );
}

/// Explicit malformed colors and offsets reject instead of silently clamping;
/// unknown/missing optional boolean attributes retain existing false defaults.
#[test]
fn wire_styles_reject_invalid_explicit_colors_and_offsets() {
    for color in [
        serde_json::json!({"kind":"indexed","index":256}),
        serde_json::json!({"kind":"rgb","red":-1,"green":0,"blue":0}),
        serde_json::json!({"kind":"rgb","red":0,"green":0}),
        serde_json::json!({"kind":"unknown"}),
    ] {
        assert!(
            parse_terminal_graphic_rendition(&serde_json::json!({"foreground":color})).is_err()
        );
    }
    for row in [
        serde_json::json!({}),
        serde_json::json!([{"start":-1,"length":1,"rendition":{}}]),
        serde_json::json!([{"start":0,"rendition":{}}]),
    ] {
        assert!(parse_terminal_style_span_row(&row).is_err());
    }
}
