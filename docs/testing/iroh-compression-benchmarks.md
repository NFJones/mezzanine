# Iroh compression benchmarks

## Purpose

Reproduce application-frame compression measurements used for Iroh rollout
decisions. This benchmark does not measure QUIC or relay network latency; codec
framing is path-independent, while direct and relayed transport checks remain in
the Iroh operations matrix.

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

The reference run met these budgets and supports retaining
`compression_min_bytes = 512` and `compression_zstd_level = 3`. Re-run before
changing defaults, enabling compression broadly, or after codec dependency or
compiler upgrades.
