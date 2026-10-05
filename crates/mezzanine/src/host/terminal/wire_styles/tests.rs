//! Shared rendition interpretation, deliberately independent of viewport policy.
use super::*;

/// Production viewport selection appends an overlay to one-cell base runs.
/// That valid output has more spans than columns, including at width one;
/// snapshot decoding must preserve the compositor's exact layering order.
#[test]
fn wire_styles_bounded_rows_accept_production_selection_layers() {
    use mez_mux::copy::CopyPosition;
    use mez_mux::presentation::{
        ClientViewRole, RenderedClientView, TerminalCursorStyle, compose_client_viewport,
    };
    for columns in [1_u16, 8] {
        let size = mez_terminal::TerminalSize::new(columns, 1).unwrap();
        let view = RenderedClientView {
            role: ClientViewRole::Primary,
            authoritative_size: size,
            client_size: size,
            lines: vec!["x".repeat(usize::from(columns))],
            line_style_spans: vec![
                (0..usize::from(columns))
                    .map(|start| TerminalStyleSpan {
                        start,
                        length: 1,
                        rendition: GraphicRendition {
                            foreground: Some(TerminalColor::Indexed(start as u8)),
                            ..Default::default()
                        },
                    })
                    .collect(),
            ],
            selection: Some((
                CopyPosition { line: 0, column: 0 },
                CopyPosition {
                    line: 0,
                    column: usize::from(columns),
                },
            )),
            requires_client_scroll: false,
            viewport_row: 0,
            viewport_column: 0,
            cursor_row: 0,
            cursor_column: 0,
            cursor_visible: false,
            cursor_style: TerminalCursorStyle::Block,
            cursor_blink: false,
            cursor_blink_interval_ms: 500,
            application_keypad: false,
            bracketed_paste: false,
            focus_events: false,
            alternate_screen: false,
            host_mouse_reporting: false,
            animation_refresh_interval_ms: 0,
            ui_theme: Default::default(),
            agent_prompt_region: None,
            primary_prompt_active: false,
            readline_input_active: false,
        };
        let (lines, styles) = compose_client_viewport(&view);
        assert!(styles[0].len() > usize::from(columns));
        assert_eq!(
            bounded_style_rows(&style_rows_value(&styles), lines.len(), columns).unwrap(),
            styles
        );
    }
}

/// Renderer overlays may overlap and return to an earlier cell position. Their
/// source order determines precedence and must survive snapshot transport;
/// bounding each span must not reject a valid later overlay.
#[test]
fn wire_styles_bounded_rows_preserve_layered_precedence() {
    let rows = serde_json::json!([[
        {"start":0,"length":8,"rendition":{"bold":true}},
        {"start":2,"length":3,"rendition":{"inverse":true}},
        {"start":0,"length":1,"rendition":{"italic":true}}
    ]]);
    let decoded = bounded_style_rows(&rows, 1, 8).unwrap();
    assert_eq!(
        decoded[0]
            .iter()
            .map(|span| (span.start, span.length))
            .collect::<Vec<_>>(),
        vec![(0, 8), (2, 3), (0, 1)]
    );
    assert!(decoded[0][1].rendition.inverse);
    assert!(decoded[0][2].rendition.italic);
    assert_eq!(
        bounded_style_rows(&style_rows_value(&decoded), 1, 8).unwrap(),
        decoded
    );
}

/// Aligned snapshot rows reject empty/out-of-range/overflowing spans and finite
/// row-budget violations without sorting away legitimate overlay precedence.
#[test]
fn wire_styles_bounded_rows_reject_invalid_geometry() {
    for rows in [
        serde_json::json!([]),
        serde_json::json!([[{"start":0,"length":0,"rendition":{}}]]),
        serde_json::json!([[{"start":7,"length":2,"rendition":{}}]]),
        serde_json::json!([[{"start":u64::MAX,"length":1,"rendition":{}}]]),
        serde_json::json!([vec![
            serde_json::json!({"start":0,"length":1,"rendition":{}});
            MAX_SNAPSHOT_STYLE_SPANS_PER_ROW + 1
        ]]),
    ] {
        assert!(bounded_style_rows(&rows, 1, 8).is_err());
    }
    assert!(bounded_style_rows(&serde_json::json!([[]]), 1, 0).is_err());
    assert!(bounded_style_rows(&serde_json::json!([[]]), 1, 4097).is_err());
}

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
