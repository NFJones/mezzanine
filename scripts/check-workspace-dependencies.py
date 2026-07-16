#!/usr/bin/env python3
"""Validate Mezzanine workspace dependencies and source ownership boundaries."""

from __future__ import annotations

import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib


EXPECTED_PACKAGES = {
    "mez-agent",
    "mez-core",
    "mez-mux",
    "mez-terminal",
    "mezzanine",
}

EXPECTED_MANIFESTS = {
    "mez-agent": Path("crates/mez-agent/Cargo.toml"),
    "mez-core": Path("crates/mez-core/Cargo.toml"),
    "mez-mux": Path("crates/mez-mux/Cargo.toml"),
    "mez-terminal": Path("crates/mez-terminal/Cargo.toml"),
    "mezzanine": Path("crates/mezzanine/Cargo.toml"),
}

EXPECTED_EDGES = {
    "mez-agent": {"mez-core"},
    "mez-core": set(),
    "mez-mux": {"mez-core", "mez-terminal"},
    "mez-terminal": set(),
    "mezzanine": {"mez-agent", "mez-core", "mez-mux", "mez-terminal"},
}

EXPECTED_LOWER_DEPENDENCIES = {
    "mez-agent": {
        "base64",
        "mez-core",
        "rand",
        "serde",
        "serde_json",
        "serde_norway",
        "sha2",
        "shlex",
        "urlencoding",
        "wait-timeout",
    },
    "mez-core": set(),
    "mez-mux": {
        "mez-core",
        "mez-terminal",
        "portable-pty",
        "pulldown-cmark",
        "rustix",
        "shlex",
        "syntect",
        "thiserror",
        "unicode-segmentation",
        "unicode-width",
    },
    "mez-terminal": {"unicode-segmentation", "unicode-width"},
}

LOWER_FORBIDDEN_PRODUCT_IO_DEPENDENCIES = {
    "keyring",
    "keyring-core",
    "reqwest",
    "rusqlite",
    "tokio",
    "zbus-secret-service-keyring-store",
}

LOWER_PLATFORM_DEPENDENCY_OWNERS = {
    "portable-pty": {"mez-mux"},
    "rustix": {"mez-mux"},
}

REQUIRED_OWNER_PATHS = {
    "crates/mez-agent/src/lib.rs",
    "crates/mez-agent/src/macro_workflow.rs",
    "crates/mez-agent/src/execution.rs",
    "crates/mez-agent/src/instructions/mod.rs",
    "crates/mez-agent/src/instructions/planning.rs",
    "crates/mez-agent/src/issues/mod.rs",
    "crates/mez-agent/src/issues/presentation.rs",
    "crates/mez-agent/src/issues/types.rs",
    "crates/mez-agent/src/issues/validation.rs",
    "crates/mez-agent/src/messaging/mod.rs",
    "crates/mez-agent/src/messaging/service.rs",
    "crates/mez-agent/src/memory/mod.rs",
    "crates/mez-agent/src/memory/retrieval.rs",
    "crates/mez-agent/src/memory/action_results.rs",
    "crates/mez-agent/src/memory/session_store.rs",
    "crates/mez-agent/src/memory/types.rs",
    "crates/mez-agent/src/mcp/mod.rs",
    "crates/mez-agent/src/mcp/protocol.rs",
    "crates/mez-agent/src/mcp/registry.rs",
    "crates/mez-agent/src/mcp/types.rs",
    "crates/mez-agent/src/auto_sizing.rs",
    "crates/mez-agent/src/outcome/mod.rs",
    "crates/mez-agent/src/outcome/presentation.rs",
    "crates/mez-agent/src/permissions/mod.rs",
    "crates/mez-agent/src/permissions/classification.rs",
    "crates/mez-agent/src/subagent/scope.rs",
    "crates/mez-agent/src/turn_activity.rs",
    "crates/mez-agent/src/progress.rs",
    "crates/mez-agent/src/routing.rs",
    "crates/mez-agent/src/shell_observation.rs",
    "crates/mez-agent/src/skill_workflow.rs",
    "crates/mez-agent/src/subagent_output.rs",
    "crates/mez-agent/src/transcript/checkpoint.rs",
    "crates/mez-agent/src/transcript/records.rs",
    "crates/mez-agent/src/transcript/summary.rs",
    "crates/mez-agent/src/shell/mod.rs",
    "crates/mez-agent/src/turn_runner.rs",
    "crates/mez-core/src/ids.rs",
    "crates/mez-mux/src/layout/mod.rs",
    "crates/mez-mux/src/command/mod.rs",
    "crates/mez-mux/src/attached_client/mod.rs",
    "crates/mez-mux/src/attached_client/input.rs",
    "crates/mez-mux/src/attached_client/mouse.rs",
    "crates/mez-mux/src/attached_client/output.rs",
    "crates/mez-mux/src/host_input.rs",
    "crates/mez-mux/src/process/mod.rs",
    "crates/mez-mux/src/presentation.rs",
    "crates/mez-mux/src/overlay/mod.rs",
    "crates/mez-mux/src/overlay/interaction.rs",
    "crates/mez-mux/src/overlay/state.rs",
    "crates/mez-mux/src/record_browser.rs",
    "crates/mez-mux/src/readline/tests/buffer.rs",
    "crates/mez-mux/src/readline/tests/prompt.rs",
    "crates/mez-mux/src/render/wrap.rs",
    "crates/mez-mux/src/render/rich_text.rs",
    "crates/mez-mux/src/render/diff.rs",
    "crates/mez-mux/src/render/prompt.rs",
    "crates/mez-mux/src/selector.rs",
    "crates/mez-mux/src/session/mod.rs",
    "crates/mez-terminal/src/screen/mod.rs",
    "crates/mez-terminal/src/screen/cells.rs",
    "crates/mez-terminal/src/screen/content.rs",
    "crates/mez-terminal/src/screen/editing.rs",
    "crates/mez-terminal/src/screen/lifecycle.rs",
    "crates/mez-terminal/src/screen/parser.rs",
    "crates/mez-terminal/src/screen/state.rs",
    "crates/mez-terminal/src/screen/wrap.rs",
    "crates/mez-terminal/src/width.rs",
    "docs/workspace-ownership-matrix.md",
    "docs/workspace-product-ownership.toml",
}

