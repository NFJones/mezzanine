#!/usr/bin/env python3
"""Reject forbidden dependency edges between Mezzanine workspace crates."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys


EXPECTED_PACKAGES = {
    "mez-agent",
    "mez-core",
    "mez-mux",
    "mez-terminal",
    "mezzanine",
}

ALLOWED_EDGES = {
    "mez-agent": {"mez-core"},
    "mez-core": set(),
    "mez-mux": {"mez-core", "mez-terminal"},
    "mez-terminal": {"mez-core"},
    "mezzanine": {"mez-agent", "mez-core", "mez-mux", "mez-terminal"},
}

REQUIRED_OWNER_PATHS = {
    "crates/mez-agent/src/lib.rs",
    "crates/mez-agent/src/execution.rs",
    "crates/mez-agent/src/shell/mod.rs",
    "crates/mez-agent/src/turn_runner.rs",
    "crates/mez-core/src/ids.rs",
    "crates/mez-mux/src/layout/mod.rs",
    "crates/mez-mux/src/process/mod.rs",
    "crates/mez-mux/src/session/mod.rs",
    "crates/mez-terminal/src/screen.rs",
    "docs/workspace-ownership-matrix.md",
}

RETIRED_COMPATIBILITY_PATHS = {
    "src/agent/shell.rs",
    "src/ids.rs",
    "src/layout.rs",
    "src/layout/mod.rs",
    "src/process.rs",
    "src/process/mod.rs",
    "src/scheduler.rs",
    "src/session.rs",
    "src/session/mod.rs",
}

RETIRED_RUST_IDENTIFIERS = {
    "AgentHarness": "parallel agent acceptance contracts",
}

ROOT_RUNNER_FORBIDDEN_CALLS = {
    "advance_provider_failure(": "provider-failure negotiation",
    "advance_provider_response(": "provider-response negotiation",
    "plan_batch_continuation(": "batch-continuation negotiation",
}


def workspace_metadata() -> dict[str, object]:
    """Return Cargo metadata for the current workspace or fail visibly."""

    completed = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(completed.stdout)


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

    return violations


def main() -> int:
    """Validate package membership, dependency direction, and retired facades."""

    metadata = workspace_metadata()
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
        forbidden = internal_dependencies - ALLOWED_EDGES[package_name]
        for dependency_name in sorted(forbidden):
            violations.append(f"{package_name} -> {dependency_name}")

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

    print("Mezzanine workspace dependency and ownership guardrails are valid.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
