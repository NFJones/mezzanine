//! Release-mode cross-platform responsiveness load coverage.
//!
//! This ignored test drives the same live PTY, actor event, input, render, and
//! process-metadata workload on Linux and macOS. It emits a machine-readable
//! report for CI comparison, but intentionally enforces only workload
//! correctness until repeated hosted-runner samples establish stable,
//! platform-specific latency thresholds.

use super::super::*;

/// PTY output bytes required before the load sample is considered complete.
const MINIMUM_OUTPUT_BYTES: usize = 1024 * 1024;
/// Input records mixed into the PTY output flood.
const INPUT_RECORDS: usize = 64;
/// Maximum workload iterations before the outer timeout reports a failure.
const MAX_WORKLOAD_ITERATIONS: usize = 4096;
/// Marker emitted after the deterministic PTY output flood.
const OUTPUT_COMPLETE_MARKER: &[u8] = b"release-load-output-done";
/// Marker emitted after the child receives the final input record.
const INPUT_COMPLETE_MARKER: &[u8] = b"ack:done";
/// Product-aligned worker count used when the load check has no override.
const DEFAULT_RELEASE_LOAD_WORKERS: usize = 2;

/// Process resource counters sampled around one release-load execution.
#[derive(Debug, Clone, Copy)]
struct ReleaseLoadResourceUsage {
    /// User and system CPU time consumed by the test process.
    cpu_micros: u64,
    /// Maximum resident set size in bytes.
    max_rss_bytes: u64,
}

/// Returns process CPU and resident-memory counters with normalized units.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn release_load_resource_usage() -> ReleaseLoadResourceUsage {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` receives a valid writable `rusage` allocation and
    // the value is read only when the operating system reports success.
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if status != 0 {
        return ReleaseLoadResourceUsage {
            cpu_micros: 0,
            max_rss_bytes: 0,
        };
    }
    // SAFETY: a successful `getrusage` call initialized the complete value.
    let usage = unsafe { usage.assume_init() };
    let timeval_micros = |value: libc::timeval| {
        u64::try_from(value.tv_sec)
            .unwrap_or(0)
            .saturating_mul(1_000_000)
            .saturating_add(u64::try_from(value.tv_usec).unwrap_or(0))
    };
    let raw_max_rss = u64::try_from(usage.ru_maxrss).unwrap_or(0);
    let max_rss_bytes = if cfg!(target_os = "linux") {
        raw_max_rss.saturating_mul(1024)
    } else {
        raw_max_rss
    };
    ReleaseLoadResourceUsage {
        cpu_micros: timeval_micros(usage.ru_utime).saturating_add(timeval_micros(usage.ru_stime)),
        max_rss_bytes,
    }
}

/// Returns zero resource counters on unsupported development targets.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn release_load_resource_usage() -> ReleaseLoadResourceUsage {
    ReleaseLoadResourceUsage {
        cpu_micros: 0,
        max_rss_bytes: 0,
    }
}

/// Converts one monotonic duration into saturated microseconds.
fn elapsed_micros(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Returns a bounded percentile summary for one nonempty latency sample set.
fn latency_summary(mut samples: Vec<u64>) -> serde_json::Value {
    samples.sort_unstable();
    let percentile = |percent: usize| {
        let index = samples
            .len()
            .saturating_sub(1)
            .saturating_mul(percent)
            .saturating_add(99)
            / 100;
        samples[index.min(samples.len().saturating_sub(1))]
    };
    serde_json::json!({
        "samples": samples.len(),
        "p50_us": percentile(50),
        "p95_us": percentile(95),
        "p99_us": percentile(99),
        "max_us": samples.last().copied().unwrap_or(0),
    })
}

/// Retains only the recent output suffix needed for completion markers.
fn retain_output_tail(tail: &mut Vec<u8>, bytes: &[u8]) {
    const TAIL_BYTES: usize = 1024;
    tail.extend_from_slice(bytes);
    if tail.len() > TAIL_BYTES {
        tail.drain(..tail.len() - TAIL_BYTES);
    }
}

/// Writes one formatted load report to the configured CI artifact path.
fn write_release_load_report(report: &serde_json::Value) {
    let path = std::env::var_os("MEZ_RELEASE_LOAD_REPORT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(format!("target/release-load/{}.json", std::env::consts::OS))
        });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, serde_json::to_vec_pretty(report).unwrap()).unwrap();
    println!("release-load-report={}", path.display());
    println!("{}", serde_json::to_string(report).unwrap());
}

/// Builds the report written before the workload starts so CI retains useful
/// platform and configuration evidence when setup, execution, or timeout
/// handling panics before the successful measurements are available.
fn incomplete_release_load_report(
    requested_runtime_worker_threads: Option<String>,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "platform": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "profile": "release",
        "runtime_worker_threads": null,
        "requested_runtime_worker_threads": requested_runtime_worker_threads,
        "report_only": true,
        "completed": false,
        "diagnostic": "release load workload did not complete; inspect the test log for the primary failure",
    })
}