PRODUCT_OWNERSHIP_STATES = {"adapter", "product", "temporary"}
PRODUCT_OWNERSHIP_MANIFEST = Path("docs/workspace-product-ownership.toml")
FINAL_PRODUCT_MANIFEST = Path("crates/mezzanine/Cargo.toml")
FINAL_WORKSPACE_MEMBERS = {
    "crates/mezzanine",
    "crates/mez-agent",
    "crates/mez-core",
    "crates/mez-mux",
    "crates/mez-terminal",
}
EXPECTED_PRODUCT_PRIVATE_MODULES = {
    "cli",
    "config",
    "control",
    "error",
    "host",
    "integrations",
    "protocol",
    "runtime",
    "security",
    "storage",
    "test_support",
    "ui",
}
EXPECTED_PRODUCT_PUBLIC_MODULES = {"control_client"}
EXPECTED_PRODUCT_PUBLIC_FUNCTIONS = {"run_cli"}
EXPECTED_PRODUCT_PUBLIC_USES = {"pub use error::{MezError, MezErrorKind, Result};"}
EXPECTED_RUNTIME_COMPONENT_FIELDS = [
    ("presentation", "RuntimePresentationComponent"),
    ("process", "RuntimeProcessComponent"),
    ("agent", "RuntimeAgentComponent"),
    ("persistence", "RuntimePersistenceComponent"),
    ("control", "RuntimeControlComponent"),
    ("integration", "RuntimeIntegrationComponent"),
    ("session", "RuntimeSessionComponent"),
]
EXPECTED_SHARED_TEST_SUPPORT = {"mod.rs", "runtime.rs"}
MAX_RUST_SOURCE_LINES = 2_000

RETIRED_COMPATIBILITY_PATHS = {
    "agent/shell.rs",
    "agent/semantic/mod.rs",
    "agent/maap.rs",
    "agent/provider/catalog.rs",
    "integrations/agent/shell.rs",
    "integrations/agent/semantic/mod.rs",
    "integrations/agent/maap.rs",
    "integrations/agent/provider/catalog.rs",
    "command/shell.rs",
    "ui/command/shell.rs",
    "ids.rs",
    "layout.rs",
    "layout/mod.rs",
    "process.rs",
    "process/mod.rs",
    "readline/prompt_loop.rs",
    "scheduler.rs",
    "session.rs",
    "session/mod.rs",
    "terminal/tests/client/io_loop.rs",
    "terminal/tests/input/mouse.rs",
}

RETIRED_RUST_IDENTIFIERS = {
    "AgentHarness": "parallel agent acceptance contracts",
    "struct AttachedTerminalFdLoopIo": "test-only synchronous terminal FD loop",
    "ReadlinePromptLoop": "test-only root prompt-loop contracts",
    "run_attached_terminal_client_loop": "test-only synchronous terminal loop",
}

PRODUCT_RUNNER_FORBIDDEN_CALLS = {
    "advance_provider_failure(": "provider-failure negotiation",
    "advance_provider_response(": "provider-response negotiation",
    "plan_batch_continuation(": "batch-continuation negotiation",
}

