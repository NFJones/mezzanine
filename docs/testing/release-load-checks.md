# Cross-platform release load checks

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
different path. CI uploads one `release-load-<platform>` artifact per matrix
entry.

## Report contract

The JSON report includes:

- platform, architecture, schema version, and release-profile identity;
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
