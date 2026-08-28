# Iroh compression benchmarks

## Purpose

Reproduce application-frame compression measurements used for Iroh rollout
decisions. This benchmark does not measure QUIC or relay network latency; codec
framing is path-independent, while direct and relayed transport checks remain in
the Iroh operations matrix.

## Prerequisites

Run from the repository root with the Rust toolchain and `just` described in
[Development and validation](../contributing/development-and-validation.md).
Use a release build on an otherwise reasonably idle machine when comparing
throughput across changes.

## Run

```text
just iroh-compression-bench
```

The release-mode, single-threaded ignored test writes
`target/iroh-compression-bench.json`. Set
`MEZ_IROH_COMPRESSION_BENCH_REPORT` to choose another output path.

The report compares `none`, zstd level 3, and LZ4 with the 512-byte threshold
across small control, repetitive terminal, JSON/configuration, deterministic
incompressible, and bidirectional attach/event fixtures. For each pair it records
operations, elapsed time, nanoseconds per operation, decoded throughput,
allocation count and bytes, decoded bytes, wire bytes, and wire ratio.

## Budgets and interpretation

- Below-threshold frames must bypass codec work.
- Compression must fall back to identity when encoded payload bytes would not
  be smaller; a v2 identity envelope still adds its fixed 16-byte header.
- Compressible fixtures should remain at or below 15% wire/decoded size.
- A comparable release run should sustain at least 250 MiB/s for zstd and
  4 GiB/s for LZ4 on the compressible fixtures.
- Absolute timing and allocation totals are report-only because allocator,
  compiler, CPU, and operating-system versions vary.

## Interactive latency interpretation

OpenSSH keeps compression state in the connection's per-direction packet
state instead of treating compression as a reason to delay interactive packet
delivery. Mezzanine follows the latency-relevant part of that design: every
complete control frame remains independently decodable and is flushed to QUIC
immediately. It does not wait to accumulate a larger compression batch.

The protocols differ above that boundary. SSH pushes terminal channel bytes.
Legacy Iroh event streams use redraw wakeups followed by an authoritative view
fetch, while negotiated v3 streams push exact-client snapshots or deltas.
Consequently, legacy visible choppiness can come from redundant view round
trips even when codec work is small. V3 removes that fetch while retaining one
independently decodable, immediately flushed envelope per update.

The OpenSSH implementation reference used for this comparison is
[`packet.c`](https://github.com/openssh/openssh-portable/blob/master/packet.c),
whose session packet state owns separate incoming and outgoing zlib contexts.

The reference run met these budgets and supports retaining
`compression_min_bytes = 512` and `compression_zstd_level = 3`. Re-run before
changing defaults, enabling compression broadly, or after codec dependency or
compiler upgrades.

## Related pages

- [Iroh render-update benchmarks](iroh-render-update-benchmarks.md)
- [Iroh production operations and rollout](../operations/iroh-production-operations-and-rollout.md)
- [Cross-platform release load checks](release-load-checks.md)
- [Development and validation](../contributing/development-and-validation.md)

## Next step

Retain the generated report under `target/`, record the environment used for
the run, and compare like-for-like release measurements before changing codec
defaults.
