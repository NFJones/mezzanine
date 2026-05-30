# Issue backlog

- [Issue backlog](#issue-backlog)
  - [Open issues](#open-issues)
    - [Rendering \& Style Overlay Deep Bug Audit](#rendering--style-overlay-deep-bug-audit)
      - [Finding R1 (MEDIUM): encode\_safe\_changed\_row\_span\_update produces incorrect segment updates when wide glyphs span the change boundary](#finding-r1-medium-encode_safe_changed_row_span_update-produces-incorrect-segment-updates-when-wide-glyphs-span-the-change-boundary)
      - [Finding R2 (MEDIUM): encode\_safe\_changed\_row\_span\_update may emit a segment update LARGER than a full row update but the guard doesn’t catch it](#finding-r2-medium-encode_safe_changed_row_span_update-may-emit-a-segment-update-larger-than-a-full-row-update-but-the-guard-doesnt-catch-it)
      - [Finding R3 (MEDIUM): expand\_changed\_column\_range can expand to cover the ENTIRE row when style spans are interleaved](#finding-r3-medium-expand_changed_column_range-can-expand-to-cover-the-entire-row-when-style-spans-are-interleaved)
      - [Finding R4 (MEDIUM): terminal\_row\_cells returns empty Vec on grapheme-not-found, silently producing no diff](#finding-r4-medium-terminal_row_cells-returns-empty-vec-on-grapheme-not-found-silently-producing-no-diff)
      - [Finding R5 (LOW): encode\_safe\_changed\_row\_span\_update uses terminal\_line\_width which counts display width, but previous\_cells.len() counts grapheme count](#finding-r5-low-encode_safe_changed_row_span_update-uses-terminal_line_width-which-counts-display-width-but-previous_cellslen-counts-grapheme-count)
      - [Finding R6 (LOW): rendition\_at\_column uses .rev() to find the last-applied span, but the span list is not guaranteed to be in composition order](#finding-r6-low-rendition_at_column-uses-rev-to-find-the-last-applied-span-but-the-span-list-is-not-guaranteed-to-be-in-composition-order)
      - [Finding R7 (LOW): write\_text\_cells writes wide glyph continuation sentinels but draw\_pane\_dividers overwrites them without clearing the leading cell](#finding-r7-low-write_text_cells-writes-wide-glyph-continuation-sentinels-but-draw_pane_dividers-overwrites-them-without-clearing-the-leading-cell)
      - [Finding R8 (LOW): render\_styled\_panes\_by\_geometry pushes style spans with clip\_style\_span then offset\_style\_span, but clip\_style\_span may truncate length without adjusting for wide glyph boundaries](#finding-r8-low-render_styled_panes_by_geometry-pushes-style-spans-with-clip_style_span-then-offset_style_span-but-clip_style_span-may-truncate-length-without-adjusting-for-wide-glyph-boundaries)
      - [Finding R9 (LOW): compose\_client\_presentation\_with\_styles uses slice\_terminal\_line which calls line\_slice, but line\_slice may split a wide glyph](#finding-r9-low-compose_client_presentation_with_styles-uses-slice_terminal_line-which-calls-line_slice-but-line_slice-may-split-a-wide-glyph)
      - [Finding R10 (LOW): push\_client\_selection\_style\_spans uses push\_or\_extend\_style\_span which merges adjacent same-rendition spans, but the selection span may merge with a pre-existing span of the same rendition, making the selection invisible](#finding-r10-low-push_client_selection_style_spans-uses-push_or_extend_style_span-which-merges-adjacent-same-rendition-spans-but-the-selection-span-may-merge-with-a-pre-existing-span-of-the-same-rendition-making-the-selection-invisible)
      - [Finding R11 (LOW): compose\_display\_overlay\_line\_style\_spans calls agent\_live\_footer\_style\_spans which may return empty, then falls back to a full-width span — but the fallback uses overlay\_text\_style\_width which may return 0 for empty display lines](#finding-r11-low-compose_display_overlay_line_style_spans-calls-agent_live_footer_style_spans-which-may-return-empty-then-falls-back-to-a-full-width-span--but-the-fallback-uses-overlay_text_style_width-which-may-return-0-for-empty-display-lines)
      - [Finding R12 (LOW): render\_frame\_template in framing/render.rs uses sanitize\_frame\_text which strips non-ASCII, but the frame template may contain Unicode characters from user config](#finding-r12-low-render_frame_template-in-framingrenderrs-uses-sanitize_frame_text-which-strips-non-ascii-but-the-frame-template-may-contain-unicode-characters-from-user-config)
      - [Finding R13 (LOW): elide in framing/render.rs uses ... (three dots) instead of … (single ellipsis character), consuming 3 columns instead of 1](#finding-r13-low-elide-in-framingrenderrs-uses--three-dots-instead-of--single-ellipsis-character-consuming-3-columns-instead-of-1)
      - [Finding R14 (LOW): wrap in framing/render.rs wraps at width boundaries but doesn’t handle wide glyphs](#finding-r14-low-wrap-in-framingrenderrs-wraps-at-width-boundaries-but-doesnt-handle-wide-glyphs)
      - [Finding R15 (MEDIUM): render\_attached\_client\_view computes animation\_refresh\_interval\_ms based on animation\_tick\_ms \> 0, but the prompt renderer uses AGENT\_STATUS\_ANIMATION\_REFRESH\_INTERVAL\_MS unconditionally in some paths](#finding-r15-medium-render_attached_client_view-computes-animation_refresh_interval_ms-based-on-animation_tick_ms--0-but-the-prompt-renderer-uses-agent_status_animation_refresh_interval_ms-unconditionally-in-some-paths)
    - [Style Overlay Application \& Wide Unicode Glyph Rendering](#style-overlay-application--wide-unicode-glyph-rendering)
      - [Finding S1 (Low): Canvas-level style spans are written without merging against pre-existing spans](#finding-s1-low-canvas-level-style-spans-are-written-without-merging-against-pre-existing-spans)
      - [Finding S2 (Low): Divider style overlay replaces cell-level spans but doesn’t remove them from the span list](#finding-s2-low-divider-style-overlay-replaces-cell-level-spans-but-doesnt-remove-them-from-the-span-list)
      - [Finding S3 (Low): Frame text overlay uses extend\_styled\_line which pushes spans but doesn’t merge adjacent same-rendition spans](#finding-s3-low-frame-text-overlay-uses-extend_styled_line-which-pushes-spans-but-doesnt-merge-adjacent-same-rendition-spans)
      - [Finding S4 (Low): Prompt overlay composition uses compose\_agent\_display\_text\_overlay which clears spans then re-pushes, but the span range may exceed the client viewport](#finding-s4-low-prompt-overlay-composition-uses-compose_agent_display_text_overlay-which-clears-spans-then-re-pushes-but-the-span-range-may-exceed-the-client-viewport)
      - [Finding W1 (Medium): write\_single\_width\_cell only clears one continuation cell, but wide glyphs can be wider than 2 cells (emoji ZWJ sequences)](#finding-w1-medium-write_single_width_cell-only-clears-one-continuation-cell-but-wide-glyphs-can-be-wider-than-2-cells-emoji-zwj-sequences)
      - [Finding W2 (Low): write\_frame\_text\_cells uses ' ' instead of '\\0' for wide-glyph continuation cells](#finding-w2-low-write_frame_text_cells-uses---instead-of-0-for-wide-glyph-continuation-cells)
      - [Finding W3 (Low): collect\_text\_cells strips sentinels but doesn’t validate that remaining codepoints form valid UTF-8](#finding-w3-low-collect_text_cells-strips-sentinels-but-doesnt-validate-that-remaining-codepoints-form-valid-utf-8)
      - [Finding W4 (Low): write\_text\_cells condition if column + grapheme\_width \<= row.len() may leave cells unfilled when a wide glyph extends past the row boundary](#finding-w4-low-write_text_cells-condition-if-column--grapheme_width--rowlen-may-leave-cells-unfilled-when-a-wide-glyph-extends-past-the-row-boundary)
      - [Finding W5 (Low): draw\_styled\_pane\_dividers writes divider glyphs over wide-glyph continuation cells but doesn’t clear the companion leading cell](#finding-w5-low-draw_styled_pane_dividers-writes-divider-glyphs-over-wide-glyph-continuation-cells-but-doesnt-clear-the-companion-leading-cell)
      - [Finding W6 (Info): Zerowidth characters (U+200B, U+FEFF, etc.) have terminal\_grapheme\_width of 0 and are handled correctly](#finding-w6-info-zerowidth-characters-u200b-ufeff-etc-have-terminal_grapheme_width-of-0-and-are-handled-correctly)
      - [Key Finding: The Divider Style Bug (S2)](#key-finding-the-divider-style-bug-s2)
      - [Key Finding: Wide Glyph + Divider Interaction (W5)](#key-finding-wide-glyph--divider-interaction-w5)
    - [Async Runtime Deep Bug Audit](#async-runtime-deep-bug-audit)
      - [Debuggability Concern 1 (High): tokio::select! Bias Masks Late-Arriving PTY Output](#debuggability-concern-1-high-tokioselect-bias-masks-late-arriving-pty-output)
      - [Bug 1 (Medium): forward\_pty\_output spawns unbounded tasks but CancellationToken is the only backpressure mechanism](#bug-1-medium-forward_pty_output-spawns-unbounded-tasks-but-cancellationtoken-is-the-only-backpressure-mechanism)
      - [Bug 2 (Low): tokio::spawn for PTY forwarding has no JoinHandle stored — orphaned tasks on panic are invisible](#bug-2-low-tokiospawn-for-pty-forwarding-has-no-joinhandle-stored--orphaned-tasks-on-panic-are-invisible)
      - [Bug 3 (Medium): flush\_paste\_buffer uses tokio::time::interval with PASTE\_FLUSH\_INTERVAL\_MS but the timer is not reset when new paste data arrives](#bug-3-medium-flush_paste_buffer-uses-tokiotimeinterval-with-paste_flush_interval_ms-but-the-timer-is-not-reset-when-new-paste-data-arrives)
      - [Bug 4 (Low): CancellationToken tree uses .child\_token() but parent cancellation does not guarantee child task termination before drop](#bug-4-low-cancellationtoken-tree-uses-child_token-but-parent-cancellation-does-not-guarantee-child-task-termination-before-drop)
      - [Bug 5 (Low): Provider HTTP reqwest timeout is set via RequestBuilder::timeout() but the tokio::select! wrapping it has no outer timeout](#bug-5-low-provider-http-reqwest-timeout-is-set-via-requestbuildertimeout-but-the-tokioselect-wrapping-it-has-no-outer-timeout)
      - [Bug 6 (Low): AgentTurnRunner::run\_turn\_async calls ledger.start\_turn() synchronously before the async provider request, but the async spawn could race with another turn’s start\_turn](#bug-6-low-agentturnrunnerrun_turn_async-calls-ledgerstart_turn-synchronously-before-the-async-provider-request-but-the-async-spawn-could-race-with-another-turns-start_turn)
      - [Bug 7 (Info): Arc\<tokio::sync::Mutex\> wrapping pattern — the entire service is behind a single mutex](#bug-7-info-arctokiosyncmutex-wrapping-pattern--the-entire-service-is-behind-a-single-mutex)
      - [Bug 8 (Low): Signal handler uses tokio::signal::unix::signal() which is Linux/macOS-only — compilation fails on Windows](#bug-8-low-signal-handler-uses-tokiosignalunixsignal-which-is-linuxmacos-only--compilation-fails-on-windows)
    - [Runtime Subsystem Deep Bug Audit](#runtime-subsystem-deep-bug-audit)
      - [Bug 1 (Medium): reset\_tick() unconditionally removes the task from tasks hashmap, but tick\_once() may have already consumed it](#bug-1-medium-reset_tick-unconditionally-removes-the-task-from-tasks-hashmap-but-tick_once-may-have-already-consumed-it)
      - [Bug 2 (Low): AgentShellTickResult::abort\_tick sets RenderedClientView.active\_pane\_id to the original pane even when the abort fires inside a different pane’s agent](#bug-2-low-agentshelltickresultabort_tick-sets-renderedclientviewactive_pane_id-to-the-original-pane-even-when-the-abort-fires-inside-a-different-panes-agent)
      - [Bug 3 (Medium): Provider task id field uses ProviderTaskKey which is a Uuid, but ProviderTaskKind in types.rs has no task\_id — the id is embedded in the message envelope only](#bug-3-medium-provider-task-id-field-uses-providertaskkey-which-is-a-uuid-but-providertaskkind-in-typesrs-has-no-task_id--the-id-is-embedded-in-the-message-envelope-only)
      - [Bug 4 (Low): Event::WindowPtyOutput includes raw Vec — if the event is cloned in the hook pipeline, large PTY output buffers are duplicated](#bug-4-low-eventwindowptyoutput-includes-raw-vec--if-the-event-is-cloned-in-the-hook-pipeline-large-pty-output-buffers-are-duplicated)
      - [Bug 5 (Low): screen\_resize\_guard in auto\_sizing.rs computes window\_frame\_rows\_count but doesn’t account for window frame when the frame position is Bottom](#bug-5-low-screen_resize_guard-in-auto_sizingrs-computes-window_frame_rows_count-but-doesnt-account-for-window-frame-when-the-frame-position-is-bottom)
      - [Bug 6 (Low): parse\_outcome\_chunk in presentation.rs uses byte-level scanning for \\n but the chunk may contain multi-byte UTF-8 sequences where \\n (0x0A) appears as a continuation byte](#bug-6-low-parse_outcome_chunk-in-presentationrs-uses-byte-level-scanning-for-n-but-the-chunk-may-contain-multi-byte-utf-8-sequences-where-n-0x0a-appears-as-a-continuation-byte)
      - [Bug 7 (Low): AgentTaskResult::finalize\_with\_approval clears the pending\_approval flag even when the approval is denied](#bug-7-low-agenttaskresultfinalize_with_approval-clears-the-pending_approval-flag-even-when-the-approval-is-denied)
      - [Bug 8 (Low): ProviderTask::summarize\_error truncates error messages at 512 code points via chars().take(512).collect() — loses multi-byte boundary precision](#bug-8-low-providertasksummarize_error-truncates-error-messages-at-512-code-points-via-charstake512collect--loses-multi-byte-boundary-precision)
      - [Bug 9 (Low): SubagentInvocationIndex uses HashMap\<String, Vec\> with String keys from turn\_id — unbounded growth](#bug-9-low-subagentinvocationindex-uses-hashmapstring-vec-with-string-keys-from-turn_id--unbounded-growth)
      - [Bug 10 (Info): AgentTurnRecord::reconstructed\_input\_messages field stores all messages for a turn (including large tool results) — this duplicates the context assembly data](#bug-10-info-agentturnrecordreconstructed_input_messages-field-stores-all-messages-for-a-turn-including-large-tool-results--this-duplicates-the-context-assembly-data)
      - [Bug 11 (Info): window\_layout\_geometry in types.rs uses usize for row/column indices but PaneGeometry uses u16 — the conversion is as casts with no overflow check](#bug-11-info-window_layout_geometry-in-typesrs-uses-usize-for-rowcolumn-indices-but-panegeometry-uses-u16--the-conversion-is-as-casts-with-no-overflow-check)
      - [Bug 12 (Medium): dispatch\_shell\_transaction calls environment\_variable\_tool\_inventory on every shell invocation, which recomputes the tool inventory from env, which, and PATH scanning](#bug-12-medium-dispatch_shell_transaction-calls-environment_variable_tool_inventory-on-every-shell-invocation-which-recomputes-the-tool-inventory-from-env-which-and-path-scanning)
    - [Terminal Client/Server Deep Bug Audit](#terminal-clientserver-deep-bug-audit)
      - [Bug 1 (Medium): resize\_grid\_preserving\_cells loses line\_copy\_texts when rows shrink and preserve\_bottom is false](#bug-1-medium-resize_grid_preserving_cells-loses-line_copy_texts-when-rows-shrink-and-preserve_bottom-is-false)
      - [Bug 2 (Medium): resize\_normal\_screen\_rows\_only uses last\_significant\_row inconsistently with resize\_grid\_preserving\_cells](#bug-2-medium-resize_normal_screen_rows_only-uses-last_significant_row-inconsistently-with-resize_grid_preserving_cells)
      - [Bug 3 (Low): print uses ' ' for wide-glyph continuation cells instead of \\0 sentinel](#bug-3-low-print-uses---for-wide-glyph-continuation-cells-instead-of-0-sentinel)
      - [Bug 4 (Low): scroll\_region\_up\_from uses explicit indexing on cells\[0\] before remove, risking index-out-of-bounds after concurrent mutation](#bug-4-low-scroll_region_up_from-uses-explicit-indexing-on-cells0-before-remove-risking-index-out-of-bounds-after-concurrent-mutation)
      - [Bug 5 (Low): decode\_standard\_base64 in screen.rs differs from decode\_base64\_transport\_block in agent subsystem](#bug-5-low-decode_standard_base64-in-screenrs-differs-from-decode_base64_transport_block-in-agent-subsystem)
      - [Bug 6 (Low): push\_osc\_char silently truncates OSC payload exceeding MAX\_OSC\_STRING\_BYTES (4096)](#bug-6-low-push_osc_char-silently-truncates-osc-payload-exceeding-max_osc_string_bytes-4096)
      - [Bug 7 (Low): parse\_extended\_sgr\_color accepts CSI 38;2;R;G;B with values \> 255, clamping to 255](#bug-7-low-parse_extended_sgr_color-accepts-csi-382rgb-with-values--255-clamping-to-255)
      - [Bug 8 (Low): classify\_copy\_mode\_key\_action doesn’t handle KeyCode::Char for ctrl modified characters beyond space](#bug-8-low-classify_copy_mode_key_action-doesnt-handle-keycodechar-for-ctrl-modified-characters-beyond-space)
      - [Bug 9 (Low): route\_mouse\_event checks !config.mouse\_pane\_agent\_selector\_cells.is\_empty() before checking actual cell matches](#bug-9-low-route_mouse_event-checks-configmouse_pane_agent_selector_cellsis_empty-before-checking-actual-cell-matches)
      - [Bug 10 (Low): application\_cursor\_forwarding\_bytes maps standard arrow keys CSI A/B/C/D to application-mode SS3 A/B/C/D but doesn’t handle the full range of cursor key sequences](#bug-10-low-application_cursor_forwarding_bytes-maps-standard-arrow-keys-csi-abcd-to-application-mode-ss3-abcd-but-doesnt-handle-the-full-range-of-cursor-key-sequences)
      - [Bug 11 (Low): parse\_key\_chord\_bytes returns Some for bytes 0x01-0x1A (Ctrl+A through Ctrl+Z) but doesn’t validate the input length before indexing input\[0\]](#bug-11-low-parse_key_chord_bytes-returns-some-for-bytes-0x01-0x1a-ctrla-through-ctrlz-but-doesnt-validate-the-input-length-before-indexing-input0)
      - [Bug 12 (Info): modified\_special\_key\_bytes uses u8 for modifier encoding but the modifier formula produces values 1-8 (fits in u8)](#bug-12-info-modified_special_key_bytes-uses-u8-for-modifier-encoding-but-the-modifier-formula-produces-values-1-8-fits-in-u8)
      - [Bug 13 (Low): cursor\_phase\_visible divides by 2 when cursor\_blink\_interval\_ms is odd](#bug-13-low-cursor_phase_visible-divides-by-2-when-cursor_blink_interval_ms-is-odd)
      - [Bug 14 (Low): encode\_attached\_terminal\_output\_update\_frame\_with\_styles segment-update span\_update.len() \< row\_update.len() comparison may produce larger output than a full row update](#bug-14-low-encode_attached_terminal_output_update_frame_with_styles-segment-update-span_updatelen--row_updatelen-comparison-may-produce-larger-output-than-a-full-row-update)
      - [Bug 15 (Info): expand\_changed\_column\_range iterates previous\_spans.iter().chain(spans.iter()) — O(n) per iteration, O(n²) worst case](#bug-15-info-expand_changed_column_range-iterates-previous_spansiterchainspansiter--on-per-iteration-on-worst-case)
    - [Agent Subsystem Deep Bug Audit](#agent-subsystem-deep-bug-audit)
      - [Bug 1 (Medium): AgentTurnLedger::start\_turn allows duplicate pushes without checking existing terminal turns](#bug-1-medium-agentturnledgerstart_turn-allows-duplicate-pushes-without-checking-existing-terminal-turns)
      - [Bug 2 (Medium): Assemble\_model\_request embeds project guidance blocks into system prompt but also emits them as separate user messages](#bug-2-medium-assemble_model_request-embeds-project-guidance-blocks-into-system-prompt-but-also-emits-them-as-separate-user-messages)
      - [Bug 2 (Medium): parse\_maap\_action\_batch\_value silently drops empty say actions but requires at least one action](#bug-2-medium-parse_maap_action_batch_value-silently-drops-empty-say-actions-but-requires-at-least-one-action)
      - [Bug 3 (Low): ShellTransaction::render\_stateful and render\_fish\_stateful don’t include error markers for command presentation](#bug-3-low-shelltransactionrender_stateful-and-render_fish_stateful-dont-include-error-markers-for-command-presentation)
      - [Bug 4 (Low): decode\_base64\_transport\_block silently truncates incomplete base64 quartets on partial blocks](#bug-4-low-decode_base64_transport_block-silently-truncates-incomplete-base64-quartets-on-partial-blocks)
      - [Bug 5 (Low): requires\_bootstrap returns false for unknown signatures when cached](#bug-5-low-requires_bootstrap-returns-false-for-unknown-signatures-when-cached)
      - [Bug 6 (Low): provider\_http\_body\_has\_terminal\_sse\_event uses replace("\\r\\n", "\\n") which allocates per chunk](#bug-6-low-provider_http_body_has_terminal_sse_event-uses-replacern-n-which-allocates-per-chunk)
      - [Bug 7 (Info): Duplicate validation in MaapBatch::validate and parse\_maap\_action\_batch\_value](#bug-7-info-duplicate-validation-in-maapbatchvalidate-and-parse_maap_action_batch_value)
      - [Bug 8 (Info): ACTION\_OUTPUT\_TEXT\_DIFF\_CONTENT\_TYPE defined as text/x-diff but normalize also accepts text/diff](#bug-8-info-action_output_text_diff_content_type-defined-as-textx-diff-but-normalize-also-accepts-textdiff)
      - [Bug 9 (Low): collect\_openai\_maap\_function\_call\_arguments\_from\_accumulators uses is\_none\_or unstable API on stable Rust 2024](#bug-9-low-collect_openai_maap_function_call_arguments_from_accumulators-uses-is_none_or-unstable-api-on-stable-rust-2024)
      - [Bug 10 (Low): openai\_usage\_u64 returns 0 for missing fields; model may silently consume uncounted tokens](#bug-10-low-openai_usage_u64-returns-0-for-missing-fields-model-may-silently-consume-uncounted-tokens)
      - [Bug 11 (Low): optional\_tool\_field returns Some("") for empty strings passed as non-empty](#bug-11-low-optional_tool_field-returns-some-for-empty-strings-passed-as-non-empty)
      - [Bug 12 (Medium): Race between finish\_turn in runner and finish\_turn in ledger: duplicate terminal state transitions possible](#bug-12-medium-race-between-finish_turn-in-runner-and-finish_turn-in-ledger-duplicate-terminal-state-transitions-possible)
      - [Bug 13 (Low): Shell\_transaction\_input.len() saturating\_add may undercount for very large payloads](#bug-13-low-shell_transaction_inputlen-saturating_add-may-undercount-for-very-large-payloads)
    - [Pane Rendering \& Styling Bug Audit](#pane-rendering--styling-bug-audit)
      - [Bug 1 — Wide-glyph sentinel inconsistency (frame.rs:2447-2451)](#bug-1--wide-glyph-sentinel-inconsistency-framers2447-2451)
      - [Bug 2 — Cursor escapes pane when prompt empty (mod.rs:345-365)](#bug-2--cursor-escapes-pane-when-prompt-empty-modrs345-365)
      - [Bug 3 — Division by AGENT\_STATUS\_ANIMATION\_REFRESH\_INTERVAL\_MS (prompt.rs:1199, frame.rs:456)](#bug-3--division-by-agent_status_animation_refresh_interval_ms-promptrs1199-framers456)
      - [Bug 6 — Wide glyphs dropped at column boundaries (prompt.rs:829-862)](#bug-6--wide-glyphs-dropped-at-column-boundaries-promptrs829-862)
      - [Bug 7 — Duplicate color math (theme.rs vs render/style.rs)](#bug-7--duplicate-color-math-themers-vs-renderstylers)
      - [Bug 9 — visible\_thinking\_palette\_hex fallback (theme.rs:946-988)](#bug-9--visible_thinking_palette_hex-fallback-themers946-988)
    - [Pager Rendering \& Styling Bug Audit](#pager-rendering--styling-bug-audit)
      - [Bug 1 (Medium): Wide-glyph sentinel inconsistency in write\_frame\_text\_cells](#bug-1-medium-wide-glyph-sentinel-inconsistency-in-write_frame_text_cells)
      - [Bug 2 (Medium): rendered\_cursor returns (0, 0, false) when agent shell has no prompt lines](#bug-2-medium-rendered_cursor-returns-0-0-false-when-agent-shell-has-no-prompt-lines)
      - [Bug 3 (Low): agent\_live\_footer\_style\_spans unconditional division by AGENT\_STATUS\_ANIMATION\_REFRESH\_INTERVAL\_MS](#bug-3-low-agent_live_footer_style_spans-unconditional-division-by-agent_status_animation_refresh_interval_ms)
      - [Bug 4 (Low): Mouse hit-test u16 overflow in pane\_frame\_agent\_status\_pillbox\_cells](#bug-4-low-mouse-hit-test-u16-overflow-in-pane_frame_agent_status_pillbox_cells)
      - [Bug 5 (Low): style\_span\_segments\_outside\_range may produce zero-length spans](#bug-5-low-style_span_segments_outside_range-may-produce-zero-length-spans)
      - [Bug 6 (Low): write\_line\_segment handles wide glyphs at boundary but may split them](#bug-6-low-write_line_segment-handles-wide-glyphs-at-boundary-but-may-split-them)
      - [Bug 7 (Info): Duplicate color math across theme.rs and render/style.rs](#bug-7-info-duplicate-color-math-across-themers-and-renderstylers)
      - [Bug 8 (Low): readable\_prompt\_shadow\_gray iterates 256 gray levels on every prompt render](#bug-8-low-readable_prompt_shadow_gray-iterates-256-gray-levels-on-every-prompt-render)
      - [Bug 9 (Low): visible\_thinking\_palette\_hex may return unchanged color when no shift meets criteria](#bug-9-low-visible_thinking_palette_hex-may-return-unchanged-color-when-no-shift-meets-criteria)
  - [Closed Issues](#closed-issues)


Work through and solve/implement fixes for open issues and then move them to the closed section.

## Open issues

- Double click to copy does not noticably highlight the copied text. It should be highlighted for at least 200 ms.

### Async Runtime Deep Bug Audit

#### Debuggability Concern 1 (High): tokio::select! Bias Masks Late-Arriving PTY Output
File: src/runtime/service.rs (main event loop tokio::select!)
tokio::select! is biased toward the first branch that becomes ready, evaluated in source order. When multiple
branches are simultaneously ready, the first listed branch wins. In the runtime’s select loop, PTY output
channels are listed after timer and signal branches. If a timer fires and a PTY output event arrives in the same
poll cycle, the timer is processed first, and the PTY output event waits until the next loop iteration.
At 60fps (16ms per frame), this means PTY output could be delayed by up to one frame. This is not a correctness
issue — PTY output is buffered in the unbounded channel and will be processed on the next iteration. But it
violates the principle that interactive terminal output should have scheduling priority over periodic timers.
Severity: Low. One-frame delay (≤16ms) is imperceptible to humans. But if timer handlers perform significant
work (e.g., a slow hook pipeline), PTY output could be delayed longer. The select bias is a design property, not
a configurable fairness policy.
────────

#### Bug 1 (Medium): forward_pty_output spawns unbounded tasks but CancellationToken is the only backpressure mechanism
File: src/runtime/service.rs and src/terminal/fd.rs
The event loop spawns one tokio::spawn task per pane for PTY output forwarding:
tokio::spawn(forward_pty_output(
    pty_reader,
    event_tx.clone(),
    cancellation_token.child_token(),
    window_id,
    pane_id,
));
The forward_pty_output function (fd.rs) loops reading from the PTY and sends Event::WindowPtyOutput through an
UnboundedSender. There is no backpressure — the channel is unbounded. If the main loop falls behind processing
events (e.g., a slow provider HTTP response, a long hook pipeline), the channel accumulates unbounded PTY
output. This is memory-safe only because tokio tasks are cooperative and the event loop will eventually catch
up, but during a provider timeout (30+ seconds), a busy PTY (e.g., cat /dev/urandom or find /) could produce
gigabytes of buffered output.
Severity: Low-Medium. Unbounded channel growth during provider stalls. The practical impact requires both a busy
PTY AND a stalled event loop simultaneously. Mitigation exists via CancellationToken — the spawned task is
cancelled on pane close.
────────

#### Bug 2 (Low): tokio::spawn for PTY forwarding has no JoinHandle stored — orphaned tasks on panic are invisible
File: src/runtime/service.rs
The tokio::spawn return value (JoinHandle) for PTY forwarding tasks is dropped:
tokio::spawn(forward_pty_output(...));  // JoinHandle dropped
If the spawned task panics (e.g., a PTY read error that isn’t caught), the panic is silently swallowed by
tokio’s default panic handler in spawned tasks. The event loop never learns that the PTY reader died. The pane
would stop receiving output with no diagnostic. The cancellation token provides cleanup but cannot signal an
unexpected panic.
Severity: Low. PTY read errors are caught by forward_pty_output’s internal error handling. A panic would require
a programming error (e.g., index out of bounds in the forwarding code). The dropped JoinHandle is an
observability gap.
────────

#### Bug 3 (Medium): flush_paste_buffer uses tokio::time::interval with PASTE_FLUSH_INTERVAL_MS but the timer is not reset when new paste data arrives
File: src/runtime/service.rs and src/terminal/paste.rs
The paste flush timer fires at a fixed interval (default ~10ms). When the timer fires, it flushes accumulated
paste bytes to the active pane’s PTY. But new paste data arriving between timer ticks is queued
(self.paste_buffer.extend_from_slice(&data)) without resetting the timer. This means:
1. Timer fires at T=0ms, flushes buffer (empty)
2. Paste data arrives at T=1ms, queued
3. Timer fires at T=10ms, flushes the 9ms-old data
4. More data arrives at T=11ms
5. Timer fires at T=20ms, flushes the 9ms-old data
Each paste chunk is delayed by up to one interval period. For a 10ms interval, this adds ~10ms latency per
chunk. Interactive paste (from the terminal emulator’s bracketed-paste) is typically a single chunk, so this is
fine. But programmatic paste (scripted input) could produce multiple chunks with 10ms gaps between each.
Severity: Low. Human paste is single-chunk. Programmatic paste is rare. The 10ms gap is imperceptible.
────────

#### Bug 4 (Low): CancellationToken tree uses .child_token() but parent cancellation does not guarantee child task termination before drop
File: src/runtime/service.rs and src/runtime/lifecycle.rs
The runtime creates a tree of cancellation tokens:
let root_token = CancellationToken::new();
let session_token = root_token.child_token();
// ... spawned tasks receive further child tokens
When the session ends, the parent token is cancelled. Child tasks that are still running will observe
cancellation on their next .cancelled() check. However, tokio::spawn tasks may not poll immediately after
cancellation — they’re scheduled cooperatively. If a child task is in the middle of a blocking operation (e.g.,
a read() from a PTY that has no data), the cancellation won’t take effect until the read completes or the
runtime is dropped.
For PTY reads specifically, the forward_pty_output function uses tokio::io::AsyncReadExt::read() which is
cancellation-safe: the read future is dropped when the task is cancelled, closing the read operation. But for
HTTP requests (reqwest), cancellation of the tokio task drops the future, which may leave the TCP connection in
an indeterminate state (connection not gracefully closed).
Severity: Low. Tokio’s async I/O primitives are cancellation-safe (dropping the future closes the underlying
fd). HTTP connection teardown via drop may cause TCP RST instead of FIN, which is acceptable for session
termination.
────────

#### Bug 5 (Low): Provider HTTP reqwest timeout is set via RequestBuilder::timeout() but the tokio::select! wrapping it has no outer timeout
File: src/agent/provider/http.rs
The provider HTTP request uses reqwest’s built-in timeout:
let request = client
    .post(&url)
    .timeout(std::time::Duration::from_secs(timeout_secs))
    .body(body)
    .send();
This timeout covers the initial connection, TLS handshake, and response headers. Once the response stream starts
(streaming SSE), reqwest’s timeout no longer applies — the stream is unbounded in time. If the provider sends
response headers then stalls indefinitely, the runtime hangs waiting for the next SSE event.
Mitigation exists in the provider loop: the response body reader checks CancellationToken periodically (every
chunk read). But the chunk read itself has no timeout — if the provider sends a partial chunk and then stalls,
the read() future never resolves.
Severity: Low-Medium. Provider stalls mid-stream when chunk read hangs can block the agent turn until the
cancellation token fires (pane close or global shutdown). The turn would appear “stuck” with no timeout error.
────────

#### Bug 6 (Low): AgentTurnRunner::run_turn_async calls ledger.start_turn() synchronously before the async provider request, but the async spawn could race with another turn’s start_turn
File: src/runtime/agent/turn_runner.rs
The turn runner checks ledger.start_turn() synchronously (non-async) before spawning the provider task. The
ledger is protected by Arc<tokio::sync::Mutex<AgentTurnLedger>>. The mutex is held only for the duration of
start_turn(), which is fast (validates and pushes a record). After releasing the mutex, the turn runner spawns
the async provider task. If another turn for the same agent starts between the mutex release and the spawn, it
will also pass start_turn() because the first turn is already in Running state and allow_concurrent_turns is the
guard.
With allow_concurrent_turns = false (the default), the second start_turn() would fail with “agent already has a
running turn” — correct. With allow_concurrent_turns = true, both turns proceed, which is the intended behavior.
Severity: Low. The concurrent-turn guard works correctly. The mutex scope is minimal (just the push), which is
the correct async mutex pattern.
────────

#### Bug 7 (Info): Arc<tokio::sync::Mutex<RuntimeService>> wrapping pattern — the entire service is behind a single mutex
File: src/runtime/service.rs
The RuntimeService is shared between the main event loop and hook callbacks via
Arc<tokio::sync::Mutex<RuntimeService>>. This means ALL access to runtime state (config, sessions, windows,
panes, agents, tasks, hooks) is serialized through a single mutex. This is architecturally simple and prevents
data races, but it means:
1. Hook callbacks hold the mutex while executing user-provided code (hooks are async and could be slow)
2. No concurrent access to different subsystems — a hook reading config blocks the paste timer from flushing
3. Provider task results (which modify agent state) must contend with render operations (which read pane screens)
This is a correctness-over-performance tradeoff that is appropriate for a terminal multiplexer (single user, low
throughput). But it means the async concurrency is cooperative I/O only — there is no truly parallel state
access.
Severity: Info. Architectural choice, not a bug. Fine-grained locking could improve throughput but would add
complexity without user-visible benefit for a terminal application.
────────

#### Bug 8 (Low): Signal handler uses tokio::signal::unix::signal() which is Linux/macOS-only — compilation fails on Windows
File: src/runtime/service.rs
The SIGWINCH signal handler uses:
let mut sigwinch = tokio::signal::unix::signal(
    tokio::signal::unix::SignalKind::window_change()
)?;
This is #[cfg(unix)] gated. On Windows, terminal resize events are delivered differently (via the Windows
console API). The code correctly gates the entire signal block behind #[cfg(unix)] with a Windows fallback.
Severity: Info. Correctly gated. Windows support is untested but compiles.
────────

### Runtime Subsystem Deep Bug Audit
Audited the runtime subsystem (~95,100 lines across 55 files). Coverage: lifecycle.rs, mod.rs, service.rs,
types.rs, config.rs, json.rs, hook_pipeline.rs, hooks.rs, hook_support.rs, sockets.rs, auto_sizing.rs, control/
(mod.rs, configuration.rs, subagents.rs), agent/ (all 22 files), render/ (all 8 files), processes/ (mod.rs,
output_filter.rs, transactions.rs), commands/ (mod.rs, compaction.rs, model.rs), commands_support/ (mod.rs,
keybindings.rs, mcp.rs). Test files excluded.
────────

#### Bug 1 (Medium): reset_tick() unconditionally removes the task from tasks hashmap, but tick_once() may have already consumed it
File: src/runtime/service.rs
In the reset_tick() method:
pub fn reset_tick(&mut self) {
    self.tasks.remove(&Task::ResetTick);
}
And in tick_once() around the reset tick handling:
Task::ResetTick => {
    // ... reset sequence state ...
    // Returns without removing task from hashmap in some paths
}
The reset_tick and tick_once methods operate on the same self.tasks HashMap. If tick_once consumes
Task::ResetTick (removes it), then reset_tick() is a no-op. If tick_once does NOT remove it, reset_tick()
correctly cleans up. The inconsistency depends on the tick_once code path, which varies based on session state.
If tick_once returns early from ResetTick handling without removing the task, reset_tick() provides the cleanup.
This dual ownership is fragile — a future code change could cause the task to be removed twice or never.
Severity: Low-Medium. Currently correct due to defensive remove (no-op if absent). The architectural concern is
the split responsibility.
────────

#### Bug 2 (Low): AgentShellTickResult::abort_tick sets RenderedClientView.active_pane_id to the original pane even when the abort fires inside a different pane’s agent
File: src/runtime/service.rs (around line 1450)
The abort_tick short-circuit constructs a new RenderedClientView with active_pane_id from the current window’s
state. If the abort was triggered by a pane different from the active pane (e.g., a background pane’s agent
crashed), the rendered view may reference a stale active pane.
Severity: Low. Abort ticks are transient error-recovery frames. The next successful tick restores the correct
active pane.
────────

#### Bug 3 (Medium): Provider task id field uses ProviderTaskKey which is a Uuid, but ProviderTaskKind in types.rs has no task_id — the id is embedded in the message envelope only
File: src/runtime/types.rs and src/runtime/agent/provider_tasks.rs
ProviderTaskKind variants carry agent_id: AgentId and pane_id: PaneId but not a task UUID. The task UUID is
stored in the ProviderTask wrapper. If a ProviderTaskKind is logged or serialized after being extracted from its
wrapper, the task identity is lost. This means error messages for failed provider tasks may not include the task
UUID, making log correlation difficult.
Severity: Low. Provider task failure diagnostics lose the UUID if the task kind is extracted and logged
separately. Currently mitigated because write_provider_task_kind_to_json (json.rs) writes the full ProviderTask
not just the kind.
────────

#### Bug 4 (Low): Event::WindowPtyOutput includes raw Vec<u8> — if the event is cloned in the hook pipeline, large PTY output buffers are duplicated
File: src/runtime/types.rs (Event enum definition)
WindowPtyOutput {
    window_id: WindowId,
    pane_id: PaneId,
    data: Vec<u8>,
}
The Event type derives Clone. Hook pipeline processing (hooks.rs) passes these events by reference, but any path
that clones the event for async processing duplicates the PTY output buffer. For large paste events or binary
OSC sequences, this doubles memory usage.
Severity: Low. Clone is avoidable in the hook pipeline (it uses references). The Clone derive is a convenience
that could become a problem if future code paths clone large events.
────────

#### Bug 5 (Low): screen_resize_guard in auto_sizing.rs computes window_frame_rows_count but doesn’t account for window frame when the frame position is Bottom
File: src/runtime/auto_sizing.rs
The window frame row count calculation:
let window_frame_rows = if config.window_frame_template.is_empty() { 0 } else { 1 };
This accounts for a top-positioned frame (1 row) but a bottom-positioned frame also consumes 1 row. The
window_frame_position config field (Top/Bottom) is checked elsewhere but the auto-sizing logic appears to assume
the frame is always at the top. A bottom-positioned frame would mean the pane body is sized 1 row too tall.
Severity: Low. Bottom-positioned window frames are rare or unsupported in current config templates. If enabled,
pane geometry would be off by 1 row.
────────

#### Bug 6 (Low): parse_outcome_chunk in presentation.rs uses byte-level scanning for \n but the chunk may contain multi-byte UTF-8 sequences where \n (0x0A) appears as a continuation byte
File: src/runtime/agent/presentation.rs
let line_end = chunk[start..].iter().position(|&b| b == b'\n');
The byte 0x0A can appear as a continuation byte in multi-byte UTF-8 sequences. This would cause a false
line-break mid-character. In practice, model outputs are overwhelmingly ASCII (especially JSON control
sequences), so this is unlikely but technically incorrect.
Severity: Low. Model JSON output is ASCII-safe. Binary or non-Latin content in say text could produce garbled
line splitting.
────────

#### Bug 7 (Low): AgentTaskResult::finalize_with_approval clears the pending_approval flag even when the approval is denied
File: src/runtime/agent/approvals.rs
The approval flow:
1. Task execution produces an action requiring approval → sets pending_approval flag
2. User approves or denies
3. finalize_with_approval is called with the decision
4. Regardless of approved/denied, the flag is cleared
If the user DENIES, the action shouldn’t execute, and the turn should complete with a denial outcome. But the
flag clearing means the next action in the same turn won’t re-prompt. If multiple actions in one turn require
approval, the first denial clears the flag and subsequent actions execute without approval.
Severity: Medium. Multiple-approval turns could see the second+ action bypass approval after the first is
denied. This depends on the action planning step producing multiple tool calls in one batch — which IS possible
in the current MAAP protocol.
────────

#### Bug 8 (Low): ProviderTask::summarize_error truncates error messages at 512 code points via chars().take(512).collect() — loses multi-byte boundary precision
File: src/runtime/agent/provider_execution.rs
let truncated: String = message.chars().take(512).collect();
This is correct — chars() iterates Unicode scalar values, so take(512) produces exactly 512 characters
regardless of byte width. Not a bug on re-read.
────────

#### Bug 9 (Low): SubagentInvocationIndex uses HashMap<String, Vec<SubagentInvocationEntry>> with String keys from turn_id — unbounded growth
File: src/runtime/agent/subagents.rs
The invocation index maps turn_id to invocation history. Old turn entries are never pruned. Over long sessions,
this grows without bound. Each entry is small (~100 bytes), so practical impact is low, but the data structure
has no eviction policy.
Severity: Low. Memory usage grows linearly with total agent turns. At 100K turns × 100 bytes = 10MB, still
negligible for a terminal application.
────────

#### Bug 10 (Info): AgentTurnRecord::reconstructed_input_messages field stores all messages for a turn (including large tool results) — this duplicates the context assembly data
File: src/runtime/agent/bookkeeping.rs
pub reconstructed_input_messages: Vec<ProviderMessage>,
Turn records retain the full reconstructed provider messages. For turns with large tool outputs (e.g., 1MB file
reads), this duplicates the data that’s already stored in the screen’s scrollback and copy-text metadata. The
turn ledger (AgentTurnLedger in the agent subsystem) doesn’t prune old turns — both the turn record AND the
terminal history hold copies.
Severity: Low. Duplicated storage. Turn records are bounded by session lifetime.
────────

#### Bug 11 (Info): window_layout_geometry in types.rs uses usize for row/column indices but PaneGeometry uses u16 — the conversion is as casts with no overflow check
File: src/runtime/types.rs and src/runtime/render/geometry.rs
column: geometry.column as u16,
row: geometry.row as u16,
The as u16 cast silently truncates on terminals wider than 65535 columns. In practice, no terminal is that wide,
but the truncation is silent.
Severity: Low. Implausible terminal sizes. Defensive try_from or saturating would be cleaner.
────────

#### Bug 12 (Medium): dispatch_shell_transaction calls environment_variable_tool_inventory on every shell invocation, which recomputes the tool inventory from env, which, and PATH scanning
File: src/runtime/agent/shell_dispatch.rs
The ToolDiscoveryCache is designed to cache inventories by EnvironmentSignature. But dispatch_shell_transaction
calls requires_bootstrap and maybe_bootstrap which interact with the cache. The actual inventory is not
recomputed from scratch if the cache has a hit. However, the EnvironmentSignature includes the PATH variable —
if PATH changes between invocations (e.g., export PATH=... in a shell command), the signature changes and a full
re-bootstrap occurs. This is correct behavior but could be expensive if PATH frequently changes.
Severity: Low. Correctly handles PATH changes. The cost is intentional.
────────

### Terminal Client/Server Deep Bug Audit
Audited all non-render terminal subsystem files (~12,000 lines) plus previously audited render files. Coverage:
mod.rs, client_loop.rs (2778), screen.rs (3204), keys.rs (1214), mouse.rs (900), fd.rs (963), copy.rs (805),
history.rs (296), paste.rs (628), profile.rs (771), plus render/ (8 files) and theme.rs.
────────

#### Bug 1 (Medium): resize_grid_preserving_cells loses line_copy_texts when rows shrink and preserve_bottom is false
File: src/terminal/screen.rs:957-1001
fn resize_grid_preserving_cells(&mut self, size: Size) {
    // ...
    let preserve_bottom = new_rows < old_rows
        && (self.cursor.row >= new_rows || self.last_significant_row() >= Some(new_rows));
    let row_offset = if preserve_bottom {
        old_rows.saturating_sub(new_rows)
    } else {
        0
    };
    // ... copies cells line-by-line for `rows` rows
}
When new_rows >= old_rows (terminal grew vertically), preserve_bottom is false, and row_offset = 0. All existing
rows are preserved at index 0 onward with their line_copy_texts. This is correct.
But when new_rows < old_rows (terminal shrank) and preserve_bottom is false (cursor is in the top region),
row_offset = 0. The top new_rows are preserved, and the bottom old_rows - new_rows rows are dropped. Their
line_copy_texts are lost without being committed to history. If those rows had Mezzanine copy-text annotations
(from set_recent_normal_copy_texts), the copy-text data is silently discarded.
Severity: Medium. Copy-mode text for agent output at the bottom of a recently-shrunk pane may be lost. The rows
aren’t in scrollback history, so the copy-text is unrecoverable.
────────

#### Bug 2 (Medium): resize_normal_screen_rows_only uses last_significant_row inconsistently with resize_grid_preserving_cells
File: src/terminal/screen.rs:1008-1094
fn resize_normal_screen_rows_only(&mut self, size: Size) {
    let live_bottom = self
        .last_significant_row()
        .map(|row| row.max(self.cursor.row))
        .unwrap_or(self.cursor.row);
    if new_rows < old_rows && live_bottom < new_rows {
        self.resize_grid_preserving_cells(size);
        return;
    }
    let preserve_bottom = new_rows < old_rows && live_bottom >= new_rows;
The resize_grid_preserving_cells fallback path when live_bottom < new_rows uses preserve_bottom differently than
the primary path. In resize_grid_preserving_cells, preserve_bottom is computed as cursor.row >= new_rows ||
last_significant_row() >= Some(new_rows), which checks both cursor AND content. In
resize_normal_screen_rows_only, live_bottom uses last_significant_row().max(cursor.row). When
last_significant_row() is None and the cursor is at row 0, live_bottom = 0, which is < new_rows, so it falls
through to resize_grid_preserving_cells. But the two functions use different logic for where to anchor the
preserved rows. This is an inconsistency in how the cursor-content anchor is chosen but not a correctness bug
per se — both paths converge on the same result for the common case.
Severity: Low. The fallback path in resize_grid_preserving_cells may choose a different anchor than
resize_normal_screen_rows_only would for edge cases where last_significant_row() changes classification.
────────

#### Bug 3 (Low): print uses ' ' for wide-glyph continuation cells instead of \0 sentinel
File: src/terminal/screen.rs:2029-2034
for offset in 1..width {
    let column = self.cursor.column.saturating_add(offset);
    if column <= self.max_column() {
        self.cells[self.cursor.row][column] = ' ';
        self.renditions[self.cursor.row][column] = self.graphic_rendition;
    }
}
The terminal screen uses ' ' (space) for wide-glyph continuation cells. The render subsystem uses \0
(TERMINAL_WIDE_CONTINUATION_CELL) as a sentinel for the same purpose. This is architecturally inconsistent with
the render canvas model, but for the screen’s internal representation (which is consumed by
visible_styled_lines() which trims trailing whitespace, not by collect_text_cells), using ' ' is functionally
correct. The screen’s blank-cell default is ' ' anyway.
Severity: Low. Architectural inconsistency with the render subsystem. Not a runtime bug — the terminal screen
and the render canvas are separate representations consumed through different collection paths.
────────

#### Bug 4 (Low): scroll_region_up_from uses explicit indexing on cells[0] before remove, risking index-out-of-bounds after concurrent mutation
File: src/terminal/screen.rs:2130-2135
if top == 0 && bottom == self.max_row() && self.alternate.should_record_to_history() {
    self.normal_viewport_detached_from_history = false;
    self.history.push_styled_line_with_wrap(
        styled_line_from_row_with_copy_text(
            &self.cells[0],              // line 2135
            &self.renditions[0],
            self.line_copy_texts.first().cloned().flatten(),
        ),
        self.line_wraps.first().copied().unwrap_or(false),
    );
}
self.cells.remove(top);  // line 2143
cells[0] is indexed at line 2135, then cells.remove(top) at line 2143. If top == 0, the [0] access at line 2135
reads the same row that will be removed at 2143. This is correct — the read-then-remove ordering is safe within
the same method. However, the styled_line_from_row_with_copy_text call constructs a TerminalStyledLine from the
row data, which is then pushed to history. If the row has wide glyphens with ' ' sentinel cells (Bug 3), the
copy-text annotation in the history line may not match the logical text.
Severity: Low. No index-out-of-bounds risk — the remove happens after the read. The data flow is correct. Only
the wide-glyph sentinel inconsistency (Bug 3) could affect the pushed history line quality.
────────

#### Bug 5 (Low): decode_standard_base64 in screen.rs differs from decode_base64_transport_block in agent subsystem
File: src/terminal/screen.rs:532-560 and src/agent/actions/shell_transport.rs:83-90
The screen parser’s decode_standard_base64 (used for OSC 52 clipboard decoding) requires complete quartets and
returns None for incomplete trailing bytes:
if quartet_len != 0 {
    return None;  // incomplete quartet → no output
}
Meanwhile, the agent subsystem’s decode_base64_transport_block truncates to complete quartets and returns
partial output. The screen path is more conservative for clipboard data (don’t produce partial content), which
is correct behavior for the OSC 52 use case. Not a bug — just an intentional divergence.
────────

#### Bug 6 (Low): push_osc_char silently truncates OSC payload exceeding MAX_OSC_STRING_BYTES (4096)
File: src/terminal/screen.rs:1822-1825
fn push_osc_char(&mut self, ch: char) {
    if self.osc_buffer.len().saturating_add(ch.len_utf8()) <= MAX_OSC_STRING_BYTES {
        self.osc_buffer.push(ch);
    }
}
If a terminal application emits an OSC string longer than 4096 bytes, the excess characters are silently
dropped. The OSC string will parse with truncated content. For OSC 52 (clipboard), this means large clipboard
payloads will be silently truncated.
Severity: Low-Medium. OSC 52 clipboard sharing is relatively uncommon. The truncation is silent — no diagnostic
event is emitted. Large pastes via OSC 52 would produce corrupted content.
────────

#### Bug 7 (Low): parse_extended_sgr_color accepts CSI 38;2;R;G;B with values > 255, clamping to 255
File: src/terminal/screen.rs:3191-3203
[2, red, green, blue, ..] => Some((
    TerminalColor::Rgb(
        (*red).min(255) as u8,
        (*green).min(255) as u8,
        (*blue).min(255) as u8,
    ),
    4,
)),
SGR true-color parameters > 255 are clamped rather than rejected. Real terminals typically clamp or ignore
these. Mezzanine’s clamping is consistent with common terminal emulator behavior, so this isn’t a practical
issue.
Severity: Low. Clamping matches common terminal behavior.
────────

#### Bug 8 (Low): classify_copy_mode_key_action doesn’t handle KeyCode::Char for ctrl modified characters beyond space
File: src/terminal/client_loop.rs:2510-2543
KeyCode::Char(' ') => Some(CopyModeKeyAction::BeginSelection),
_ => None,  // ctrl-modified chars (e.g., ctrl+u, ctrl+d) fall through to None
In copy mode, Ctrl+U (scroll up half page) and Ctrl+D (scroll down half page) are common tmux conventions but
produce None here, meaning they’re ignored in copy mode. This is a feature gap, not a correctness bug.
Severity: Low. Missing feature, not incorrect behavior. Copy mode forward/backward page operations work via
PageUp/PageDown.
────────

#### Bug 9 (Low): route_mouse_event checks !config.mouse_pane_agent_selector_cells.is_empty() before checking actual cell matches
File: src/terminal/client_loop.rs:1791-1800
if !config.mouse_pane_agent_selector_cells.is_empty()
    && matches!(
        (event.kind, event.button),
        (super::MouseEventKind::Press, super::MouseButton::Left)
    )
{
    return Ok(TerminalClientLoopAction::HandleMouse(
        MouseAction::ClosePaneAgentStatusSelector,
    ));
}
When agent status selector cells are populated, ANY left-press that didn’t match a selector cell closes the
selector. This includes presses on pane dividers, window frames, or even outside the pane region. A press on a
window pill would close the agent status selector AND focus the window (because the press also matches
mouse_window_frame_cells). But since this return happens before the window frame cell check (which is at line
1869), the window frame press is consumed by the selector close action.
Severity: Low. The selector is a transient popup — clicking anywhere else to close it is the expected UX. But it
means a click on a window pill when a selector is open will close the selector instead of focusing the window.
You’d need a second click to actually focus the window.
────────

#### Bug 10 (Low): application_cursor_forwarding_bytes maps standard arrow keys CSI A/B/C/D to application-mode SS3 A/B/C/D but doesn’t handle the full range of cursor key sequences
File: src/terminal/client_loop.rs:2550-2563
match input {
    b"\x1b[A" => Some(b"\x1bOA".to_vec()),
    b"\x1b[B" => Some(b"\x1bOB".to_vec()),
    b"\x1b[C" => Some(b"\x1bOC".to_vec()),
    b"\x1b[D" => Some(b"\x1bOD".to_vec()),
    _ => None,
}
Only unmodified cursor keys are remapped. Modified cursor keys (Ctrl+Up = CSI 1;5A, etc.) pass through
unchanged. This is correct for most applications — they expect to distinguish Ctrl+cursor from plain cursor in
application mode. The function is only invoked when pane_application_cursor_mode is true, which means the pane
application itself requested DECCKM.
Severity: Low. Correct behavior — only remapping unmodified cursor keys to application mode is the expected
terminal emulator convention.
────────

#### Bug 12 (Info): modified_special_key_bytes uses u8 for modifier encoding but the modifier formula produces values 1-8 (fits in u8)
File: src/terminal/keys.rs:758-761
let modifier = 1
    + u8::from(modifiers.shift)
    + (u8::from(modifiers.alt) * 2)
    + (u8::from(modifiers.ctrl) * 4);
Max value: 1 + 1 + 2 + 4 = 8, fits in u8. Not a bug.
────────

#### Bug 13 (Low): cursor_phase_visible divides by 2 when cursor_blink_interval_ms is odd
File: src/terminal/client_loop.rs:1516-1517
let visible_ms = (modes.cursor_blink_interval_ms / 2).max(1);
modes.cursor_blink_elapsed_ms % modes.cursor_blink_interval_ms < visible_ms
For odd intervals (e.g., 501ms), visible_ms = 250 and the modulo wraps at 501. The visible phase is 250ms,
invisible phase is 251ms. This asymmetry (one extra ms of invisible) is imperceptible. Not a bug.
────────

#### Bug 14 (Low): encode_attached_terminal_output_update_frame_with_styles segment-update span_update.len() < row_update.len() comparison may produce larger output than a full row update
File: src/terminal/client_loop.rs:1334-1336
let mut row_update = format!("\x1b[{row};1H\x1b[0m").into_bytes();
row_update.extend_from_slice(encode_styled_terminal_line(line, spans).as_bytes());
(span_update.len() < row_update.len()).then_some(span_update)
The segment update includes the expanded column range (including overlapping style spans), which can grow beyond
the length of a full row rewrite. The span_update.len() < row_update.len() guard correctly falls back to the
full row update when the segment approach is larger. But there’s a subtle case: the span update includes the
\x1b[0m reset but doesn’t account for the fact that a partial-row write may leave subsequent cells with stale
rendition state if the previous frame didn’t reset them. The \x1b[0m before writing the segment should handle
this.
Severity: Low. The \x1b[0m reset before the segment write ensures clean state. The guard correctly avoids larger
segment updates.
────────

#### Bug 15 (Info): expand_changed_column_range iterates previous_spans.iter().chain(spans.iter()) — O(n) per iteration, O(n²) worst case
File: src/terminal/client_loop.rs:1340-1368
loop {
    let mut changed = false;
    for span in previous_spans.iter().chain(spans.iter()) {
        // expand range if overlapping
    }
    if !changed { break; }
}
The loop expands the range each time it finds an overlapping span, which may cause new overlaps. The worst-case
iteration count equals the number of spans. In practice, spans per line are small (1-10), so O(n²) where n ≤ 10
is negligible.
Severity: Info. Not a practical performance issue.
────────

### Agent Subsystem Deep Bug Audit
Audited all source files across src/agent/ (~30+ files, ~10,000+ lines). Findings ordered by severity.
────────

#### Bug 1 (Medium): AgentTurnLedger::start_turn allows duplicate pushes without checking existing terminal turns
File: src/agent/turn.rs:100-112
pub fn start_turn(&mut self, mut turn: AgentTurnRecord) -> Result<()> {
    if !self.allow_concurrent_turns
        && self.turns.iter().any(|existing| {
            existing.agent_id == turn.agent_id && existing.state == AgentTurnState::Running
        })
    {
        return Err(MezError::conflict(
            "agent already has a running turn and concurrent turns are disabled",
        ));
    }
    validate_non_empty("turn_id", &turn.turn_id)?;
    validate_non_empty("agent_id", &turn.agent_id)?;
    validate_non_empty("pane_id", &turn.pane_id)?;
    turn.state = AgentTurnState::Running;
    self.turns.push(turn);
    Ok(())
}
start_turn checks for concurrent Running turns but does not check for duplicate turn_id values (unlike
queue_turn which does). If the same turn_id is start_turn’d twice, two entries with the same ID exist in the
ledger. Later calls to finish_turn / resume_blocked_turn use find() (first match) and mark_turn_running uses
position(), so only the first entry would be updated — the duplicate becomes orphaned but remains in the ledger.
Severity: Medium. Could cause stale turn state if start_turn is called with a reused turn_id (e.g. retry logic).
The caller at AgentTurnRunner::run_turn_async always calls start_turn with a fresh turn, so this is a
defense-in-depth gap rather than a triggered bug in current code.
────────

#### Bug 2 (Medium): Assemble_model_request embeds project guidance blocks into system prompt but also emits them as separate user messages
File: src/agent/context/assembly.rs:47-78
let blocks = prepare_model_context_blocks(context.blocks.clone());
let repository_instruction_blocks = blocks
    .iter()
    .filter(|block| block.source == ContextSourceKind::ProjectGuidance)
    .map(|block| block.content.clone())
    .collect::<Vec<_>>();
// ... project guidance content is embedded in system prompt via repository_instruction_blocks
// ... then later:
for block in &blocks {
    // ...
    if matches!(block.source, ContextSourceKind::ProjectGuidance) {
        continue;  // This correctly skips separate emission
    }
The continue at line 67 correctly skips project guidance blocks from separate messages. No bug here — the block
is embedded in the system prompt only. False positive on re-read. No bug.
────────

#### Bug 2 (Medium): parse_maap_action_batch_value silently drops empty say actions but requires at least one action
File: src/agent/maap.rs:502-508
actions.retain(|action| match &action.payload {
    AgentActionPayload::Say { text, .. } => !text.trim().is_empty(),
    _ => true,
});
if actions.is_empty() {
    return Err(MezError::invalid_args(
        "maap action batch must include at least one action",
    ));
}
When a model emits a say action with empty text, it is silently dropped. If that was the only action, the batch
fails with “must include at least one action”. But if there were other actions plus one empty say, the empty say
is just removed with no diagnostic. The model gets no feedback that its say was malformed.
Severity: Low. Malformed say actions are filtered without repair feedback. Could obscure why model intent wasn’t
rendered.
────────

#### Bug 3 (Low): ShellTransaction::render_stateful and render_fish_stateful don’t include error markers for command presentation
File: src/agent/shell.rs (render_stateful lines ~1035-1070, render_fish_stateful ~1165-1185)
Stateful transaction renderers emit the marker tokens and the raw command inline but do not wrap the command in
the base64 transport used by non-stateful commands. This is intentional (stateful commands run in the parent
shell), but the raw command text is echoed to the PTY before the parent shell executes it. If the command
contains terminal control sequences, they may affect the pane display before the shell actually runs them.
Severity: Low. Stateful commands are typically simple slash-command mutations initiated by the runtime, not
model-authored. The model cannot emit stateful actions directly.
────────

#### Bug 4 (Low): decode_base64_transport_block silently truncates incomplete base64 quartets on partial blocks
File: src/agent/actions/shell_transport.rs:83-90
if partial {
    let full_quartets = cleaned.len() - (cleaned.len() % 4);
    cleaned.truncate(full_quartets);
}
When the base64 block ended before its end marker (partial=true), trailing incomplete quartet bytes are silently
discarded. The decoded output may be truncated mid-stream with no indication that bytes were lost beyond the
[mez: ... base64 transport ended before end marker] note. The note doesn’t say how many base64 bytes were
dropped.
Severity: Low. Partial blocks indicate an interrupted shell transaction, and the truncation at quartet
boundaries is correct base64 handling. The diagnostic could be more informative.
────────

#### Bug 5 (Low): requires_bootstrap returns false for unknown signatures when cached
File: src/agent/shell.rs:1874-1877
pub fn requires_bootstrap(&self, signature: &EnvironmentSignature) -> bool {
    !self.inventories.contains_key(signature)
}
If EnvironmentSignature::unknown() was accidentally stored in the cache (possible through
EnvironmentSignature::unknown() constructor which is public), all subsequent lookups for genuinely unknown
environments would return false, skipping bootstrap.
Severity: Low. EnvironmentSignature::unknown() should not be cached. If a code path calls record with the
unknown sentinel, future bootstraps are suppressed.
────────

#### Bug 6 (Low): provider_http_body_has_terminal_sse_event uses replace("\r\n", "\n") which allocates per chunk
File: src/agent/provider/http.rs:189-201
let body = body.replace("\r\n", "\n");
Called on every chunk in the read loop for SSE detection. At multi-MB response sizes with small chunks, this
allocates frequently. The body is &[u8]/&str transitively but replace always allocates a new String.
Severity: Low (performance). The default provider cap is 16MB. At worst this is O(n) allocations summing to
O(n²) copying over many small chunks. Real-world chunks are typically large enough that this is minor.
────────

#### Bug 7 (Info): Duplicate validation in MaapBatch::validate and parse_maap_action_batch_value
File: src/agent/maap.rs:310-370 and 502-508
MaapBatch::validate checks:
• rationale.trim().is_empty() → error
• At least one non-empty action
parse_maap_action_batch_value also checks:
• rationale.is_empty() (after trim) → error
• actions.is_empty() (after retain for empty says) → error
Both paths perform substantially the same validation at different lifecycle stages. If one check is updated
without the other, they could diverge.
Severity: Info. Code duplication, not a runtime bug. Currently consistent.
────────

#### Bug 8 (Info): ACTION_OUTPUT_TEXT_DIFF_CONTENT_TYPE defined as text/x-diff but normalize also accepts text/diff
File: src/agent/maap.rs:8 and 28-41
pub const AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE: &str = "text/x-diff; charset=utf-8";
normalize_agent_output_content_type accepts both text/diff and text/x-diff but only normalizes to text/x-diff.
This means agent_output_content_type_is_diff returns false for a raw text/diff;charset=utf-8 until after
normalization. Callers using the raw value before normalization would misclassify it.
Severity: Info. All current call sites that care about content_type pass through normalize first. The is_diff
check compares against the normalized constant, so post-normalization it’s correct.
────────

#### Bug 9 (Low): collect_openai_maap_function_call_arguments_from_accumulators uses is_none_or unstable API on stable Rust 2024
File: src/agent/provider/response.rs:222
.filter(|call| {
    call.name
        .as_deref()
        .is_none_or(openai_function_call_name_is_maap)
})
Option::is_none_or was stabilized in Rust 1.82. The project uses Rust 2024 edition which requires 1.85+. This is
fine — just noting the API dependency. Not a bug.
────────

#### Bug 10 (Low): openai_usage_u64 returns 0 for missing fields; model may silently consume uncounted tokens
File: src/agent/provider/response.rs:115-118
fn openai_usage_u64(value: &serde_json::Value, pointers: &[&str]) -> u64 {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_u64))
        .unwrap_or(0)
}
If OpenAI introduces a new usage field shape that doesn’t match any of the known pointers, usage is silently
reported as 0. This is correct defensive behavior but means quota tracking can silently undercount.
Severity: Low. Token accounting is best-effort. Unknown shapes won’t crash, just won’t be counted.
────────

#### Bug 12 (Medium): Race between finish_turn in runner and finish_turn in ledger: duplicate terminal state transitions possible
File: src/agent/actions/runner.rs:103-112 and 133
The runner calls ledger.finish_turn() in failure paths inside the provider loop, then again after the loop at
the final action-planning step:
// Inside loop:
ledger.finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
return Ok(failed_maap_validation_execution_with_summary(...));
// Later, after action planning:
if terminal_state != AgentTurnState::Running {
    ledger.finish_turn(&turn.turn_id, terminal_state)?;
}
If a turn is already in Failed state and finish_turn is called again with Completed, the ledger allows the
transition (it only validates the state is terminal, not that it matches the current state). This means a turn
could transition from Failed → Completed, which is semantically wrong.
Severity: Medium. In practice this can’t happen because the loop exits before reaching the post-loop finish_turn
when a failure occurs (the return Ok(...) exits). But if AgentTurnExecution with terminal_state: Failed is
returned and the caller then calls finish_turn again, the double-finish is possible. The caller contract should
prevent this.
────────

#### Bug 13 (Low): Shell_transaction_input.len() saturating_add may undercount for very large payloads
File: src/agent/shell.rs:527-529
pub fn len(&self) -> usize {
    self.wrapper.len().saturating_add(self.payload.len())
}
saturating_add returns usize::MAX on overflow. For payloads > usize::MAX - wrapper.len(), the reported length is
capped. This is correct behavior — usize::MAX bytes is implausible for shell input. Not a bug.
────────

## Closed Issues

- scrolling in full screen apps like codex does not work. It should.
- Drag copy/paste in pagers like less does not work. It should.
  - This is also broken in nano and other full screen terminal programs.
- mez mcp login does not work properly for the atlassian_rovo MCP. Dig into how codex handles this and implement a fix.
  - It looks like the client_id needs to be randomized
  - Is this an abstract problem that can be solved such that mez does the right thing for all remote MCP servers?
- Styling on text following wide glyphs is shifted left by one. I suspect that the renderer is not accounting for true column width when applying the style overlay.
- The agent prompt height appeared to be shared across panes and caused unrelated prompts to grow in height.
- Link styling (coloring) in the pager view had an off-by-one bug. The styling was shifted left one column when the unicode triangle selector was before it. It rendered correctly when not selected.
  - This was also an issue with normal ANSI colored strings output by shells, so it was a rendering bug or closely related.
- Mouse select copy did not work in the pager view when it should.
- `/` search in the pager view resulted in a full rerender when it should not.
- `/` search highlighting in the pager did not work. Search matches were not highlighted as expected.
- apply patch failures had a retry limit. It displayed in the logs, but it should have been removed and models allowed to retry as many times as needed.
- the cwd view in the window footer status area that is templated in by default should display a maximum of three directory levels at once. if the depth from the home directory or the root is greater than that, then the path should be prefixed iwth an elipsis unicode character. So, ~/Documents/a/b/c/d would become …/b/c/d as an example.
- Double click with the mouse should highlight and copy the surrounding word using readline delimeters as if the user had drag selected it.

- Terminal Client/Server Deep Bug Audit (1 finding):
  - [LOW] parse_key_chord_bytes empty-input contract: made the empty-slice early return explicit before reading the first byte and added a regression test locking in None for empty input. (<pending commit>)

- MAAP Protocol & Action Handler Audit Report (12 findings):
  - [HIGH] Action ID Overwrite in parse_maap_action_batch_value: removed redundant inner synthesized_action_id assignment; outer loop correctly re-indexes after empty-say filtering. (c025f0a)
  - [MEDIUM] Empty Action Batch After Say Filtering Can Bypass Validation: added empty-say-aware counting to MaapBatch::validate() to independently verify non-empty actions. (c025f0a)
  - [MEDIUM] infer_final_turn Does Not Consider Abort as Terminal: added AgentActionPayload::Abort to the terminal action pattern. (c025f0a)
  - [LOW] validate_batch_allowed_actions Skips Complete But Not Abort: added AllowedAction::Abort to action_execution_base(), say_only(), and respond_only() action sets; capability_decision() intentionally excludes abort to preserve repair-negotiation flow. (c025f0a)
  - [LOW] shell_command_summary Falls Back to Action Rationale, Not Batch: renamed parameter from 'rationale' to 'action_rationale' for clarity. (c025f0a)
  - [LOW] required_value Does Not Validate Against JSON Null: improved required_string, required_object, required_array error messages to distinguish null from wrong type. (c025f0a)
  - [LOW] nullable_u64 Requires Field Presence: added rustdoc documenting the distinction and pointing callers to optional_nullable_u64. (c025f0a)
  - [INFO] validate_invariants Allows Running Status with No Content: retained by design (running actions may start with empty content). (c025f0a)

- Agent Subsystem Deep Bug Audit (1 finding):
  - [LOW] optional_tool_field Returns Some("") for Whitespace-only Optional Fields: trim whitespace-only values before deciding Some/None so structured tool probe parsing normalizes blank fields. (76e0db2)
  - [INFO] fetch_url_file_path Strips localhost/ But Not localhost Alone: added explicit error for bare file://localhost. (c025f0a)
  - [INFO] json_escape Replaces Control Characters with Space: replaced silent space corruption with proper \uXXXX escape sequences. (c025f0a)
  - [INFO] ActionContentBlock::to_json Uses Manual JSON Construction: replaced format!() with serde_json::json!(). (c025f0a)
  - [INFO] validate_apply_patch_payload Calls apply_patch_touched_paths But Discards Result: retained by design (validate-then-execute pattern). (c025f0a)

- Shell Command Output Audit (6 findings):
  - [MEDIUM] Line-based wrapper filtering is brittle: refactored mez_wrapper_echo_text_is_hidden to use shared marker lists (WRAPPER_MARKERS, WRAPPER_EXACT_TOKENS, WRAPPER_PREFIXES) instead of duplicated checks per variant. Also fixed unsafe contains("MEZ_COMMAND_") prefix match by using explicit MEZ_COMMAND_FILE, MEZ_COMMAND_B64, and MEZ_COMMAND_ markers.
  - [HIGH] Command echo detection can produce false positives: added found_output state tracking in agent_shell_transaction_observation_bytes so command echo is only hidden before the first legitimate output line appears.
  - [LOW] Prompt detection has false positives: removed broad "repo" substring check from shell_observation_line_has_common_prompt_suffix; retained powerline glyph detection for prompt repaint filtering.
  - [MEDIUM] shell_output_line_is_mezzanine_transport_scaffold is overly aggressive: reduced scaffold filter to only shell block-syntax tokens ({, }, done), removing C, I, o, SY, TC, PS0, and - which are common in legitimate command output (JSON, markdown, diff, single-letter output).
  - [LOW] Base64 transport marker filtering is redundant but fragile: base64 markers must survive agent_shell_transaction_observation_bytes so decode_shell_output_transport can find them; kept the original printf+marker combination check.
  - [HIGH] No structured test coverage for output fidelity: added unit tests for wrapper filtering, prompt detection, command echo state tracking, and scaffold filter behaviour in output_filter.rs.

- Rendering and Style Overlay Audit (7 findings):
  - [MEDIUM] R1 Style span misalignment when wide glyphs span diff boundaries: aligned start_column to the leading cell's column_start so clipped style spans match segment text byte offsets. (client_loop.rs)
  - [LOW] R13 frame elide uses ASCII ... instead of Unicode ellipsis: replaced with U+2026, saving 2 columns of frame space. Also handles narrow-width edge cases (width 0, 1, 2). (framing/render.rs)
  - [LOW] W1 write_single_width_cell only clears one continuation cell: added loop to clear all adjacent continuation cells for emoji ZWJ sequences wider than 2 cells. (text.rs)
  - [LOW] R4 terminal_row_cells returns empty Vec on grapheme-not-found: added debug_assert! with diagnostic context. (client_loop.rs)
  - [LOW] R6 rendition_at_column assumes composition order: documented invariant so callers know spans must be in canvas-composed order. (client_loop.rs)
  - [LOW] Pane-7/Pager-7 duplicate color math: deduplicated terminal_color_rgb, terminal_color_contrast_ratio, terminal_color_relative_luminance, srgb_channel_to_linear, and shifted_channel from theme.rs into shared definitions in render/style.rs, standardizing shifted_channel on i32. (style.rs, theme.rs)
  - [MEDIUM] S2 and R7/W5 already fixed in current code: draw_styled_pane_dividers now calls write_single_width_cell which clears both neighbor cells. rendition_at_column processes spans in reverse composition order so divider spans correctly override pane content spans.
