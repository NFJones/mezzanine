#!/usr/bin/env python3
"""Profile Rust libtest cases without charging Cargo startup to each test.

The script asks Cargo to build every workspace test executable once, then runs
each resulting harness serially. Stable libtest flushes ``test ...`` before a
case runs and its status afterward, so timestamping those boundaries produces
useful per-test timings without nightly-only JSON or ``--report-time`` flags.

Reports are written beneath ``target/`` by default. Ordinary ``just test`` runs
remain timing-policy free; this explicit profiler reports tests above the warning
threshold and fails only for harness failures, hangs, or the conservative
critical-duration budget.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import datetime as dt
import json
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import sys
import time
from typing import Any


TEST_START = re.compile(r"(?:^|\n)test (.*?) \.\.\. ")
TEST_END = re.compile(r"(ok|FAILED|ignored)\r?\n")
DEFAULT_REPORT = Path("target/slow-tests/report.json")
OUTPUT_TAIL_BYTES = 128 * 1024


@dataclass(frozen=True)
class TestHarness:
    """One Cargo test executable and its package-root working directory."""

    executable: str
    working_directory: str


class PrettyOutputParser:
    """Timestamp stable libtest pretty-output boundaries for one harness."""

    def __init__(self, executable: str) -> None:
        self.executable = executable
        self.buffer = ""
        self.current: tuple[str, float] | None = None
        self.records: list[dict[str, Any]] = []

    def feed(self, text: str, observed_at: float) -> None:
        """Consume one decoded output chunk observed at a monotonic timestamp."""

        self.buffer += text
        while True:
            if self.current is None:
                match = TEST_START.search(self.buffer)
                if match is None:
                    self.buffer = self.buffer[-4096:]
                    return
                self.current = (match.group(1), observed_at)
                self.buffer = self.buffer[match.end() :]
                continue

            match = TEST_END.search(self.buffer)
            if match is None:
                self.buffer = self.buffer[-4096:]
                return
            name, started_at = self.current
            self.records.append(
                {
                    "executable": self.executable,
                    "test": name,
                    "status": match.group(1),
                    "duration_ms": round((observed_at - started_at) * 1000, 3),
                }
            )
            self.current = None
            self.buffer = self.buffer[match.end() :]


def parse_args() -> argparse.Namespace:
    """Parse profiling thresholds, report location, and bounded debug filters."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--warn-ms",
        type=float,
        default=float(os.environ.get("MEZ_SLOW_TEST_WARN_MS", "500")),
        help="report tests at or above this duration (default: 500)",
    )
    parser.add_argument(
        "--fail-ms",
        type=float,
        default=float(os.environ.get("MEZ_SLOW_TEST_FAIL_MS", "2000")),
        help="fail for tests at or above this duration; 0 disables (default: 2000)",
    )
    parser.add_argument(
        "--report",
        type=Path,
        default=Path(os.environ.get("MEZ_SLOW_TEST_REPORT", DEFAULT_REPORT)),
        help="JSON report path (default: target/slow-tests/report.json)",
    )
    parser.add_argument(
        "--harness-timeout-seconds",
        type=float,
        default=float(os.environ.get("MEZ_SLOW_TEST_HARNESS_TIMEOUT_SECONDS", "600")),
        help="absolute timeout for one test executable (default: 600)",
    )
    parser.add_argument(
        "--binary-filter",
        help="profile only executable basenames containing this text",
    )
    parser.add_argument(
        "--test-filter",
        help="pass one substring filter to each selected libtest harness",
    )
    return parser.parse_args()


def build_test_executables(environment: dict[str, str]) -> list[TestHarness]:
    """Build all workspace tests once and return unique executable harnesses."""

    metadata = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version=1"],
        check=False,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if metadata.returncode != 0:
        sys.stderr.write(metadata.stderr)
        raise RuntimeError("Cargo failed to describe workspace packages")
    package_roots = {
        package["id"]: str(Path(package["manifest_path"]).parent)
        for package in json.loads(metadata.stdout)["packages"]
    }

    command = [
        "cargo",
        "test",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--no-run",
        "--message-format=json",
    ]
    completed = subprocess.run(
        command,
        check=False,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr)
        raise RuntimeError("Cargo failed to build workspace test executables")

    harnesses: list[TestHarness] = []
    seen: set[str] = set()
    for line in completed.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        executable = message.get("executable")
        package_root = package_roots.get(message.get("package_id"))
        if (
            message.get("profile", {}).get("test")
            and executable
            and package_root
            and executable not in seen
        ):
            seen.add(executable)
            harnesses.append(TestHarness(executable, package_root))
    if not harnesses:
        raise RuntimeError("Cargo reported no workspace test executables")
    return harnesses