PRODUCT_FORBIDDEN_DECLARATIONS = {
    "fn action_is_local_shell_backed": "canonical local-action classification",
    "fn action_content_blocks_from_json_or_text": "canonical MCP action content projection",
    "fn apply_patch_touched_paths": "semantic-patch touched-path forwarding",
    "fn apply_patch_write_plan_from_read_output": "semantic-patch plan forwarding",
    "fn apply_patch_write_plan_from_read_outputs": "semantic-patch plan forwarding",
    "fn memory_action_kind": "model-writable memory-kind policy",
    "fn memory_kind_name_for_show": "canonical memory-kind naming",
    "fn memory_source_name_for_show": "canonical memory-source naming",
    "fn memory_state_name_for_show": "canonical memory-state naming",
    "fn pane_content_size_for_geometry": "mux pane-content geometry",
    "fn pane_render_region_size_for_geometry": "mux pane render-region geometry",
    "fn parse_command_sequence": "mux command-language parsing",
    "fn parse_memory_kind_for_show": "canonical memory-kind parsing",
    "fn parse_memory_state_for_show": "canonical memory-state parsing",
    "fn rendered_window_body_size": "mux window-body geometry",
    "fn insert_skill_summary": "canonical skill catalog precedence",
    "fn issue_delete_action_result": "canonical issue action-result projection",
    "fn issue_query_action_result": "canonical issue action-result projection",
    "fn issue_record_action_result": "canonical issue action-result projection",
    "fn issue_record_json": "canonical issue record JSON projection",
    "fn issue_update_action_result": "canonical issue action-result projection",
    "fn local_action_plan": "canonical local-action lowering",
    "fn local_action_summary": "canonical local-action presentation",
    "fn mcp_response_to_action_result": "canonical MCP action-result projection",
    "fn memory_action_content": "canonical memory action content planning",
    "fn memory_action_limit": "canonical memory action limit planning",
    "fn memory_action_preview": "canonical memory action presentation",
    "fn memory_action_record_id": "canonical memory action idempotency",
    "fn compare_memory_search_results": "canonical memory retrieval ordering",
    "fn compare_runtime_memory_results": "canonical memory retrieval ordering",
    "fn compare_search_results": "canonical memory retrieval ordering",
    "fn memory_search_action_result": "canonical memory action-result projection",
    "fn memory_store_action_result": "canonical memory action-result projection",
    "fn memory_store_record": "canonical memory store planning",
    "fn build_memory_store_record": "canonical memory store planning",
    "trait MaapBatchProductValidation": "canonical MAAP harness validation",
    "fn macro_judge_decision_from_text": "canonical macro judge parsing",
    "fn macro_judge_model_request": "canonical macro judge request construction",
    "fn macro_judge_outcome_wire_value": "canonical macro judge outcome naming",
    "fn macro_message_recipient_agent_id": "canonical macro recipient parsing",
    "fn runtime_macro_initial_step_prompt": "canonical macro step prompting",
    "fn runtime_macro_judge_policy": "canonical macro judge policy",
    "fn runtime_macro_judge_task": "canonical macro judge task projection",
    "fn runtime_macro_parent_orchestration_prompt": "canonical macro orchestration prompting",
    "fn runtime_owned_macro_step_model_request": "canonical macro step request projection",
    "fn parse_skill_prompt_invocation": "canonical skill invocation parsing",
    "fn parse_openai_models_http_body": "canonical provider model catalog parsing",
    "const PROVIDER_RETRY_MAX_ATTEMPTS": "canonical provider retry policy",
    "const PROVIDER_RETRY_INITIAL_DELAY_MS": "canonical provider retry policy",
    "const PROVIDER_RETRY_MAX_DELAY_MS": "canonical provider retry policy",
    "struct ProviderRetryPolicy": "canonical provider retry policy",
    "fn agent_provider_retry_delay_ms": "canonical provider retry policy",
    "fn agent_provider_retry_max_attempts": "canonical provider retry policy",
    "fn runtime_action_result_is_suppressed_duplicate_file_mutation": "canonical action duplicate-result policy",
    "fn runtime_action_supports_auto_allow": "canonical action auto-allow policy",
    "fn runtime_execution_is_patch_free": "canonical apply-patch action classification",
    "fn runtime_agent_action_error_suffix": "canonical action outcome presentation",
    "fn runtime_agent_action_has_runtime_visible_effect": "canonical action visibility policy",
    "fn runtime_agent_action_rejects_duplicate_success": "canonical duplicate-action policy",
    "fn runtime_agent_action_rationale_repeats_visible_batch_text": "canonical action rationale suppression",
    "fn runtime_agent_batch_rationale_repeats_visible_batch_text": "canonical batch rationale suppression",
    "fn runtime_agent_batch_visible_action_texts": "canonical visible-action extraction",
    "fn runtime_agent_recoverable_network_warning_line": "canonical recoverable-network presentation",
    "fn runtime_action_pressure_phase": "canonical agent action-pressure policy",
    "fn runtime_action_pressure_severity": "canonical agent action-pressure policy",
    "fn runtime_action_pressure_context_content": "canonical agent action-pressure context",
    "fn runtime_shell_command_looks_like_validation": "canonical validation-command recognition",
    "fn runtime_agent_turn_steering_context_content": "canonical agent steering context",
    "fn runtime_agent_provider_context_usage_snapshot": "canonical provider context accounting",
    "fn runtime_auto_sizing_minimum_context_profile": "canonical auto-sizing context policy",
    "const RUNTIME_AGENT_TURN_TIMEOUT_MS": "canonical agent turn timeout policy",
    "const LOCAL_EXECUTION_DEFAULT_TIMEOUT_MS": "canonical agent turn timeout policy",
    "fn runtime_agent_turn_remaining_timeout_ms": "canonical agent turn timeout policy",
    "fn runtime_shell_action_timeout_ms": "canonical agent shell timeout policy",
    "fn local_execution_turn_remaining_timeout_ms": "canonical agent turn timeout policy",
    "fn local_execution_shell_timeout_ms": "canonical agent shell timeout policy",
    "fn postprocess_semantic_shell_output": "canonical local execution output policy",
    "fn local_output_to_action_result": "canonical local execution result projection",
    "fn local_output_to_action_result_with_transport": "canonical local execution result projection",
    "fn shell_command_result_content": "canonical shell action result projection",
    "fn shell_command_structured_content_json": "canonical shell action structured projection",
    "fn postprocess_shell_action_success_output": "canonical local execution output policy",
    "fn runtime_config_change_value_json": "canonical config-change value normalization",
    "fn runtime_config_change_string_value": "canonical config-change string parsing",
    "fn runtime_config_change_operation_sets_value": "canonical config-change operation normalization",
    "fn runtime_embedded_provider_error": "canonical agent execution failure classification",
    "fn runtime_agent_execution_failure(": "canonical agent execution failure classification",
    "struct RuntimeAgentExecutionFailure": "canonical agent execution failure classification",
    "fn failure_summary_execution_from_response": "canonical failure-summary execution projection",
    "fn current_turn_rationale_entries": "canonical rationale-ledger extraction",
    "fn runtime_agent_terminal_preview": "canonical bounded action preview",
    "fn runtime_agent_user_action_phrase": "canonical action target presentation",
    "fn runtime_fetch_url_status_label": "canonical fetch-result presentation",
    "fn runtime_task_state_suffix": "canonical messaging task-state naming",
    "fn subagent_scope_violation": "canonical subagent action-scope routing",
    "fn runtime_subagent_scope_violation": "canonical subagent action-scope routing",
    "fn maap_spawn_role_for_action": "canonical subagent role normalization",
    "fn maap_read_only_subagent_role_alias": "canonical subagent role normalization",
    "fn normalize_subagent_spawn_role": "canonical subagent role normalization",
    "fn skill_context_text": "canonical loaded-skill context formatting",
    "fn redundant_skill_action_failure": "canonical skill-action planning",
    "struct RuntimeSkillActionContext": "canonical skill-action turn state",
    "fn skill_source_precedence": "canonical skill source precedence",
    "fn split_skill_front_matter": "canonical skill document parsing",
    "fn normalize_agent_user_visible_text": "canonical rationale normalization",
    "fn validate_conversation_id": "canonical transcript conversation-id validation",
    "enum SelectorCandidateKind": "selector candidate category",
    "fn dedupe_selector_candidates": "selector candidate deduplication",
    "fn filter_and_sort_selector_candidates": "selector candidate ranking",
    "fn selector_candidate_prefix_suffix": "selector prefix matching",
    "fn selector_score": "selector candidate scoring",
    "fn selector_token_context": "selector token parsing",
    "fn unescape_selector_shell_token": "selector shell-token normalization",
    "struct ActiveSelector": "active selector state",
    "struct HostBracketedPasteDecoder": "host-input framing state",
    "struct InstructionDiscoveryConfig": "instruction discovery configuration",
    "struct InstructionDiscoveryPlan": "instruction discovery command plan",
    "struct MemoryRetentionPolicy": "canonical memory retention policy",
    "struct MemoryRetrievalRequest": "canonical memory retrieval request",
    "struct MemoryRetrievalResult": "canonical memory retrieval result",
    "struct MemorySearchRequest": "canonical memory search request",
    "struct MemorySearchResult": "canonical memory search result",
    "struct MemoryStoreRecordRequest": "canonical memory store planning",
    "struct RuntimeAgentNetworkActionHistory": "canonical agent network-action history",
    "struct RuntimeAgentShellDispatchHistory": "canonical agent shell-dispatch history",
    "struct RuntimeAgentTurnSteering": "canonical agent turn-steering state",
    "enum SkillSource": "canonical skill source scope",
    "enum MacroJudgeOutcome": "canonical macro judge outcome",
    "enum MacroRunPhase": "canonical macro run phase",
    "struct SkillCatalog": "canonical skill catalog",
    "struct SkillDiagnostic": "canonical skill discovery diagnostic",
    "struct SkillDocument": "canonical loaded skill document",
    "struct SkillPromptInvocation": "canonical skill invocation",
    "struct SkillSummary": "canonical skill summary",
    "struct MacroJudgeDecision": "canonical macro judge decision",
    "struct MacroManagedSubagent": "canonical macro-managed subagent state",
    "struct MacroRunState": "canonical macro run state",
    "struct MacroRunStep": "canonical macro run step state",
    "struct MacroStepTaskResult": "canonical macro child result",
    "enum IssueKind": "canonical issue kind",
    "enum IssueState": "canonical issue state",
    "struct IssueRecord {": "canonical issue record",
    "struct IssueUpdate {": "canonical issue update",
    "struct MessageService": "MMP delivery service state",
    "struct McpRegistry": "canonical MCP registry state",
    "struct McpServerConfig {": "secret-safe MCP server policy",
    "enum McpServerStatus": "canonical MCP availability state",
    "struct McpToolCallPlan {": "canonical MCP tool-call plan",
    "struct McpToolListPagination": "bounded MCP pagination state",
    "enum MemoryScope": "canonical memory scope",
    "struct MemoryRecord {": "canonical memory record",
    "struct SessionMemoryStore": "process-local session memory store",
    "enum TranscriptRole": "canonical transcript role",
    "struct TranscriptEntry {": "canonical transcript entry",
    "struct AgentSessionMetadata {": "agent-session checkpoint record",
    "struct ConversationSummary {": "conversation summary record",
    "struct PermissionPolicy": "deterministic permission policy",
    "struct CommandRule {": "permission command rule contract",
    "struct SelectorCandidate": "selector candidate contract",
    "struct SelectorPlan": "selector replacement plan",
    "struct SelectorShadowHint": "selector shadow-hint contract",
    "struct SelectorTokenContext": "selector token-context contract",
    "trait AsyncMcpActionExecutor": "async MCP execution port",
    "trait LocalActionExecutor": "local action execution port",
    "trait McpActionExecutor": "MCP execution port",
    "trait PaneShellExecutor": "pane shell execution port",
    "trait SubagentScopeEnforcement": "default subagent scope enforcement",
    "fn plan_instruction_discovery": "instruction discovery command planning",
    "fn build_mcp_initialize_request": "MCP JSON-RPC construction",
    "struct RuntimeProviderRegistry": "provider routing registry",
    "struct RuntimeModelPreset": "provider model preset",
    "struct RuntimeAutoSizingDispatch": "auto-sizing dispatch record",
    "struct RuntimeAutoSizingDecision": "auto-sizing decision record",
    "fn runtime_validate_provider_completion_execution": "agent completion policy",
    "fn runtime_progress_say_entries_for_execution": "agent progress policy",
    "fn mez_wrapper_echo_text_is_hidden": "shell observation policy",
    "fn subagent_task_output_for_execution": "subagent result shaping",
    "struct AttachedTerminalOutputFrameState": "attached-terminal retained frame state",
    "fn encode_attached_terminal_output_frame_with_styles": "attached-terminal SGR encoding",
    "fn encode_styled_terminal_line": "attached-terminal styled row encoding",
    "fn application_mouse_forwarding_bytes": "application mouse packet encoding",
    "fn input_sequence_start": "attached-client input boundary planning",
    "fn overlay_text_cells": "mux overlay cell geometry",
    "fn clipped_overlay_style_span": "mux overlay style clipping",
    "fn push_or_extend_style_span": "mux style-span coalescing",
    "fn terminal_color_luminance": "mux color luminance policy",
    "fn wrap_agent_log_physical_line": "generic terminal-cell text wrapping",
    "struct RuntimeRecordBrowser": "record-browser state machine",
    "struct RuntimeRecordBrowserRecord": "neutral record-browser record",
    "struct RuntimeDisplayOverlay": "neutral display-overlay state",
    "struct OverlaySearchMatch": "neutral overlay search range",
    "struct OverlaySelection": "neutral overlay selection",
    "fn runtime_display_overlay_next_search_match": "overlay search policy",
    "fn primary_display_overlay_copy_selection": "overlay copy-selection policy",
    "fn runtime_display_overlay_selection_index_at_position": "overlay hit testing",
    "struct AgentMarkdownRenderer": "CommonMark terminal renderer",
    "struct AgentDiffDisplayLine": "unified-diff display record",
    "struct AgentDiffDisplaySection": "unified-diff section record",
    "fn render_markdown_preserving_source_blank_lines": "CommonMark rendering",
    "fn parse_agent_unified_diff_sections": "unified-diff parsing",
    "fn append_agent_syntax_spans": "syntax-highlight span generation",
    "fn compose_prompt_region(": "neutral prompt-region composition",
    "fn set_terminal_emoji_width": "one-terminal active width policy",
    "fn terminal_grapheme_width": "one-terminal grapheme width measurement",
    "fn terminal_text_width": "one-terminal text width measurement",
    "fn terminal_graphemes": "one-terminal grapheme segmentation",
    "fn compose_display_overlay_lines": "neutral bottom-overlay composition",
    "fn modal_display_overlay_page_rows": "neutral modal-overlay pagination",
    "fn modal_display_overlay_max_scroll": "neutral modal-overlay pagination",
    "fn classify_mouse_event": "neutral attached-client mouse classification",
    "fn pane_border_cells_for_geometries": "mux pane-divider mouse geometry",
    "fn flag_value": "mux command flag lookup",
    "fn positional_args": "mux command positional argument parsing",
    "fn parse_slash_command(": "canonical slash-command parsing",
    "fn effective_provider_api(": "provider API compatibility resolution",
    "fn parse_fenced_maap_action_batch(": "canonical MAAP parsing",
    "fn parse_fenced_maap_action_batch_for_turn(": "canonical turn-aware MAAP parsing",
    "fn parse_maap_action_batch_json(": "canonical MAAP JSON parsing",
    "fn parse_maap_action_batch_json_for_turn(": "canonical turn-aware MAAP JSON parsing",
    "fn network_action_structured_content_json(": "canonical network action result shaping",
}