/// Verifies an incomplete report preserves requested worker configuration and
/// remains explicitly distinguishable from a successful workload artifact.
#[test]
fn incomplete_release_load_report_records_failure_context() {
    let report = incomplete_release_load_report(Some("4".to_string()));

    assert_eq!(report["requested_runtime_worker_threads"], "4");
    assert_eq!(report["runtime_worker_threads"], serde_json::Value::Null);
    assert_eq!(report["completed"], false);
    assert_eq!(report["report_only"], true);
    assert!(
        report["diagnostic"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
}

/// Returns the explicit Tokio worker count selected for this load run.
fn release_load_worker_threads() -> usize {
    let worker_threads = std::env::var("MEZ_RELEASE_LOAD_WORKERS")
        .ok()
        .map_or(Ok(DEFAULT_RELEASE_LOAD_WORKERS), |value| {
            value.parse::<usize>().map_err(|_| value)
        })
        .unwrap_or_else(|value| {
            panic!("MEZ_RELEASE_LOAD_WORKERS must be a positive integer, got {value:?}")
        });
    assert!(
        worker_threads > 0,
        "MEZ_RELEASE_LOAD_WORKERS must be greater than zero"
    );
    worker_threads
}

/// Exercises an identical live PTY output flood, mixed pane input, frame
/// rendering, and process-metadata sampling workload on Linux and macOS.
///
/// The ignored release-mode check records throughput, CPU, peak RSS, and
/// p50/p95/p99 phase latency as JSON. Assertions protect workload integrity,
/// not host-specific performance; CI remains report-only until repeated
/// samples establish stable platform baselines and tolerances.
#[test]
#[ignore = "release-mode cross-platform load check; run with `just release-load-check`"]
fn release_load_reports_cross_platform_pty_responsiveness() {
    write_release_load_report(&incomplete_release_load_report(
        std::env::var("MEZ_RELEASE_LOAD_WORKERS").ok(),
    ));
    let worker_threads = release_load_worker_threads();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let workload = async {
        let mut service = test_service();
        let primary = service
            .attach_primary("release-load", true, Size::new(120, 40).unwrap(), 20_000)
            .unwrap();
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
            .build()
            .unwrap();
        let launch = PaneProcessLaunch::new("/bin/sh".into());
        let process = spawn_pane_process(
            &launch,
            Some(
                "/bin/sh -c 'i=0; while [ \"$i\" -lt 16384 ]; do printf \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n\"; i=$((i + 1)); done; printf \"release-load-output-done\\n\"; while IFS= read -r line; do printf \"ack:%s\\n\" \"$line\"; [ \"$line\" = done ] && break; done; sleep 1'",
            ),
            &test_pane_environment(),
            Size::new(120, 40).unwrap(),
        )
        .unwrap();
        let backend = AsyncPtyPaneProcessIo::new("%1", process).unwrap();
        let mut driver =
            AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
                .unwrap();
        let resources_before = release_load_resource_usage();
        let workload_started = Instant::now();

        let client = async move {
            let mut output_bytes = 0usize;
            let mut output_events = 0usize;
            let mut output_tail = Vec::new();
            let mut input_records = 0usize;
            let mut render_samples = Vec::new();
            let mut input_samples = Vec::new();
            let mut output_samples = Vec::new();
            let mut metadata_samples = Vec::new();
            let mut metadata_observations = 0usize;
            let mut output_complete_seen = false;
            let mut done_sent = false;
            let mut workload_complete = false;

            for iteration in 0..MAX_WORKLOAD_ITERATIONS {
                let output_started = Instant::now();
                let output_event = driver.poll_output_event().await.unwrap();
                let output_ready = output_event.is_some();
                if let Some(event) = output_event {
                    let bytes = match &event {
                        RuntimeEvent::Pane(PaneEvent::Output { bytes, .. }) => bytes,
                        other => panic!("expected pane output event, got {other:?}"),
                    };
                    output_bytes = output_bytes.saturating_add(bytes.len());
                    output_events = output_events.saturating_add(1);
                    retain_output_tail(&mut output_tail, bytes);
                    let mut batch = RuntimeEventBatch::new();
                    batch.push(event);
                    handle.submit_runtime_events(batch).await.unwrap();
                    output_samples.push(elapsed_micros(output_started));

                    if output_events.is_multiple_of(4) {
                        let render_started = Instant::now();
                        handle
                            .render_client_frame(
                                primary.clone(),
                                ClientViewRole::Primary,
                                Size::new(120, 40).unwrap(),
                                TerminalClientLoopConfig::default(),
                                true,
                            )
                            .await
                            .unwrap();
                        render_samples.push(elapsed_micros(render_started));
                    }
                }

                if input_records < INPUT_RECORDS {
                    let input = format!("input-{input_records:03}\n");
                    let input_started = Instant::now();
                    let event = driver.write_input_event(input.as_bytes()).await;
                    assert!(
                        matches!(
                            &event,
                            RuntimeEvent::Pane(PaneEvent::InputWritten { bytes, .. })
                                if *bytes == input.len()
                        ),
                        "unexpected input result: {event:?}"
                    );
                    let mut batch = RuntimeEventBatch::new();
                    batch.push(event);
                    handle.submit_runtime_events(batch).await.unwrap();
                    input_samples.push(elapsed_micros(input_started));
                    input_records = input_records.saturating_add(1);
                }

                if iteration % 16 == 0 {
                    let metadata_started = Instant::now();
                    if let Some(event) = driver.poll_foreground_process_event().await.unwrap() {
                        metadata_observations = metadata_observations.saturating_add(1);
                        let mut batch = RuntimeEventBatch::new();
                        batch.push(event);
                        handle.submit_runtime_events(batch).await.unwrap();
                    }
                    metadata_samples.push(elapsed_micros(metadata_started));
                }

                output_complete_seen |= output_tail
                    .windows(OUTPUT_COMPLETE_MARKER.len())
                    .any(|window| window == OUTPUT_COMPLETE_MARKER);
                if output_complete_seen && input_records == INPUT_RECORDS && !done_sent {
                    let event = driver.write_input_event(b"done\n").await;
                    assert!(matches!(
                        event,
                        RuntimeEvent::Pane(PaneEvent::InputWritten { bytes: 5, .. })
                    ));
                    let mut batch = RuntimeEventBatch::new();
                    batch.push(event);
                    handle.submit_runtime_events(batch).await.unwrap();
                    done_sent = true;
                }
                if done_sent
                    && output_tail
                        .windows(INPUT_COMPLETE_MARKER.len())
                        .any(|window| window == INPUT_COMPLETE_MARKER)
                {
                    workload_complete = true;
                    break;
                }

                if !output_ready && let Some(activity) = driver.output_activity() {
                    let _ = tokio::time::timeout(Duration::from_millis(5), activity).await;
                }
            }

            let termination = driver.terminate_event(true).await;
            let mut batch = RuntimeEventBatch::new();
            batch.push(termination);
            handle.submit_runtime_events(batch).await.unwrap();
            let metrics = handle.metrics().await.unwrap();
            handle.shutdown().await.unwrap();

            assert!(
                workload_complete,
                "release load workload did not complete: output_bytes={output_bytes} \
                 output_events={output_events} input_records={input_records} \
                 done_sent={done_sent} tail={:?}",
                String::from_utf8_lossy(&output_tail)
            );
            assert!(output_bytes >= MINIMUM_OUTPUT_BYTES, "{output_bytes}");
            assert_eq!(input_records, INPUT_RECORDS);
            assert!(!output_samples.is_empty());
            assert!(!render_samples.is_empty());
            assert!(!metadata_samples.is_empty());

            (
                output_bytes,
                output_events,
                metadata_observations,
                output_samples,
                input_samples,
                render_samples,
                metadata_samples,
                metrics,
            )
        };

        let (measurements, mut exit) = tokio::join!(client, actor.run());
        exit.service.terminate_all_pane_processes().unwrap();
        let resources_after = release_load_resource_usage();
        let duration_micros = elapsed_micros(workload_started).max(1);
        let (
            output_bytes,
            output_events,
            metadata_observations,
            output_samples,
            input_samples,
            render_samples,
            metadata_samples,
            metrics,
        ) = measurements;
        let report = serde_json::json!({
            "schema_version": 1,
            "platform": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "profile": "release",
            "runtime_worker_threads": worker_threads,
            "report_only": true,
            "completed": true,
            "diagnostic": null,
            "workload": {
                "minimum_output_bytes": MINIMUM_OUTPUT_BYTES,
                "input_records": INPUT_RECORDS,
                "terminal_columns": 120,
                "terminal_rows": 40,
            },
            "result": {
                "duration_us": duration_micros,
                "output_bytes": output_bytes,
                "output_events": output_events,
                "throughput_bytes_per_second": u64::try_from(output_bytes)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(1_000_000)
                    / duration_micros,
                "metadata_observations": metadata_observations,
                "cpu_time_us": resources_after.cpu_micros
                    .saturating_sub(resources_before.cpu_micros),
                "max_rss_bytes": resources_after.max_rss_bytes,
                "actor_commands": metrics.commands_processed,
                "actor_events_accepted": metrics.runtime_events_accepted,
                "actor_events_applied": metrics.runtime_events_applied,
                "actor_side_effect_queue_high_water": metrics.side_effect_queue_high_water,
            },
            "latency": {
                "pty_output_apply": latency_summary(output_samples),
                "pane_input_apply": latency_summary(input_samples),
                "render_frame": latency_summary(render_samples),
                "process_metadata": latency_summary(metadata_samples),
            },
        });
        write_release_load_report(&report);
    };

        tokio::time::timeout(Duration::from_secs(30), workload)
            .await
            .expect("release load workload exceeded 30 seconds");
    });
}
