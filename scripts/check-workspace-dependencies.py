#!/usr/bin/env python3
"""Reject forbidden dependency edges between Mezzanine workspace crates."""

from __future__ import annotations

import json
from pathlib import Path
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

EXPECTED_EDGES = {
    "mez-agent": {"mez-core"},
    "mez-core": set(),
    "mez-mux": {"mez-core", "mez-terminal"},
    "mez-terminal": set(),
    "mezzanine": {"mez-agent", "mez-core", "mez-mux", "mez-terminal"},
}

REQUIRED_OWNER_PATHS = {
    "crates/mez-agent/src/lib.rs",
    "crates/mez-agent/src/execution.rs",
    "crates/mez-agent/src/instructions/mod.rs",
    "crates/mez-agent/src/instructions/planning.rs",
    "crates/mez-agent/src/issues/mod.rs",
    "crates/mez-agent/src/issues/types.rs",
    "crates/mez-agent/src/issues/validation.rs",
    "crates/mez-agent/src/messaging/mod.rs",
    "crates/mez-agent/src/messaging/service.rs",
    "crates/mez-agent/src/memory/mod.rs",
    "crates/mez-agent/src/memory/session_store.rs",
    "crates/mez-agent/src/memory/types.rs",
    "crates/mez-agent/src/mcp/mod.rs",
    "crates/mez-agent/src/mcp/protocol.rs",
    "crates/mez-agent/src/mcp/registry.rs",
    "crates/mez-agent/src/mcp/types.rs",
    "crates/mez-agent/src/auto_sizing.rs",
    "crates/mez-agent/src/outcome.rs",
    "crates/mez-agent/src/permissions/mod.rs",
    "crates/mez-agent/src/permissions/classification.rs",
    "crates/mez-agent/src/subagent/scope.rs",
    "crates/mez-agent/src/progress.rs",
    "crates/mez-agent/src/routing.rs",
    "crates/mez-agent/src/shell_observation.rs",
    "crates/mez-agent/src/subagent_output.rs",
    "crates/mez-agent/src/transcript/checkpoint.rs",
    "crates/mez-agent/src/transcript/records.rs",
    "crates/mez-agent/src/transcript/summary.rs",
    "crates/mez-agent/src/shell/mod.rs",
    "crates/mez-agent/src/turn_runner.rs",
    "crates/mez-core/src/ids.rs",
    "crates/mez-mux/src/layout/mod.rs",
    "crates/mez-mux/src/attached_client/mod.rs",
    "crates/mez-mux/src/attached_client/input.rs",
    "crates/mez-mux/src/attached_client/mouse.rs",
    "crates/mez-mux/src/attached_client/output.rs",
    "crates/mez-mux/src/host_input.rs",
    "crates/mez-mux/src/process/mod.rs",
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
    "crates/mez-terminal/src/screen.rs",
    "docs/workspace-ownership-matrix.md",
    "docs/workspace-root-ownership.toml",
}

ROOT_OWNERSHIP_STATES = {"adapter", "product", "temporary"}
ROOT_OWNERSHIP_MANIFEST = Path("docs/workspace-root-ownership.toml")

RETIRED_COMPATIBILITY_PATHS = {
    "src/agent/shell.rs",
    "src/ids.rs",
    "src/layout.rs",
    "src/layout/mod.rs",
    "src/process.rs",
    "src/process/mod.rs",
    "src/readline/prompt_loop.rs",
    "src/scheduler.rs",
    "src/session.rs",
    "src/session/mod.rs",
    "src/terminal/tests/client/io_loop.rs",
}

RETIRED_RUST_IDENTIFIERS = {
    "AgentHarness": "parallel agent acceptance contracts",
    "struct AttachedTerminalFdLoopIo": "test-only synchronous terminal FD loop",
    "ReadlinePromptLoop": "test-only root prompt-loop contracts",
    "run_attached_terminal_client_loop": "test-only synchronous terminal loop",
}

ROOT_RUNNER_FORBIDDEN_CALLS = {
    "advance_provider_failure(": "provider-failure negotiation",
    "advance_provider_response(": "provider-response negotiation",
    "plan_batch_continuation(": "batch-continuation negotiation",
}