LOWER_CRATE_PREFIXES = ("mez_agent::", "mez_core::", "mez_mux::", "mez_terminal::")


def workspace_metadata() -> dict[str, object]:
    """Return Cargo metadata for the current workspace or fail visibly."""

    completed = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(completed.stdout)


def product_source_root(metadata: dict[str, object]) -> Path:
    """Return the product source root relative to the repository workspace."""

    workspace_root = Path(str(metadata["workspace_root"])).resolve()
    product_package = next(
        package for package in metadata["packages"] if package["name"] == "mezzanine"
    )
    manifest = Path(product_package["manifest_path"]).resolve()
    return manifest.parent.joinpath("src").relative_to(workspace_root)


def product_source_surfaces(source_root: Path) -> set[str]:
    """Return top-level product source surfaces that contain Rust source."""

    surfaces: set[str] = set()
    for path in source_root.iterdir():
        if path.is_file() and path.suffix == ".rs":
            surfaces.add(path.as_posix())
        elif path.is_dir() and any(path.rglob("*.rs")):
            surfaces.add(path.as_posix())
    return surfaces


def product_ownership_violations(
    source_root: Path,
) -> tuple[list[str], str, set[str]]:
    """Validate exhaustive product ownership and return its lifecycle state."""

    document = tomllib.loads(PRODUCT_OWNERSHIP_MANIFEST.read_text(encoding="utf-8"))
    violations: list[str] = []
    if document.get("version") != 1:
        violations.append("product ownership manifest version must be 1")
    status = document.get("status")
    if status not in {"open", "complete"}:
        violations.append("product ownership manifest status must be open or complete")
        status = "invalid"

    entries = document.get("surface")
    if not isinstance(entries, list):
        return (
            ["product ownership manifest must contain [[surface]] entries"],
            status,
            set(),
        )

    recorded: dict[str, str] = {}
    for entry in entries:
        if not isinstance(entry, dict):
            violations.append("product ownership surface entry must be a table")
            continue
        path = entry.get("path")
        state = entry.get("state")
        role = entry.get("role")
        if not isinstance(path, str) or not path.startswith(f"{source_root.as_posix()}/"):
            violations.append(
                f"invalid product ownership path for {source_root}: {path!r}"
            )
            continue
        if path in recorded:
            violations.append(f"duplicate product ownership path: {path}")
        if state not in PRODUCT_OWNERSHIP_STATES:
            violations.append(f"{path}: invalid product ownership state {state!r}")
            continue
        if not isinstance(role, str) or not role.strip():
            violations.append(f"{path}: product ownership role must not be empty")
        recorded[path] = state

    actual = product_source_surfaces(source_root)
    missing = actual - recorded.keys()
    stale = recorded.keys() - actual
    for path in sorted(missing):
        violations.append(f"unclassified product source surface: {path}")
    for path in sorted(stale):
        violations.append(f"stale product ownership surface: {path}")

    temporary = {path for path, state in recorded.items() if state == "temporary"}
    if status == "open" and not temporary:
        violations.append("open product decomposition must identify temporary surfaces")
    if status == "complete" and temporary:
        violations.append(
            "complete product decomposition still has temporary surfaces: "
            + ", ".join(sorted(temporary))
        )
    return violations, status, temporary


