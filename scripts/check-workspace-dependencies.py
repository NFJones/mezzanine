#!/usr/bin/env python3
"""Reject forbidden dependency edges between Mezzanine workspace crates."""

from __future__ import annotations

import json
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


def workspace_metadata() -> dict[str, object]:
    """Return Cargo metadata for the current workspace or fail visibly."""

    completed = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(completed.stdout)


def main() -> int:
    """Validate package membership and all internal dependency directions."""

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

    print("Mezzanine workspace dependency edges are valid.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
