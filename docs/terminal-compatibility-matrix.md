# Terminal Compatibility Matrix

This matrix records the current compatibility bar for Mezzanine's default
`xterm-compatible` terminal profile. The profile is a bounded implemented
subset for multiplexed pane workloads; it is not a claim that Mezzanine is a
complete xterm emulator.

Use this document when changing terminal parsing, rendering, terminfo/profile
claims, or full-screen TUI behavior. Every advertised capability should have an
implementation owner and at least one focused regression or fixture. Known
unsupported behavior should be explicit instead of implied by the profile name.

## Current automated coverage

| Area | Current coverage | Representative tests or fixtures |
| --- | --- | --- |
| C0 controls and cursor motion | Backspace, carriage return, line feed, VT-style IND/NEL, relative cursor movement, and origin-mode scroll-region clamping. | `terminal_screen_handles_relative_cursor_movement_and_c0_controls`, `terminal_screen_handles_vt_line_movement_controls`, `terminal_screen_origin_mode_offsets_cursor_addressing_into_scroll_region`, `terminal_screen_origin_mode_clamps_relative_vertical_movement_to_scroll_region` |
| CSI editing and scrolling | Cursor addressing, erase display/line, insert/delete characters and lines, scroll regions, CHA/HPA, and CPR replies. | `terminal_screen_csi_cursor_movement`, `terminal_screen_handles_erase_display_variants`, `terminal_screen_handles_erase_line_variants`, `terminal_screen_handles_insertion_deletion_and_scroll_regions`, `terminal_screen_honors_horizontal_absolute_cursor_movement`, `terminal_screen_queues_device_status_report_replies` |
| SGR and color state | Per-cell SGR, styled blanks, attached output style spans, 256-color, truecolor, and row-diff style preservation. | `terminal_screen_stores_sgr_rendition_per_printed_cell`, `terminal_screen_preserves_styled_trailing_blank_cells`, `attached_output_frame_encodes_sgr_style_spans`, `attached_terminal_output_update_keeps_style_on_final_changed_character` |
| UTF-8, width, and graphemes | Split UTF-8, invalid UTF-8 replacement, wide glyph boundaries, emoji width policy, complete-grapheme restoration, and documented combining-mark boundary behavior. | `terminal_screen_preserves_split_utf8_across_feed_calls`, `terminal_screen_replaces_invalid_utf8_without_breaking_layout`, `terminal_screen_double_width_character_boundary`, `terminal_screen_colored_check_mark_wraps_as_double_width`, `terminal_screen_restores_styled_lines_with_complete_graphemes`, `terminal_screen_documents_combining_mark_boundary_behavior` |
| Autowrap | Deferred autowrap and DECAWM enable/disable behavior at the right margin. | `terminal_screen_defers_autowrap_until_next_printable_cell`, `terminal_screen_decawm_disabled_keeps_printing_at_right_margin`, `terminal_screen_decawm_reenabled_restores_deferred_wrap` |
| Alternate screen and history isolation | Pane-local alternate screen, normal-screen restore, DEC1048 cursor-only save/restore, scroll-off exclusion, redraw exclusion, active alternate-screen copy exclusion, and host-normal-screen presentation policy. | `terminal_screen_restores_normal_screen_after_alternate_screen_exit`, `terminal_screen_dec1048_saves_cursor_without_switching_buffers`, `terminal_screen_excludes_alternate_screen_scroll_off_history`, `terminal_screen_excludes_alternate_screen_redraws_from_history`, `copy_mode_excludes_active_alternate_screen_content`, `attached_terminal_output_frame_keeps_host_normal_screen_for_alternate_panes`, `attached_terminal_output_update_ignores_alternate_screen_for_host_modes` |
| Mouse, paste, focus, and keypad modes | SGR mouse parsing, legacy xterm mouse translation for screen-style applications, bracketed paste opacity, focus mode propagation, application cursor, and keypad forwarding. | `parses_sgr_mouse_press_drag_release_and_scroll`, `client_loop_translates_sgr_host_mouse_to_legacy_xterm_pane_mouse`, `client_loop_keeps_host_bracketed_paste_opaque_across_chunks`, `terminal_screen_tracks_application_cursor_keypad_and_focus_modes`, `attached_output_frame_sets_client_application_keypad_mode` |
| Terminfo/profile claims | Mezzanine terminfo preference, safe fallback order, no default host xterm identity, explicit DCS unsupported declaration, and profile diagnostics. | `terminfo_prefers_mezzanine_entry`, `terminfo_accepts_mezzanine_alias_from_installed_terms`, `terminfo_fallbacks_have_capability_safe_degradation`, `terminfo_does_not_use_host_xterm_identity_by_default`, `xterm_compatible_profile_does_not_advertise_unimplemented_dcs_support`, `terminfo_diagnostics_expose_profile_term_and_degradation` |
| Foreground PTY full-screen behavior | A real `mez attach` PTY runs a deterministic alternate-screen script and verifies pane-local rendering without host alternate-screen entry. | `foreground_attach_runs_minimal_full_screen_script` |

## Unsupported or bounded behavior

- DCS string controls are explicitly unsupported and must remain unadvertised
  until implemented and tested.
- The default profile intentionally keeps attached foreground clients on the
  containing terminal's normal screen. Full-screen isolation is therefore a
  Mezzanine pane-rendering contract rather than host-terminal alternate-screen
  pass-through.
- Combining-mark behavior is covered as a documented Mezzanine boundary. Before
  changing it, compare the desired behavior against xterm-compatible grapheme
  composition and update tests and this matrix together.
- Real application compatibility is not a blanket guarantee. Deterministic
  regressions are the primary compatibility signal; app fixtures should be added
  only when they can run reliably in automated environments.

## Fixture backlog

The current suite covers core parser/model behavior and one foreground PTY
full-screen path. The next compatibility improvements should add deterministic
fixtures before broadening user-facing reliability claims:

1. A foreground PTY script that enters alternate screen, sets DECSTBM margins,
   uses DECOM, scrolls inside the region, and exits while proving no alternate
   rows entered normal history.
2. A resize-during-full-screen fixture that changes the attached PTY size while
   a pane-local full-screen script is active and verifies repaint/reflow output.
3. A burst repaint fixture larger than one ordinary read/render quantum to
   verify eventual final state and bound visible intermediate-frame artifacts.
4. Optional real-world smoke fixtures for `less`, `vim`/`nvim`, `dialog`,
   `fzf`, and `htop`/`top` when those tools are available and can be made
   deterministic.

## Maintenance rule

When a terminal capability is added, removed, or reclassified, update all of the
following in the same change:

- `src/terminal/profile.rs` capability declarations and diagnostics.
- Parser/rendering implementation and focused regression tests.
- `SPEC.md` if the compatibility contract changes.
- This matrix, including any new unsupported behavior or fixture backlog item.