def all_source_paths() -> list[Path]:
    """Return every workspace Rust source file during either migration layout."""

    roots = [Path("crates")]
    if Path("src").is_dir():
        roots.append(Path("src"))
    return sorted(path for root in roots for path in root.rglob("*.rs"))


def source_ownership_violations(product_root: Path) -> list[str]:
    """Return source patterns that would restore retired product ownership."""

    violations: list[str] = []
    for path in all_source_paths():
        source = path.read_text(encoding="utf-8")
        for identifier, ownership in RETIRED_RUST_IDENTIFIERS.items():
            if identifier in source:
                violations.append(f"{path}: retired {ownership} `{identifier}`")

    product_runner = product_root / "integrations/agent/actions/runner.rs"
    if product_runner.is_file():
        runner_source = product_runner.read_text(encoding="utf-8")
        for call, ownership in PRODUCT_RUNNER_FORBIDDEN_CALLS.items():
            if call in runner_source:
                violations.append(
                    f"{product_runner}: lower-owned {ownership} `{call}`"
                )

    for path in sorted(product_root.rglob("*.rs")):
        source = path.read_text(encoding="utf-8")
        for declaration, ownership in PRODUCT_FORBIDDEN_DECLARATIONS.items():
            if declaration in source:
                violations.append(f"{path}: lower-owned {ownership} `{declaration}`")
        for line_number, line in enumerate(source.splitlines(), start=1):
            stripped = line.strip()
            if stripped.startswith(("pub use ", "pub(crate) use ", "pub(super) use ")) and any(
                prefix in stripped for prefix in LOWER_CRATE_PREFIXES
            ):
                violations.append(
                    f"{path}:{line_number}: product lower-crate forwarding export `{stripped}`"
                )

    facade_roots = (
        path
        for surface in (
            product_root / "agent",
            product_root / "runtime",
            product_root / "terminal",
            product_root / "integrations/agent",
            product_root / "host/terminal",
        )
        if surface.is_dir()
        for path in surface.rglob("mod.rs")
        if "tests" not in path.parts
    )
    for path in sorted(facade_roots):
        for line_number, line in enumerate(
            path.read_text(encoding="utf-8").splitlines(), start=1
        ):
            stripped = line.strip()
            if line.startswith("use ") and stripped.endswith("::*;"):
                violations.append(
                    f"{path}:{line_number}: production module facade wildcard import `{stripped}`"
                )

    return violations