def terminate_process_group(process: subprocess.Popen[bytes]) -> None:
    """Terminate one timed-out harness and any descendants it still owns."""

    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def run_harness(
    harness: TestHarness,
    test_filter: str | None,
    timeout_seconds: float,
    environment: dict[str, str],
) -> tuple[list[dict[str, Any]], dict[str, Any] | None]:
    """Run one direct libtest harness and return timings plus any failure."""

    executable = harness.executable
    command = [executable, "--test-threads=1", "--format=pretty"]
    if test_filter:
        command.append(test_filter)
    parser = PrettyOutputParser(executable)
    process = subprocess.Popen(
        command,
        cwd=harness.working_directory,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    assert process.stdout is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    started_at = time.monotonic()
    output_tail = b""
    timed_out = False

    while True:
        remaining = timeout_seconds - (time.monotonic() - started_at)
        if remaining <= 0:
            timed_out = True
            terminate_process_group(process)
            break
        ready = selector.select(min(remaining, 1.0))
        if not ready:
            if process.poll() is not None:
                break
            continue
        chunk = os.read(process.stdout.fileno(), 64 * 1024)
        if not chunk:
            break
        observed_at = time.monotonic()
        output_tail = (output_tail + chunk)[-OUTPUT_TAIL_BYTES:]
        parser.feed(chunk.decode("utf-8", errors="replace"), observed_at)

    selector.close()
    return_code = process.wait()
    failure = None
    if timed_out or return_code != 0 or parser.current is not None:
        failure = {
            "executable": executable,
            "working_directory": harness.working_directory,
            "return_code": return_code,
            "timed_out": timed_out,
            "unfinished_test": parser.current[0] if parser.current else None,
            "output_tail": output_tail.decode("utf-8", errors="replace"),
        }
    return parser.records, failure


def write_report(
    args: argparse.Namespace,
    harnesses: list[TestHarness],
    records: list[dict[str, Any]],
    failures: list[dict[str, Any]],
) -> dict[str, Any]:
    """Write the machine-readable report and return its complete payload."""

    slow = sorted(
        (record for record in records if record["duration_ms"] >= args.warn_ms),
        key=lambda record: record["duration_ms"],
        reverse=True,
    )
    violations = (
        []
        if args.fail_ms <= 0
        else [record for record in records if record["duration_ms"] >= args.fail_ms]
    )
    report = {
        "generated_at_utc": dt.datetime.now(dt.UTC).isoformat(),
        "warn_ms": args.warn_ms,
        "fail_ms": args.fail_ms,
        "executables": [harness.executable for harness in harnesses],
        "tests_observed": len(records),
        "slow_tests": slow,
        "budget_violations": violations,
        "harness_failures": failures,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return report


def print_summary(report: dict[str, Any], report_path: Path) -> None:
    """Print a compact human-readable slow-test table."""

    print(f"slow-test report: {report_path}")
    print(f"tests observed: {report['tests_observed']}")
    slow = report["slow_tests"]
    if not slow:
        print("no tests exceeded the warning threshold")
        return
    print("duration_ms\ttest")
    for record in slow:
        print(f"{record['duration_ms']:.3f}\t{record['test']}")


def main() -> int:
    """Build, profile, report, and enforce the explicit regression budget."""

    args = parse_args()
    if args.warn_ms < 0 or args.fail_ms < 0 or args.harness_timeout_seconds <= 0:
        raise ValueError("profiling thresholds must be non-negative and timeout positive")
    environment = os.environ.copy()
    environment["TMPDIR"] = str(Path("/tmp").resolve())
    harnesses = build_test_executables(environment)
    if args.binary_filter:
        harnesses = [
            harness
            for harness in harnesses
            if args.binary_filter in Path(harness.executable).name
        ]
    if not harnesses:
        raise RuntimeError("no test executable matched the requested binary filter")

    records: list[dict[str, Any]] = []
    failures: list[dict[str, Any]] = []
    for index, harness in enumerate(harnesses, start=1):
        print(
            f"[{index}/{len(harnesses)}] {Path(harness.executable).name}",
            flush=True,
        )
        harness_records, failure = run_harness(
            harness,
            args.test_filter,
            args.harness_timeout_seconds,
            environment,
        )
        records.extend(harness_records)
        if failure is not None:
            failures.append(failure)

    report = write_report(args, harnesses, records, failures)
    print_summary(report, args.report)
    if failures:
        print(f"{len(failures)} test harness(es) failed or timed out", file=sys.stderr)
        return 1
    violations = report["budget_violations"]
    if violations:
        print(
            f"{len(violations)} test(s) exceeded the {args.fail_ms:.0f} ms critical budget",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError) as error:
        print(f"slow-test profiler failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
