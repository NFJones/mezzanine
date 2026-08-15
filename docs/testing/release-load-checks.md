# Cross-platform release load checks

## Purpose

Run and interpret the report-only responsiveness workload used to compare
Linux and macOS release builds without treating noisy hosted-runner timing as a
functional correctness gate.

## Prerequisites

Build from the repository root with the Rust toolchain and `just` described in
[Development and validation](../contributing/development-and-validation.md).

Mezzanine runs one identical report-only responsiveness workload on Linux and
macOS. The workload uses a live PTY and the serialized runtime actor to mix a
one-megabyte output flood with pane input, rendered frames, and foreground
process metadata sampling. It runs in release mode so hosted-runner artifacts
are comparable without replacing the existing functional test jobs.

Run the workload locally with:

```text
just release-load-check
```

The default report path is
`target/release-load/<platform>.json`. Set `MEZ_RELEASE_LOAD_REPORT` to choose a
different path. The workload uses the product default of two Tokio worker
threads; set `MEZ_RELEASE_LOAD_WORKERS` to measure another positive count. CI
runs 1, 2, and 4 workers on each supported platform and uploads one
`release-load-<platform>-workers-<count>` artifact per matrix entry.

Compare several worker counts with:

```text
just release-load-sweep
```

The default sweep is `1 2 4`. Override the whitespace-separated list with
`MEZ_RELEASE_LOAD_WORKER_SWEEP`. Each run writes a separate report named with
its worker count.

## Report contract

The JSON report includes:

- platform, architecture, schema version, release-profile identity, and Tokio
  worker count;
- workload dimensions and exact output/input counts;
- total duration, PTY throughput, CPU time, and peak resident memory;
- actor command, event, and side-effect queue counters; and
- p50, p95, p99, and maximum latency for PTY output application, pane input
  application, frame rendering, and process metadata sampling.

The test enforces workload integrity and bounded completion, but
`report_only` remains `true`. It does not compare Linux and macOS against one
absolute threshold because hosted runners have different hardware and noisy
neighbors.

## Calibration policy

Collect repeated artifacts for each platform before adding regression gates.
Use at least 20 successful main-branch samples per platform, discard runs with
documented infrastructure incidents, and calculate each platform's own median
and dispersion. A future gate should require a sustained material regression
across multiple runs rather than fail on one outlier. Version the report schema
when workload dimensions or measurement semantics change.

Keep macOS functional tests serial. The release-load job is isolated so its
measurements are not mixed with the functional suite and so a load failure does
not hide ordinary correctness results.

## Current tuning decision

The product default remains two Tokio worker threads, and the workspace does
not set release-profile overrides. Initial Linux measurements found no stable
responsiveness benefit from increasing the worker count: one, two, and four
workers had similar throughput, CPU, memory, and mixed tail-latency results.
Release-profile experiments also exposed tradeoffs rather than one universal
winner. Keep these settings configurable and collect the cross-platform CI
artifacts before changing defaults.

## Related pages

- [Development and validation](../contributing/development-and-validation.md)
- [Workspace architecture](../contributing/architecture.md)
- [Operations and troubleshooting](../operations/README.md)

## Next step

Compare repeated reports by platform and worker count before proposing a new
runtime default or a regression threshold.