def source_structure_violations() -> list[str]:
    """Reject oversized source units and flattened module implementations."""

    violations: list[str] = []
    for path in all_source_paths():
        source = path.read_text(encoding="utf-8")
        line_count = len(source.splitlines())
        if line_count > MAX_RUST_SOURCE_LINES:
            violations.append(
                f"{path}: {line_count} lines exceeds the {MAX_RUST_SOURCE_LINES}-line limit"
            )
        if re.search(r"\binclude!\s*\(", source):
            violations.append(f"{path}: `include!` must not flatten Rust module ownership")
        if "tests" in path.parts and re.fullmatch(r"(?:part|chunk)_?\d+\.rs", path.name):
            violations.append(f"{path}: numbered test chunks do not express ownership")
    return violations


def final_layout_violations(
    metadata: dict[str, object], product_root: Path, ownership_status: str
) -> list[str]:
    """Return target-layout violations once decomposition is marked complete."""

    if ownership_status != "complete":
        return []

    violations: list[str] = []
    workspace_root = Path(str(metadata["workspace_root"])).resolve()
    product_package = next(
        package for package in metadata["packages"] if package["name"] == "mezzanine"
    )
    product_manifest = Path(product_package["manifest_path"]).resolve()
    expected_manifest = workspace_root / FINAL_PRODUCT_MANIFEST
    if product_manifest != expected_manifest:
        violations.append(
            "complete product decomposition requires the mezzanine manifest at "
            f"{FINAL_PRODUCT_MANIFEST}, found {product_manifest.relative_to(workspace_root)}"
        )
    if product_root != FINAL_PRODUCT_MANIFEST.parent / "src":
        violations.append(
            "complete product decomposition requires source under "
            f"{FINAL_PRODUCT_MANIFEST.parent / 'src'}"
        )
    for retired_root in (Path("src"), Path("tests")):
        if retired_root.exists():
            violations.append(
                f"complete product decomposition must remove root {retired_root}/"
            )

    root_manifest = tomllib.loads(Path("Cargo.toml").read_text(encoding="utf-8"))
    package_keys = {
        "package",
        "dependencies",
        "dev-dependencies",
        "build-dependencies",
        "target",
        "lib",
        "bin",
        "features",
    }
    if package_keys & root_manifest.keys():
        violations.append("complete decomposition requires a virtual root Cargo.toml")
    workspace_members = set(root_manifest.get("workspace", {}).get("members", []))
    if workspace_members != FINAL_WORKSPACE_MEMBERS:
        violations.append(
            "complete decomposition requires these explicit workspace members: "
            + ", ".join(sorted(FINAL_WORKSPACE_MEMBERS))
        )

    product_lib = product_root / "lib.rs"
    lib_source = product_lib.read_text(encoding="utf-8")
    private_modules = set(
        re.findall(r"^mod\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*;", lib_source, re.MULTILINE)
    )
    public_modules = set(
        re.findall(
            r"^pub\s+mod\s+([a-zA-Z_][a-zA-Z0-9_]*)\b", lib_source, re.MULTILINE
        )
    )
    public_functions = set(
        re.findall(
            r"^pub\s+(?:async\s+)?fn\s+([a-zA-Z_][a-zA-Z0-9_]*)\b",
            lib_source,
            re.MULTILINE,
        )
    )
    public_uses = {
        line.strip()
        for line in lib_source.splitlines()
        if line.startswith("pub use ")
    }
    unsupported_public_items = [
        f"{line_number}:{line.strip()}"
        for line_number, line in enumerate(lib_source.splitlines(), start=1)
        if line.startswith("pub ")
        and not line.startswith(("pub mod ", "pub use ", "pub fn ", "pub async fn "))
    ]
    if private_modules != EXPECTED_PRODUCT_PRIVATE_MODULES:
        violations.append(
            "product lib private modules differ from the admitted application surfaces: "
            f"expected {sorted(EXPECTED_PRODUCT_PRIVATE_MODULES)}, "
            f"found {sorted(private_modules)}"
        )
    if public_modules != EXPECTED_PRODUCT_PUBLIC_MODULES:
        violations.append(
            "product lib public modules must remain the supported control-client surface: "
            f"found {sorted(public_modules)}"
        )
    if public_functions != EXPECTED_PRODUCT_PUBLIC_FUNCTIONS:
        violations.append(
            "product lib public functions must remain the CLI bootstrap only: "
            f"found {sorted(public_functions)}"
        )
    if public_uses != EXPECTED_PRODUCT_PUBLIC_USES:
        violations.append(
            "product lib public re-exports must remain the product error surface: "
            f"found {sorted(public_uses)}"
        )
    if unsupported_public_items:
        violations.append(
            "product lib contains unsupported public items: "
            + ", ".join(unsupported_public_items)
        )

    runtime_source_path = product_root / "runtime/mod.rs"
    runtime_source = runtime_source_path.read_text(encoding="utf-8")
    runtime_service = re.search(
        r"pub struct RuntimeSessionService\s*\{(?P<body>.*?)^\}",
        runtime_source,
        re.MULTILINE | re.DOTALL,
    )
    if runtime_service is None:
        violations.append(f"{runtime_source_path}: missing RuntimeSessionService coordinator")
    else:
        runtime_fields = re.findall(
            r"^\s{4}([a-zA-Z_][a-zA-Z0-9_]*)\s*:\s*"
            r"([a-zA-Z_][a-zA-Z0-9_]*)\s*,\s*$",
            runtime_service.group("body"),
            re.MULTILINE,
        )
        if runtime_fields != EXPECTED_RUNTIME_COMPONENT_FIELDS:
            violations.append(
                f"{runtime_source_path}: runtime coordinator fields must be exactly "
                f"{EXPECTED_RUNTIME_COMPONENT_FIELDS}, found {runtime_fields}"
            )

    test_support_root = product_root / "test_support"
    shared_test_support = {
        path.relative_to(test_support_root).as_posix()
        for path in test_support_root.rglob("*.rs")
    }
    if shared_test_support != EXPECTED_SHARED_TEST_SUPPORT:
        violations.append(
            "shared application test support must contain only multi-owner runtime fixtures: "
            f"found {sorted(shared_test_support)}"
        )

    for path in sorted(product_root.rglob("*.rs")):
        if "tests" in path.parts or path.name.endswith("_tests.rs"):
            continue
        for line_number, line in enumerate(
            path.read_text(encoding="utf-8").splitlines(), start=1
        ):
            if line.strip() == "use super::*;":
                violations.append(
                    f"{path}:{line_number}: production code must use explicit imports"
                )
            if "pub(in crate::runtime)" in line:
                violations.append(
                    f"{path}:{line_number}: runtime component state must be private"
                )
    return violations