ROOT_FORBIDDEN_DECLARATIONS = {
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


def root_source_surfaces() -> set[str]:
    """Return top-level root source surfaces that contain Rust source."""

    surfaces: set[str] = set()
    for path in Path("src").iterdir():
        if path.is_file() and path.suffix == ".rs":
            surfaces.add(path.as_posix())
        elif path.is_dir() and any(path.rglob("*.rs")):
            surfaces.add(path.as_posix())
    return surfaces


def root_ownership_violations() -> tuple[list[str], str, set[str]]:
    """Validate exhaustive root ownership and return its lifecycle state."""

    document = tomllib.loads(ROOT_OWNERSHIP_MANIFEST.read_text(encoding="utf-8"))
    violations: list[str] = []
    if document.get("version") != 1:
        violations.append("root ownership manifest version must be 1")
    status = document.get("status")
    if status not in {"open", "complete"}:
        violations.append("root ownership manifest status must be open or complete")
        status = "invalid"

    entries = document.get("surface")
    if not isinstance(entries, list):
        return (["root ownership manifest must contain [[surface]] entries"], status, set())

    recorded: dict[str, str] = {}
    for entry in entries:
        if not isinstance(entry, dict):
            violations.append("root ownership surface entry must be a table")
            continue
        path = entry.get("path")
        state = entry.get("state")
        role = entry.get("role")
        if not isinstance(path, str) or not path.startswith("src/"):
            violations.append(f"invalid root ownership path: {path!r}")
            continue
        if path in recorded:
            violations.append(f"duplicate root ownership path: {path}")
        if state not in ROOT_OWNERSHIP_STATES:
            violations.append(f"{path}: invalid root ownership state {state!r}")
            continue
        if not isinstance(role, str) or not role.strip():
            violations.append(f"{path}: root ownership role must not be empty")
        recorded[path] = state

    actual = root_source_surfaces()
    missing = actual - recorded.keys()
    stale = recorded.keys() - actual
    for path in sorted(missing):
        violations.append(f"unclassified root source surface: {path}")
    for path in sorted(stale):
        violations.append(f"stale root ownership surface: {path}")

    temporary = {path for path, state in recorded.items() if state == "temporary"}
    if status == "open" and not temporary:
        violations.append("open root decomposition must identify temporary surfaces")
    if status == "complete" and temporary:
        violations.append(
            "complete root decomposition still has temporary surfaces: "
            + ", ".join(sorted(temporary))
        )
    return violations, status, temporary


def source_ownership_violations() -> list[str]:
    """Return source patterns that would restore retired root ownership."""

    violations: list[str] = []
    for path in sorted((*Path("src").rglob("*.rs"), *Path("crates").rglob("*.rs"))):
        source = path.read_text(encoding="utf-8")
        for identifier, ownership in RETIRED_RUST_IDENTIFIERS.items():
            if identifier in source:
                violations.append(f"{path}: retired {ownership} `{identifier}`")

    root_runner = Path("src/agent/actions/runner.rs")
    runner_source = root_runner.read_text(encoding="utf-8")
    for call, ownership in ROOT_RUNNER_FORBIDDEN_CALLS.items():
        if call in runner_source:
            violations.append(f"{root_runner}: lower-owned {ownership} `{call}`")

    for path in sorted(Path("src").rglob("*.rs")):
        source = path.read_text(encoding="utf-8")
        for declaration, ownership in ROOT_FORBIDDEN_DECLARATIONS.items():
            if declaration in source:
                violations.append(f"{path}: lower-owned {ownership} `{declaration}`")
        for line_number, line in enumerate(source.splitlines(), start=1):
            stripped = line.strip()
            if stripped.startswith(("pub use ", "pub(crate) use ")) and any(
                prefix in stripped for prefix in LOWER_CRATE_PREFIXES
            ):
                violations.append(
                    f"{path}:{line_number}: root lower-crate forwarding export `{stripped}`"
                )

    return violations


def main() -> int:
    """Validate package membership, dependency direction, and retired facades."""

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

    violations: list[str] = []
    for package_name, package in packages.items():
        internal_dependencies = {
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency["name"] in EXPECTED_PACKAGES
        }
        forbidden = internal_dependencies - EXPECTED_EDGES[package_name]
        absent = EXPECTED_EDGES[package_name] - internal_dependencies
        for dependency_name in sorted(forbidden):
            violations.append(f"{package_name} -> {dependency_name}")
        for dependency_name in sorted(absent):
            violations.append(f"{package_name} missing -> {dependency_name}")

    if violations:
        print("forbidden Mezzanine workspace dependency edges:")
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

    root_violations, ownership_status, temporary_surfaces = root_ownership_violations()
    if root_violations:
        print("root ownership manifest violations:")
        for violation in root_violations:
            print(f"  {violation}")
        return 1

    ownership_matrix = Path("docs/workspace-ownership-matrix.md").read_text(encoding="utf-8")
    matrix_has_temporary = "| temporary |" in ownership_matrix
    if ownership_status == "open" and not matrix_has_temporary:
        print("open root decomposition is not reflected by temporary matrix boundaries")
        return 1
    if ownership_status == "complete" and matrix_has_temporary:
        print("complete root decomposition matrix still contains temporary boundaries")
        return 1

    restored_facades = sorted(
        path for path in RETIRED_COMPATIBILITY_PATHS if Path(path).exists()
    )
    if restored_facades:
        print("retired root compatibility facades must not be restored:")
        for path in restored_facades:
            print(f"  {path}")
        return 1

    ownership_violations = source_ownership_violations()
    if ownership_violations:
        print("source ownership violations:")
        for violation in ownership_violations:
            print(f"  {violation}")
        return 1

    if ownership_status == "open":
        print(
            "Mezzanine workspace dependency guardrails are valid; "
            f"root decomposition remains open across {len(temporary_surfaces)} surfaces."
        )
    else:
        print("Mezzanine workspace dependency and ownership guardrails are valid.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