def open_decomposition_metrics(product_root: Path) -> tuple[int, int]:
    """Return broad-import and runtime-field counts for visible open-state evidence."""

    wildcard_imports = 0
    runtime_fields = 0
    for path in sorted(product_root.rglob("*.rs")):
        if "tests" in path.parts or path.name.endswith("_tests.rs"):
            continue
        source = path.read_text(encoding="utf-8")
        wildcard_imports += sum(
            line.strip() == "use super::*;" for line in source.splitlines()
        )
        runtime_fields += len(
            re.findall(r"^\s*pub\(in crate::runtime\)\s+\w+\s*:", source, re.MULTILINE)
        )
    return wildcard_imports, runtime_fields


def main() -> int:
    """Validate package graph, final ownership, and source structure."""

    metadata = workspace_metadata()
    workspace_member_ids = set(metadata["workspace_members"])
    workspace_packages = {
        package["name"]
        for package in metadata["packages"]
        if package["id"] in workspace_member_ids
    }
    unexpected = workspace_packages - EXPECTED_PACKAGES
    if unexpected:
        print(f"unexpected Mezzanine workspace packages: {', '.join(sorted(unexpected))}")
        return 1

    packages = {
        package["name"]: package
        for package in metadata["packages"]
        if package["name"] in EXPECTED_PACKAGES
    }

    missing = EXPECTED_PACKAGES - packages.keys()
    if missing:
        print(f"missing Mezzanine workspace packages: {', '.join(sorted(missing))}")
        return 1

    product_root = product_source_root(metadata)
    workspace_root = Path(str(metadata["workspace_root"])).resolve()

    violations: list[str] = []
    for package_name, package in packages.items():
        manifest_path = Path(str(package["manifest_path"])).resolve()
        expected_manifest = workspace_root / EXPECTED_MANIFESTS[package_name]
        if manifest_path != expected_manifest:
            violations.append(
                f"{package_name} manifest must be {EXPECTED_MANIFESTS[package_name]}, "
                f"found {manifest_path.relative_to(workspace_root)}"
            )
        dependency_names = {dependency["name"] for dependency in package["dependencies"]}
        internal_dependencies = dependency_names & EXPECTED_PACKAGES
        forbidden = internal_dependencies - EXPECTED_EDGES[package_name]
        absent = EXPECTED_EDGES[package_name] - internal_dependencies
        for dependency_name in sorted(forbidden):
            violations.append(f"{package_name} -> {dependency_name}")
        for dependency_name in sorted(absent):
            violations.append(f"{package_name} missing -> {dependency_name}")
        if package_name != "mezzanine":
            expected_dependencies = EXPECTED_LOWER_DEPENDENCIES[package_name]
            unexpected_dependencies = dependency_names - expected_dependencies
            missing_dependencies = expected_dependencies - dependency_names
            for dependency_name in sorted(unexpected_dependencies):
                violations.append(
                    f"{package_name} has unapproved dependency {dependency_name}"
                )
            for dependency_name in sorted(missing_dependencies):
                violations.append(
                    f"{package_name} is missing approved dependency {dependency_name}"
                )
            for dependency_name in sorted(
                dependency_names & LOWER_FORBIDDEN_PRODUCT_IO_DEPENDENCIES
            ):
                violations.append(
                    f"{package_name} -> {dependency_name} "
                    "(product I/O dependencies belong in mezzanine)"
                )
            for dependency_name, allowed_owners in LOWER_PLATFORM_DEPENDENCY_OWNERS.items():
                if dependency_name in dependency_names and package_name not in allowed_owners:
                    violations.append(
                        f"{package_name} -> {dependency_name} "
                        "(platform dependency is outside its approved owner)"
                    )

    if violations:
        print("Mezzanine package or dependency boundary violations:")
        for violation in violations:
            print(f"  {violation}")
        return 1

    missing_owner_paths = sorted(
        path for path in REQUIRED_OWNER_PATHS if not Path(path).is_file()
    )
    if missing_owner_paths:
        print("missing required workspace owner paths:")
        for path in missing_owner_paths:
            print(f"  {path}")
        return 1

    product_violations, ownership_status, temporary_surfaces = (
        product_ownership_violations(product_root)
    )
    if product_violations:
        print("product ownership manifest violations:")
        for violation in product_violations:
            print(f"  {violation}")
        return 1

    ownership_matrix = Path("docs/workspace-ownership-matrix.md").read_text(encoding="utf-8")
    matrix_has_temporary = "| temporary |" in ownership_matrix
    if ownership_status == "open" and not matrix_has_temporary:
        print("open product decomposition is not reflected by temporary matrix boundaries")
        return 1
    if ownership_status == "complete" and matrix_has_temporary:
        print("complete product decomposition matrix still contains temporary boundaries")
        return 1

    restored_facades = sorted(
        product_root / path
        for path in RETIRED_COMPATIBILITY_PATHS
        if (product_root / path).exists()
    )
    if restored_facades:
        print("retired product compatibility facades must not be restored:")
        for path in restored_facades:
            print(f"  {path}")
        return 1

    ownership_violations = source_ownership_violations(product_root)
    if ownership_violations:
        print("source ownership violations:")
        for violation in ownership_violations:
            print(f"  {violation}")
        return 1

    structure_violations = source_structure_violations()
    if structure_violations:
        print("source structure violations:")
        for violation in structure_violations:
            print(f"  {violation}")
        return 1

    layout_violations = final_layout_violations(
        metadata, product_root, ownership_status
    )
    if layout_violations:
        print("completed product layout violations:")
        for violation in layout_violations:
            print(f"  {violation}")
        return 1

    if ownership_status == "open":
        wildcard_imports, runtime_fields = open_decomposition_metrics(product_root)
        print(
            "Mezzanine workspace dependency guardrails are valid; "
            f"product decomposition remains open across {len(temporary_surfaces)} "
            f"surfaces ({wildcard_imports} broad production imports and "
            f"{runtime_fields} crate-visible runtime fields remain)."
        )
    else:
        print("Mezzanine workspace dependency and ownership guardrails are valid.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
