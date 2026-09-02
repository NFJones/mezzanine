//! Iroh endpoint construction and lifecycle for optional remote control.

use base64::Engine as _;
use iroh::address_lookup::{DnsAddressLookup, PkarrPublisher};
use iroh::endpoint::{
    BindOpts, IdleTimeout, PortmapperConfig, QuicTransportConfig, VarInt, presets,
};
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, RelayMap, RelayMode, SecretKey, Watcher};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::task::JoinSet;

use crate::control::{AuthenticatedPeer, ControlConnectionState, encode_control_body};
use crate::error::{MezError, Result};
use crate::host::async_runtime::{
    AsyncRuntimeControlConnectionConfig, AsyncRuntimeService, AsyncRuntimeServiceExit,
    AsyncRuntimeSessionHandle,
    serve_authenticated_async_runtime_control_connection_loop_with_snapshots_hooks_and_cancellation,
};
use crate::protocol::event::{EventKind, encode_event_notification};
use crate::security::remote::{RemotePrincipal, RemoteTrustStore};
use crate::storage::snapshot::SnapshotRepository;
use mez_core::ids::ClientId;
use tokio::io::AsyncWriteExt;

use super::config::{
    RuntimeIrohAddressLookupPolicy, RuntimeIrohCompressionCodec, RuntimeIrohRelayPolicy,
    RuntimeIrohTransportPolicy,
};
use super::{
    IrohCompressionBridge, IrohCompressionMetrics, IrohCompressionPolicy, IrohFrameCompressionMode,
    IrohStreamEncoder,
};

/// ALPN identity for the first Mezzanine Iroh transport contract.
pub(crate) const MEZZANINE_IROH_ALPN: &[u8] = b"mezzanine/transport/1";
pub(crate) const MEZZANINE_IROH_EVENT_STREAM_PREFACE: &[u8] = b"mezzanine/events/1\n";
pub(crate) const MEZZANINE_IROH_EVENT_STREAM_V2_PREFACE: &[u8] = b"mezzanine/events/2\n";
pub(crate) const MEZZANINE_IROH_EVENT_STREAM_V3_PREFACE: &[u8] = b"mezzanine/events/3\n";
const IROH_EVENT_BATCH_LIMIT: usize = 64;
const IROH_CLIPBOARD_CHUNK_BYTES: usize = 256 * 1024;

/// Bounded render triggers collected after the previous v3 update completes.
#[derive(Debug, Default, PartialEq, Eq)]
struct IrohReadyRenderTriggers {
    last_event_id: Option<u64>,
    events: usize,
    render_invalidations: usize,
    render_required: bool,
    invalidate_output: bool,
    classification_uncertain: bool,
}

impl IrohReadyRenderTriggers {
    /// Merges one authorized event slice without retaining rendered state.
    fn absorb_event_wakeups(&mut self, wakeups: Vec<super::RuntimeEventWakeup>) -> usize {
        let mut batch_events = 0usize;
        for wakeup in wakeups {
            for event in wakeup.events {
                if let Some(invalidate) = iroh_event_render_invalidation(event.kind) {
                    self.render_required = true;
                    self.invalidate_output |= invalidate;
                }
                self.last_event_id = Some(
                    self.last_event_id
                        .map_or(event.id, |last_event_id| last_event_id.max(event.id)),
                );
                self.events = self.events.saturating_add(1);
                batch_events = batch_events.saturating_add(1);
            }
        }
        batch_events
    }

    /// Merges exact-client render invalidations into the strongest requirement.
    fn absorb_render_side_effects(&mut self, effects: &[super::RuntimeSideEffect]) {
        self.render_invalidations = self.render_invalidations.saturating_add(effects.len());
        if let Some(invalidate_output) = iroh_side_effect_render_invalidation(effects) {
            self.render_required = true;
            self.invalidate_output |= invalidate_output;
        }
    }

    /// Returns the bounded number of ready triggers represented by this update.
    fn ready_depth(&self) -> usize {
        self.events.saturating_add(self.render_invalidations)
    }

    /// Forces a conservative invalidating snapshot when a ready range is unsafe.
    fn mark_classification_uncertain(&mut self) {
        self.classification_uncertain = true;
        self.render_required = true;
        self.invalidate_output = true;
    }
}

/// Drains all currently ready v3 triggers before rendering latest state once.
async fn collect_ready_iroh_render_triggers(
    handle: &AsyncRuntimeSessionHandle,
    caller_client_id: &ClientId,
    connection_id: &str,
    last_delivered_event_id: u64,
    initial: Vec<super::RuntimeEventWakeup>,
) -> Result<IrohReadyRenderTriggers> {
    let mut triggers = IrohReadyRenderTriggers::default();
    let mut next = initial;
    loop {
        let batch_events = triggers.absorb_event_wakeups(next);
        if batch_events == 0 || batch_events < IROH_EVENT_BATCH_LIMIT {
            break;
        }
        if triggers.events >= crate::control::MAX_EVENT_REPLAY_RETENTION {
            triggers.mark_classification_uncertain();
            break;
        }
        next = handle
            .event_wakeups_for_client(
                caller_client_id.clone(),
                connection_id.to_string(),
                triggers.last_event_id.unwrap_or(last_delivered_event_id),
                IROH_EVENT_BATCH_LIMIT,
            )
            .await?;
    }

    let mut drained_side_effects = 0usize;
    loop {
        let effects = handle
            .drain_render_side_effects_for_client(caller_client_id.clone(), IROH_EVENT_BATCH_LIMIT)
            .await?;
        if effects.is_empty() {
            break;
        }
        drained_side_effects = drained_side_effects.saturating_add(effects.len());
        triggers.absorb_render_side_effects(&effects);
        if effects.len() < IROH_EVENT_BATCH_LIMIT {
            break;
        }
        if drained_side_effects >= crate::control::MAX_EVENT_REPLAY_RETENTION {
            triggers.mark_classification_uncertain();
            break;
        }
    }
    Ok(triggers)
}

/// Serializes the complete logical view retained by one v3 render stream.
fn iroh_render_view_value(
    snapshot: &crate::host::async_runtime::AsyncIrohRenderSnapshot,
) -> Result<serde_json::Value> {
    serde_json::from_str(&super::rendered_client_view_json(
        &snapshot.view,
        snapshot.iroh_status_slot.as_ref(),
    ))
    .map_err(|error| MezError::invalid_state(format!("failed to encode Iroh render view: {error}")))
}

/// Encodes one authoritative primary render snapshot for event-stream v3.
fn encode_iroh_render_snapshot_frame(
    snapshot: &crate::host::async_runtime::AsyncIrohRenderSnapshot,
    revision: u64,
    view: &serde_json::Value,
) -> Vec<u8> {
    encode_control_body(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "render/snapshot",
            "params": {
                "kind": "snapshot",
                "revision": revision,
                "event_cutoff": snapshot.event_cutoff,
                "invalidate_output": snapshot.invalidate_output,
                "view": view,
            }
        })
        .to_string(),
    )
}

/// Content-free accounting for one selected v3 render update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IrohRenderUpdateKind {
    Snapshot,
    Delta,
}

/// Selected render frame plus the complete logical base retained after flush.
#[derive(Debug)]
struct IrohRenderUpdateFrame {
    frame: Vec<u8>,
    view: serde_json::Value,
    kind: IrohRenderUpdateKind,
    changed_rows: usize,
    snapshot_candidate_bytes: usize,
}

impl IrohRenderUpdateFrame {
    fn snapshot(frame: Vec<u8>, view: serde_json::Value) -> Self {
        let changed_rows = view
            .get("lines")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        let snapshot_candidate_bytes = frame.len();
        Self {
            frame,
            view,
            kind: IrohRenderUpdateKind::Snapshot,
            changed_rows,
            snapshot_candidate_bytes,
        }
    }
}

/// Encodes a whole-row delta when it is safe and smaller than a snapshot.
///
/// The returned complete view becomes the next server base only after the
/// caller successfully writes and flushes the selected frame.
fn encode_iroh_render_update_frame(
    snapshot: &crate::host::async_runtime::AsyncIrohRenderSnapshot,
    base_view: Option<&serde_json::Value>,
    base_revision: u64,
    revision: u64,
) -> Result<Option<IrohRenderUpdateFrame>> {
    let view = iroh_render_view_value(snapshot)?;
    if base_view == Some(&view) && !snapshot.invalidate_output {
        return Ok(None);
    }
    let snapshot_frame = encode_iroh_render_snapshot_frame(snapshot, revision, &view);
    let Some(base_view) = base_view else {
        return Ok(Some(IrohRenderUpdateFrame::snapshot(snapshot_frame, view)));
    };
    if snapshot.invalidate_output {
        return Ok(Some(IrohRenderUpdateFrame::snapshot(snapshot_frame, view)));
    }
    let (Some(base), Some(current)) = (base_view.as_object(), view.as_object()) else {
        return Ok(Some(IrohRenderUpdateFrame::snapshot(snapshot_frame, view)));
    };
    let (Some(base_lines), Some(base_styles), Some(lines), Some(styles)) = (
        base.get("lines").and_then(serde_json::Value::as_array),
        base.get("line_style_spans")
            .and_then(serde_json::Value::as_array),
        current.get("lines").and_then(serde_json::Value::as_array),
        current
            .get("line_style_spans")
            .and_then(serde_json::Value::as_array),
    ) else {
        return Ok(Some(IrohRenderUpdateFrame::snapshot(snapshot_frame, view)));
    };
    if lines.len() != styles.len()
        || base_lines.len() != base_styles.len()
        || lines.len() != base_lines.len()
    {
        return Ok(Some(IrohRenderUpdateFrame::snapshot(snapshot_frame, view)));
    }

    let rows = lines
        .iter()
        .zip(styles)
        .enumerate()
        .filter(|(index, (line, style_spans))| {
            base_lines[*index] != **line || base_styles[*index] != **style_spans
        })
        .map(|(index, (line, style_spans))| {
            serde_json::json!({
                "index": index,
                "line": line,
                "style_spans": style_spans,
            })
        })
        .collect::<Vec<_>>();
    let mut metadata = current.clone();
    metadata.remove("lines");
    metadata.remove("line_style_spans");
    let delta_frame = encode_control_body(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "render/delta",
            "params": {
                "kind": "delta",
                "base_revision": base_revision,
                "revision": revision,
                "event_cutoff": snapshot.event_cutoff,
                "invalidate_output": false,
                "line_count": lines.len(),
                "view": metadata,
                "rows": rows,
            }
        })
        .to_string(),
    );
    let selected = if delta_frame.len() < snapshot_frame.len() {
        IrohRenderUpdateFrame {
            snapshot_candidate_bytes: snapshot_frame.len(),
            changed_rows: rows.len(),
            kind: IrohRenderUpdateKind::Delta,
            frame: delta_frame,
            view,
        }
    } else {
        IrohRenderUpdateFrame::snapshot(snapshot_frame, view)
    };
    Ok(Some(selected))
}

/// Classifies one retained event for primary v3 render ownership.
fn iroh_event_render_invalidation(kind: EventKind) -> Option<bool> {
    match kind {
        EventKind::Diagnostic | EventKind::SnapshotChanged => None,
        EventKind::ClientAttached
        | EventKind::ClientDetached
        | EventKind::ConfigChanged
        | EventKind::WindowChanged => Some(false),
        EventKind::PaneChanged
        | EventKind::AgentStatus
        | EventKind::Message
        | EventKind::ApprovalChanged
        | EventKind::McpServerChanged
        | EventKind::HookFailed => Some(false),
    }
}

/// Classifies exact-client render side effects for primary v3 snapshot push.
///
/// Logical recomposition preserves the attached terminal's retained output
/// frame. Only an exact-client geometry change makes that physical frame
/// unsafe to use as the differential-rendering base.
fn iroh_side_effect_render_invalidation(effects: &[super::RuntimeSideEffect]) -> Option<bool> {
    let mut render_required = false;
    let mut invalidate_output = false;
    for effect in effects {
        let super::RuntimeSideEffect::RenderClient { reason, .. } = effect else {
            continue;
        };
        render_required = true;
        invalidate_output |= matches!(reason, super::RenderInvalidationReason::Resize);
    }
    render_required.then_some(invalidate_output)
}

/// Encodes one bounded clipboard write as transient version-two notifications.
fn encode_iroh_clipboard_effect_frames(write: &super::ClientClipboardWrite) -> Vec<Vec<u8>> {
    let content = write.content().as_bytes();
    let chunk_count = content.len().div_ceil(IROH_CLIPBOARD_CHUNK_BYTES).max(1);
    let mut frames = Vec::with_capacity(chunk_count.saturating_add(2));
    let begin = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "client/clipboard.begin",
        "params": {
            "sequence": write.sequence(),
            "total_bytes": write.byte_len(),
            "chunks": chunk_count,
        }
    });
    frames.push(encode_control_body(&begin.to_string()));
    let chunks = if content.is_empty() {
        vec![content]
    } else {
        content.chunks(IROH_CLIPBOARD_CHUNK_BYTES).collect()
    };
    for (index, chunk) in chunks.into_iter().enumerate() {
        let chunk = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "client/clipboard.chunk",
            "params": {
                "sequence": write.sequence(),
                "index": index,
                "data_base64": base64::engine::general_purpose::STANDARD.encode(chunk),
            }
        });
        frames.push(encode_control_body(&chunk.to_string()));
    }
    let commit = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "client/clipboard.commit",
        "params": {"sequence": write.sequence()}
    });
    frames.push(encode_control_body(&commit.to_string()));
    frames
}

/// Privacy-safe aggregate diagnostics for one supervised Iroh listener.
#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeIrohDiagnostics {
    inner: Arc<RuntimeIrohDiagnosticsInner>,
}

#[derive(Debug, Default)]
struct RuntimeIrohDiagnosticsInner {
    listener_active: AtomicBool,
    active_connections: AtomicUsize,
    connections_accepted: AtomicU64,
    connections_rejected: AtomicU64,
    setup_successes: AtomicU64,
    setup_failures: AtomicU64,
    setup_latency_total_millis: AtomicU64,
    setup_latency_max_millis: AtomicU64,
    connections_completed: AtomicU64,
    connections_failed: AtomicU64,
    direct_connections: AtomicU64,
    relay_connections: AtomicU64,
    custom_connections: AtomicU64,
    unknown_connections: AtomicU64,
    shutdown_aborts: AtomicU64,
    last_path: AtomicU8,
    connection_quality: Mutex<BTreeMap<String, RuntimeIrohConnectionQualitySnapshot>>,
}

/// Privacy-safe selected-path measurements for one initialized Iroh client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeIrohConnectionQualitySnapshot {
    pub(crate) connected_millis: u64,
    pub(crate) sampled_at: Instant,
    pub(crate) rtt_micros: u64,
    pub(crate) average_rtt_micros: u64,
    pub(crate) jitter_micros: u64,
    pub(crate) tx_bytes: u64,
    pub(crate) rx_bytes: u64,
    pub(crate) tx_bytes_per_second: u64,
    pub(crate) rx_bytes_per_second: u64,
    pub(crate) lost_packets: u64,
    pub(crate) congestion_events: u64,
    pub(crate) cwnd_bytes: u64,
    pub(crate) mtu: u16,
    pub(crate) compression_codec: RuntimeIrohCompressionCodec,
    pub(crate) compression_wire_bytes: u64,
    pub(crate) compression_decoded_bytes: u64,
    pub(crate) compression_compressed_frames: u64,
    pub(crate) compression_identity_frames: u64,
    pub(crate) render_triggers_coalesced: u64,
    pub(crate) render_updates_suppressed: u64,
    pub(crate) render_snapshot_fallbacks: u64,
    pub(crate) render_ready_depth_max: u64,
    pub(crate) render_write_wait_micros: u64,
    pub(crate) render_write_wait_max_micros: u64,
    pub(crate) render_snapshot_frames: u64,
    pub(crate) render_delta_frames: u64,
    pub(crate) render_changed_rows: u64,
    pub(crate) render_selected_wire_bytes: u64,
    pub(crate) render_selected_decoded_bytes: u64,
    pub(crate) render_snapshot_candidate_bytes: u64,
    path: u8,
}

impl RuntimeIrohConnectionQualitySnapshot {
    /// Returns the selected path class without exposing an address or relay URL.
    pub(crate) const fn path_name(self) -> &'static str {
        match self.path {
            1 => "direct",
            2 => "relay",
            3 => "custom",
            _ => "unknown",
        }
    }

    /// Returns the elapsed time since the transport sample was collected.
    pub(crate) fn sample_age(self) -> std::time::Duration {
        self.sampled_at.elapsed()
    }

    /// Builds a deterministic snapshot for focused command-rendering tests.
    #[cfg(test)]
    pub(crate) fn test_fixture(path: &str) -> Self {
        Self {
            connected_millis: 12_000,
            sampled_at: Instant::now(),
            rtt_micros: 42_000,
            average_rtt_micros: 45_000,
            jitter_micros: 6_000,
            tx_bytes: 524_288,
            rx_bytes: 8_388_608,
            tx_bytes_per_second: 1_126,
            rx_bytes_per_second: 3_277,
            lost_packets: 0,
            congestion_events: 0,
            cwnd_bytes: 65_536,
            mtu: 1_200,
            compression_codec: RuntimeIrohCompressionCodec::Zstd,
            compression_wire_bytes: 512,
            compression_decoded_bytes: 1_024,
            compression_compressed_frames: 2,
            compression_identity_frames: 1,
            render_triggers_coalesced: 4,
            render_updates_suppressed: 1,
            render_snapshot_fallbacks: 1,
            render_ready_depth_max: 5,
            render_write_wait_micros: 250,
            render_write_wait_max_micros: 250,
            render_snapshot_frames: 1,
            render_delta_frames: 1,
            render_changed_rows: 25,
            render_selected_wire_bytes: 176,
            render_selected_decoded_bytes: 608,
            render_snapshot_candidate_bytes: 1_024,
            path: match path {
                "direct" => 1,
                "relay" => 2,
                "custom" => 3,
                _ => 0,
            },
        }
    }
}

/// Classifies one privacy-safe Iroh transport sample for diagnostics and UI.
pub(crate) fn classify_runtime_iroh_connection_quality(
    rtt_micros: u64,
    jitter_micros: u64,
    lost_packets: u64,
    congestion_events: u64,
    sample_age: std::time::Duration,
) -> crate::host::terminal::TerminalIrohStatusQuality {
    use crate::host::terminal::TerminalIrohStatusQuality;

    if sample_age > std::time::Duration::from_secs(5) {
        TerminalIrohStatusQuality::Unknown
    } else if rtt_micros >= 500_000 || lost_packets >= 4 || congestion_events >= 4 {
        TerminalIrohStatusQuality::Poor
    } else if rtt_micros >= 200_000
        || jitter_micros >= 75_000
        || lost_packets > 0
        || congestion_events > 0
    {
        TerminalIrohStatusQuality::Degraded
    } else {
        TerminalIrohStatusQuality::Good
    }
}

#[derive(Debug, Clone)]
struct RuntimeIrohPathSample {
    sampled_at: Instant,
    path_id: String,
    rtt_micros: u64,
    average_rtt_micros: u64,
    jitter_micros: u64,
    tx_bytes: u64,
    rx_bytes: u64,
    lost_packets: u64,
    congestion_events: u64,
    render_triggers_coalesced: u64,
    render_updates_suppressed: u64,
    render_snapshot_fallbacks: u64,
    render_write_wait_micros: u64,
    render_snapshot_frames: u64,
    render_delta_frames: u64,
    render_changed_rows: u64,
    render_selected_wire_bytes: u64,
    render_selected_decoded_bytes: u64,
    render_snapshot_candidate_bytes: u64,
}

/// Copyable status projection that contains no endpoint or peer identifiers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RuntimeIrohDiagnosticsSnapshot {
    pub(crate) listener_active: bool,
    pub(crate) active_connections: usize,
    pub(crate) connections_accepted: u64,
    pub(crate) connections_rejected: u64,
    pub(crate) setup_successes: u64,
    pub(crate) setup_failures: u64,
    pub(crate) setup_latency_total_millis: u64,
    pub(crate) setup_latency_max_millis: u64,
    pub(crate) connections_completed: u64,
    pub(crate) connections_failed: u64,
    pub(crate) direct_connections: u64,
    pub(crate) relay_connections: u64,
    pub(crate) custom_connections: u64,
    pub(crate) unknown_connections: u64,
    pub(crate) shutdown_aborts: u64,
    last_path: u8,
}

impl RuntimeIrohDiagnosticsSnapshot {
    pub(crate) fn average_setup_latency_millis(self) -> u64 {
        let attempts = self.setup_successes.saturating_add(self.setup_failures);
        self.setup_latency_total_millis
            .checked_div(attempts)
            .unwrap_or(0)
    }

    pub(crate) const fn last_path_name(self) -> &'static str {
        match self.last_path {
            1 => "direct",
            2 => "relay",
            3 => "custom",
            _ => "unknown",
        }
    }
}

impl RuntimeIrohDiagnostics {
    pub(crate) fn snapshot(&self) -> RuntimeIrohDiagnosticsSnapshot {
        RuntimeIrohDiagnosticsSnapshot {
            listener_active: self.inner.listener_active.load(Ordering::Relaxed),
            active_connections: self.inner.active_connections.load(Ordering::Relaxed),
            connections_accepted: self.inner.connections_accepted.load(Ordering::Relaxed),
            connections_rejected: self.inner.connections_rejected.load(Ordering::Relaxed),
            setup_successes: self.inner.setup_successes.load(Ordering::Relaxed),
            setup_failures: self.inner.setup_failures.load(Ordering::Relaxed),
            setup_latency_total_millis: self
                .inner
                .setup_latency_total_millis
                .load(Ordering::Relaxed),
            setup_latency_max_millis: self.inner.setup_latency_max_millis.load(Ordering::Relaxed),
            connections_completed: self.inner.connections_completed.load(Ordering::Relaxed),
            connections_failed: self.inner.connections_failed.load(Ordering::Relaxed),
            direct_connections: self.inner.direct_connections.load(Ordering::Relaxed),
            relay_connections: self.inner.relay_connections.load(Ordering::Relaxed),
            custom_connections: self.inner.custom_connections.load(Ordering::Relaxed),
            unknown_connections: self.inner.unknown_connections.load(Ordering::Relaxed),
            shutdown_aborts: self.inner.shutdown_aborts.load(Ordering::Relaxed),
            last_path: self.inner.last_path.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn listener_started(&self) {
        self.inner.listener_active.store(true, Ordering::Relaxed);
    }

    pub(crate) fn listener_stopped(&self) {
        self.inner.listener_active.store(false, Ordering::Relaxed);
    }

    fn record_setup_latency(&self, elapsed: std::time::Duration) {
        let millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.inner
            .setup_latency_total_millis
            .fetch_add(millis, Ordering::Relaxed);
        self.inner
            .setup_latency_max_millis
            .fetch_max(millis, Ordering::Relaxed);
    }

    pub(crate) fn record_rejected(&self, elapsed: std::time::Duration) {
        self.inner
            .connections_rejected
            .fetch_add(1, Ordering::Relaxed);
        self.inner.setup_failures.fetch_add(1, Ordering::Relaxed);
        self.record_setup_latency(elapsed);
    }

    pub(crate) fn connection_started(
        &self,
        connection: &iroh::endpoint::Connection,
        setup_elapsed: std::time::Duration,
    ) -> RuntimeIrohConnectionGuard {
        self.inner
            .connections_accepted
            .fetch_add(1, Ordering::Relaxed);
        self.inner.setup_successes.fetch_add(1, Ordering::Relaxed);
        self.record_setup_latency(setup_elapsed);
        self.inner
            .active_connections
            .fetch_add(1, Ordering::Relaxed);
        self.record_path(connection);
        let client_id = Arc::new(Mutex::new(None));
        RuntimeIrohConnectionGuard {
            diagnostics: self.clone(),
            connected_at: Instant::now(),
            client_id,
        }
    }

    /// Returns the latest selected-path sample for one initialized client.
    pub(crate) fn connection_quality(
        &self,
        client_id: &ClientId,
    ) -> Option<RuntimeIrohConnectionQualitySnapshot> {
        self.inner
            .connection_quality
            .lock()
            .ok()?
            .get(client_id.as_str())
            .copied()
    }

    #[cfg(test)]
    pub(crate) fn set_connection_quality_for_test(
        &self,
        client_id: &ClientId,
        snapshot: RuntimeIrohConnectionQualitySnapshot,
    ) {
        if let Ok(mut quality) = self.inner.connection_quality.lock() {
            quality.insert(client_id.to_string(), snapshot);
        }
    }

    pub(crate) fn record_result<T>(&self, result: &Result<T>) {
        if result.is_ok() {
            self.inner
                .connections_completed
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.inner
                .connections_failed
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_path(&self, connection: &iroh::endpoint::Connection) {
        let paths = connection.paths();
        let path = paths.iter().find(|path| path.is_selected());
        let code = match path {
            Some(path) if path.is_ip() => {
                self.inner
                    .direct_connections
                    .fetch_add(1, Ordering::Relaxed);
                1
            }
            Some(path) if path.is_relay() => {
                self.inner.relay_connections.fetch_add(1, Ordering::Relaxed);
                2
            }
            Some(_) => {
                self.inner
                    .custom_connections
                    .fetch_add(1, Ordering::Relaxed);
                3
            }
            None => {
                self.inner
                    .unknown_connections
                    .fetch_add(1, Ordering::Relaxed);
                0
            }
        };
        self.inner.last_path.store(code, Ordering::Relaxed);
    }
}

struct RuntimeIrohListenerGuard {
    diagnostics: RuntimeIrohDiagnostics,
    endpoint_addr: tokio::sync::watch::Sender<Option<iroh::EndpointAddr>>,
}

impl Drop for RuntimeIrohListenerGuard {
    fn drop(&mut self) {
        self.endpoint_addr.send_replace(None);
        self.diagnostics.listener_stopped();
    }
}

pub(crate) struct RuntimeIrohConnectionGuard {
    diagnostics: RuntimeIrohDiagnostics,
    connected_at: Instant,
    client_id: Arc<Mutex<Option<String>>>,
}

impl RuntimeIrohConnectionGuard {
    /// Builds a sampler tied to this guard's connection lifetime and cleanup key.
    pub(crate) fn sampler(
        &self,
        compression_metrics: IrohCompressionMetrics,
    ) -> RuntimeIrohPathSampler {
        RuntimeIrohPathSampler {
            diagnostics: self.diagnostics.clone(),
            connected_at: self.connected_at,
            client_id: self.client_id.clone(),
            compression_metrics,
            previous_sample: None,
        }
    }
}

pub(crate) struct RuntimeIrohPathSampler {
    diagnostics: RuntimeIrohDiagnostics,
    connected_at: Instant,
    client_id: Arc<Mutex<Option<String>>>,
    compression_metrics: IrohCompressionMetrics,
    previous_sample: Option<RuntimeIrohPathSample>,
}

impl RuntimeIrohPathSampler {
    /// Samples the currently selected path and associates it with the initialized client.
    pub(crate) fn sample(&mut self, connection: &iroh::endpoint::Connection, client_id: &ClientId) {
        self.sample_for_client(connection, client_id.as_str());
    }

    /// Associates this connection with its initialized client before path sampling.
    fn associate_client(&mut self, client_id: &str) {
        let client_id = client_id.to_string();
        if let Ok(mut current_client_id) = self.client_id.lock()
            && current_client_id.as_deref() != Some(client_id.as_str())
        {
            if let Some(previous_client_id) = current_client_id.replace(client_id.clone())
                && let Ok(mut quality) = self.diagnostics.inner.connection_quality.lock()
            {
                quality.remove(&previous_client_id);
            }
            self.previous_sample = None;
        }
    }

    /// Refreshes the selected path for the client already associated with the connection.
    pub(crate) fn sample_current(&mut self, connection: &iroh::endpoint::Connection) {
        let client_id = self
            .client_id
            .lock()
            .ok()
            .and_then(|client_id| client_id.clone());
        if let Some(client_id) = client_id {
            self.sample_for_client(connection, &client_id);
        }
    }

    fn sample_for_client(&mut self, connection: &iroh::endpoint::Connection, client_id: &str) {
        self.associate_client(client_id);
        let now = Instant::now();
        let paths = connection.paths();
        let Some(path) = paths.iter().find(|path| path.is_selected()) else {
            return;
        };
        let stats = path.stats();
        let path_id = format!("{:?}", path.id());
        let rtt_micros = u64::try_from(stats.rtt.as_micros()).unwrap_or(u64::MAX);
        let same_path = self
            .previous_sample
            .as_ref()
            .is_some_and(|previous| previous.path_id == path_id);
        let (average_rtt_micros, jitter_micros) = self
            .previous_sample
            .as_ref()
            .filter(|_| same_path)
            .map(|previous| {
                (
                    previous
                        .average_rtt_micros
                        .saturating_mul(3)
                        .saturating_add(rtt_micros)
                        / 4,
                    previous
                        .jitter_micros
                        .saturating_mul(3)
                        .saturating_add(rtt_micros.abs_diff(previous.rtt_micros))
                        / 4,
                )
            })
            .unwrap_or((rtt_micros, 0));
        let elapsed_millis = self
            .previous_sample
            .as_ref()
            .filter(|_| same_path)
            .map(|previous| {
                u64::try_from(now.duration_since(previous.sampled_at).as_millis())
                    .unwrap_or(u64::MAX)
                    .max(1)
            })
            .unwrap_or(1);
        let delta = |current: u64, previous: fn(&RuntimeIrohPathSample) -> u64| {
            self.previous_sample
                .as_ref()
                .filter(|_| same_path)
                .map(|sample| current.saturating_sub(previous(sample)))
                .unwrap_or(0)
        };
        let tx_delta = delta(stats.udp_tx.bytes, |sample| sample.tx_bytes);
        let rx_delta = delta(stats.udp_rx.bytes, |sample| sample.rx_bytes);
        let compression = self.compression_metrics.snapshot();
        let render_triggers_coalesced = delta(compression.render_triggers_coalesced, |sample| {
            sample.render_triggers_coalesced
        });
        let render_updates_suppressed = delta(compression.render_updates_suppressed, |sample| {
            sample.render_updates_suppressed
        });
        let render_snapshot_fallbacks = delta(compression.render_snapshot_fallbacks, |sample| {
            sample.render_snapshot_fallbacks
        });
        let render_write_wait_micros = delta(compression.render_write_wait_micros, |sample| {
            sample.render_write_wait_micros
        });
        let render_snapshot_frames = delta(compression.render_snapshot_frames, |sample| {
            sample.render_snapshot_frames
        });
        let render_delta_frames = delta(compression.render_delta_frames, |sample| {
            sample.render_delta_frames
        });
        let render_changed_rows = delta(compression.render_changed_rows, |sample| {
            sample.render_changed_rows
        });
        let render_selected_wire_bytes = delta(compression.render_selected_wire_bytes, |sample| {
            sample.render_selected_wire_bytes
        });
        let render_selected_decoded_bytes =
            delta(compression.render_selected_decoded_bytes, |sample| {
                sample.render_selected_decoded_bytes
            });
        let render_snapshot_candidate_bytes =
            delta(compression.render_snapshot_candidate_bytes, |sample| {
                sample.render_snapshot_candidate_bytes
            });
        let snapshot = RuntimeIrohConnectionQualitySnapshot {
            connected_millis: u64::try_from(now.duration_since(self.connected_at).as_millis())
                .unwrap_or(u64::MAX),
            sampled_at: now,
            rtt_micros,
            average_rtt_micros,
            jitter_micros,
            tx_bytes: stats.udp_tx.bytes,
            rx_bytes: stats.udp_rx.bytes,
            tx_bytes_per_second: tx_delta.saturating_mul(1_000) / elapsed_millis,
            rx_bytes_per_second: rx_delta.saturating_mul(1_000) / elapsed_millis,
            lost_packets: delta(stats.lost_packets, |sample| sample.lost_packets),
            congestion_events: delta(stats.congestion_events, |sample| sample.congestion_events),
            cwnd_bytes: stats.cwnd,
            mtu: stats.current_mtu,
            compression_codec: compression.codec,
            compression_wire_bytes: compression.wire_bytes,
            compression_decoded_bytes: compression.decoded_bytes,
            compression_compressed_frames: compression.compressed_frames,
            compression_identity_frames: compression.identity_frames,
            render_triggers_coalesced,
            render_updates_suppressed,
            render_snapshot_fallbacks,
            render_ready_depth_max: compression.render_ready_depth_max,
            render_write_wait_micros,
            render_write_wait_max_micros: compression.render_write_wait_max_micros,
            render_snapshot_frames,
            render_delta_frames,
            render_changed_rows,
            render_selected_wire_bytes,
            render_selected_decoded_bytes,
            render_snapshot_candidate_bytes,
            path: if path.is_ip() {
                1
            } else if path.is_relay() {
                2
            } else {
                3
            },
        };
        if let Ok(mut quality) = self.diagnostics.inner.connection_quality.lock() {
            quality.insert(client_id.to_string(), snapshot);
        }
        self.previous_sample = Some(RuntimeIrohPathSample {
            sampled_at: now,
            path_id,
            rtt_micros,
            average_rtt_micros,
            jitter_micros,
            tx_bytes: stats.udp_tx.bytes,
            rx_bytes: stats.udp_rx.bytes,
            lost_packets: stats.lost_packets,
            congestion_events: stats.congestion_events,
            render_triggers_coalesced: compression.render_triggers_coalesced,
            render_updates_suppressed: compression.render_updates_suppressed,
            render_snapshot_fallbacks: compression.render_snapshot_fallbacks,
            render_write_wait_micros: compression.render_write_wait_micros,
            render_snapshot_frames: compression.render_snapshot_frames,
            render_delta_frames: compression.render_delta_frames,
            render_changed_rows: compression.render_changed_rows,
            render_selected_wire_bytes: compression.render_selected_wire_bytes,
            render_selected_decoded_bytes: compression.render_selected_decoded_bytes,
            render_snapshot_candidate_bytes: compression.render_snapshot_candidate_bytes,
        });
    }
}

impl Drop for RuntimeIrohConnectionGuard {
    fn drop(&mut self) {
        if let Some(client_id) = self
            .client_id
            .lock()
            .ok()
            .and_then(|client_id| client_id.clone())
            && let Ok(mut quality) = self.diagnostics.inner.connection_quality.lock()
        {
            quality.remove(&client_id);
        }
        self.diagnostics
            .inner
            .active_connections
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// Bound endpoint plus the policy governing listener and connection limits.
#[derive(Debug)]
pub(crate) struct RuntimeIrohEndpoint {
    endpoint: Endpoint,
    policy: RuntimeIrohTransportPolicy,
    diagnostics: RuntimeIrohDiagnostics,
    intentional_close: Arc<AtomicBool>,
    endpoint_addr: tokio::sync::watch::Sender<Option<iroh::EndpointAddr>>,
    authority: Option<RuntimeIrohAuthority>,
}

/// Shared direct-session authority used to revalidate a bound remote principal.
#[derive(Debug, Clone)]
struct RuntimeIrohAuthority {
    trust: RemoteTrustStore,
    server_endpoint_id: String,
}

/// Cloneable handle for bounded, intentional endpoint shutdown.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeIrohShutdownHandle {
    endpoint: Endpoint,
    timeout: std::time::Duration,
    diagnostics: RuntimeIrohDiagnostics,
    intentional_close: Arc<AtomicBool>,
}

impl RuntimeIrohShutdownHandle {
    /// Notifies peers of endpoint shutdown and bounds QUIC close draining.
    pub(crate) async fn close(&self) -> bool {
        self.intentional_close.store(true, Ordering::Release);
        if tokio::time::timeout(self.timeout, self.endpoint.close())
            .await
            .is_err()
        {
            self.diagnostics
                .inner
                .shutdown_aborts
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }
}

impl RuntimeIrohEndpoint {
    /// Returns the bound Iroh endpoint.
    #[allow(
        dead_code,
        reason = "the persistent local host consumes this host-Iroh endpoint accessor in the next architecture phase"
    )]
    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Returns the policy applied to this endpoint.
    #[allow(
        dead_code,
        reason = "the persistent local host consumes this host-Iroh policy accessor in the next architecture phase"
    )]
    pub(crate) fn policy(&self) -> &RuntimeIrohTransportPolicy {
        &self.policy
    }

    /// Returns the latest dialable address published by this endpoint.
    #[allow(
        dead_code,
        reason = "the persistent local host consumes this host-Iroh address accessor in the next architecture phase"
    )]
    pub(crate) fn endpoint_addr(&self) -> Option<iroh::EndpointAddr> {
        self.endpoint_addr.borrow().clone()
    }

    /// Returns the shared privacy-safe diagnostics registry for accepted connections.
    pub(crate) fn diagnostics(&self) -> RuntimeIrohDiagnostics {
        self.diagnostics.clone()
    }

    /// Returns a cloneable handle for intentional endpoint shutdown.
    pub(crate) fn shutdown_handle(&self) -> RuntimeIrohShutdownHandle {
        RuntimeIrohShutdownHandle {
            endpoint: self.endpoint.clone(),
            timeout: self.policy.setup_timeout,
            diagnostics: self.diagnostics.clone(),
            intentional_close: self.intentional_close.clone(),
        }
    }

    /// Returns whether endpoint shutdown was initiated through the bounded
    /// runtime shutdown handle.
    pub(crate) fn is_intentionally_closed(&self) -> bool {
        self.intentional_close.load(Ordering::Acquire)
    }

    /// Closes the endpoint and bounds the wait for its I/O tasks to finish.
    pub(crate) async fn close(&self) -> bool {
        self.shutdown_handle().close().await
    }
}

/// Binds a protected endpoint only when remote transport is explicitly enabled.
pub(crate) async fn bind_runtime_iroh_endpoint(
    policy: RuntimeIrohTransportPolicy,
    secret_key: SecretKey,
) -> Result<Option<RuntimeIrohEndpoint>> {
    if !policy.enabled {
        return Ok(None);
    }

    let stream_limit = u32::try_from(policy.max_streams_per_connection)
        .map_err(|_| MezError::config("transport.iroh.max_streams_per_connection is too large"))?;
    let endpoint = bind_policy_iroh_endpoint(
        &policy,
        secret_key,
        stream_limit,
        0,
        (policy.bind_port != 0).then_some(policy.bind_port),
        IrohCompressionPolicy::ordered_alpns(&policy.compression_codecs),
        "Iroh endpoint",
    )
    .await?;
    let initial_addr = wait_for_dialable_iroh_addr(&endpoint, &policy).await?;
    let (endpoint_addr, _) = tokio::sync::watch::channel(Some(initial_addr));
    Ok(Some(RuntimeIrohEndpoint {
        endpoint,
        policy,
        diagnostics: RuntimeIrohDiagnostics::default(),
        intentional_close: Arc::new(AtomicBool::new(false)),
        endpoint_addr,
        authority: None,
    }))
}

/// Waits until the endpoint publishes at least one concrete transport route.
async fn wait_for_dialable_iroh_addr(
    endpoint: &Endpoint,
    policy: &RuntimeIrohTransportPolicy,
) -> Result<iroh::EndpointAddr> {
    let readiness = async {
        if !matches!(&policy.relay, RuntimeIrohRelayPolicy::Disabled) {
            endpoint.online().await;
        }
        let mut watcher = endpoint.watch_addr();
        loop {
            let addr = watcher.get();
            if !addr.is_empty() {
                return Ok::<iroh::EndpointAddr, MezError>(addr);
            }
            watcher.updated().await.map_err(|_| {
                MezError::invalid_state("Iroh endpoint address watcher disconnected")
            })?;
        }
    };
    tokio::time::timeout(policy.setup_timeout, readiness)
        .await
        .map_err(|_| MezError::invalid_state("Iroh endpoint address readiness timed out"))?
}

fn publish_iroh_endpoint_addr(
    publisher: &tokio::sync::watch::Sender<Option<iroh::EndpointAddr>>,
    addr: iroh::EndpointAddr,
) {
    publisher.send_replace((!addr.is_empty()).then_some(addr));
}

/// Binds a client-only endpoint with no remotely initiated streams.
pub(crate) async fn bind_runtime_iroh_client_endpoint(
    policy: &RuntimeIrohTransportPolicy,
    secret_key: SecretKey,
) -> Result<Endpoint> {
    bind_policy_iroh_endpoint(
        policy,
        secret_key,
        0,
        1,
        None,
        Vec::new(),
        "Iroh client endpoint",
    )
    .await
}

async fn bind_policy_iroh_endpoint(
    policy: &RuntimeIrohTransportPolicy,
    secret_key: SecretKey,
    incoming_bidi_streams: u32,
    incoming_uni_streams: u32,
    bind_port: Option<u16>,
    alpns: Vec<Vec<u8>>,
    diagnostic_name: &str,
) -> Result<Endpoint> {
    let idle_timeout = IdleTimeout::try_from(policy.idle_timeout)
        .map_err(|_| MezError::config("transport.iroh.idle_timeout_ms is too large"))?;
    let keep_alive_interval = policy.idle_timeout / 3;
    let transport = QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(incoming_bidi_streams))
        .max_concurrent_uni_streams(VarInt::from_u32(incoming_uni_streams))
        .max_idle_timeout(Some(idle_timeout))
        .keep_alive_interval(keep_alive_interval)
        .build();

    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key)
        .alpns(alpns)
        .transport_config(transport)
        .relay_mode(relay_mode(&policy.relay)?);

    if let Some(bind_port) = bind_port {
        builder = builder
            .bind_addr((Ipv4Addr::UNSPECIFIED, bind_port))
            .map_err(|error| {
                MezError::config(format!("invalid transport.iroh.bind_port: {error}"))
            })?
            .bind_addr_with_opts(
                (Ipv6Addr::UNSPECIFIED, bind_port),
                BindOpts::default().set_is_required(false),
            )
            .map_err(|error| {
                MezError::config(format!("invalid transport.iroh.bind_port: {error}"))
            })?;
    }

    if !policy.port_mapping {
        builder = builder.portmapper_config(PortmapperConfig::Disabled);
    }
    if !policy.direct_connections {
        builder = builder.clear_ip_transports();
    }
    if policy.proxy_from_env {
        builder = builder.proxy_from_env();
    }
    if policy.system_ca_store {
        builder = builder.ca_tls_config(CaTlsConfig::system());
    }
    builder = match &policy.address_lookup {
        RuntimeIrohAddressLookupPolicy::Disabled | RuntimeIrohAddressLookupPolicy::Local => {
            builder.clear_address_lookup()
        }
        RuntimeIrohAddressLookupPolicy::N0Dns => builder
            .address_lookup(PkarrPublisher::n0_dns())
            .address_lookup(DnsAddressLookup::n0_dns()),
        RuntimeIrohAddressLookupPolicy::CustomDns { domain } => builder
            .clear_address_lookup()
            .address_lookup(DnsAddressLookup::builder(domain.clone())),
    };

    tokio::time::timeout(policy.setup_timeout, builder.bind())
        .await
        .map_err(|_| MezError::invalid_state(format!("{diagnostic_name} setup timed out")))?
        .map_err(|error| {
            MezError::invalid_state(format!("failed to bind {diagnostic_name}: {error}"))
        })
}

impl super::RuntimeSessionService {
    /// Returns the effective Iroh policy currently applied to this session.
    pub(crate) fn configured_iroh_transport_policy(
        &self,
    ) -> Result<super::RuntimeIrohTransportPolicy> {
        let structured = super::runtime_effective_config_value(self.integration.config_layers())?;
        super::runtime_iroh_transport_policy_from_config(&structured)
    }

    /// Installs the diagnostics registry owned by a host-routed Iroh transport.
    pub(crate) fn set_host_routed_iroh_diagnostics(&mut self, diagnostics: RuntimeIrohDiagnostics) {
        self.integration
            .set_remote_iroh_diagnostics(Some(diagnostics));
    }

    /// Binds the configured endpoint while retaining the protected identity lock.
    pub(crate) async fn bind_configured_iroh_endpoint(
        &mut self,
    ) -> Result<Option<RuntimeIrohEndpoint>> {
        let policy = self.configured_iroh_transport_policy()?;
        if !policy.enabled {
            self.integration.set_remote_endpoint_addr(None);
            self.integration.set_remote_iroh_diagnostics(None);
            return Ok(None);
        }
        if policy.identity == super::RuntimeIrohIdentityPolicy::Host {
            return Err(MezError::invalid_state(
                "host-scoped Iroh identity requires the persistent host runtime",
            ));
        }
        let session_id = self.session.id.to_string();
        let (secret_key, server_endpoint_id) = {
            let identity = self
                .integration
                .ensure_remote_endpoint_identity(&session_id)?;
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = self
            .integration
            .ensure_remote_trust_store(&session_id)?
            .clone();
        let mut endpoint = bind_runtime_iroh_endpoint(policy, secret_key).await?;
        if let Some(endpoint) = endpoint.as_mut() {
            endpoint.authority = Some(RuntimeIrohAuthority {
                trust,
                server_endpoint_id,
            });
        }
        if let Some(endpoint) = endpoint.as_ref() {
            self.integration
                .set_remote_endpoint_addr_publisher(endpoint.endpoint_addr.clone());
        }
        self.integration.set_remote_iroh_diagnostics(
            endpoint
                .as_ref()
                .map(|endpoint| endpoint.diagnostics.clone()),
        );
        Ok(endpoint)
    }
}

/// Builds one supervised Iroh control-listener service.
pub(crate) fn build_runtime_iroh_control_service(
    endpoint: RuntimeIrohEndpoint,
    handle: AsyncRuntimeSessionHandle,
    control_config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<SnapshotRepository>,
) -> AsyncRuntimeService {
    AsyncRuntimeService::new("iroh-control", async move {
        let served =
            serve_runtime_iroh_control_listener(&endpoint, &handle, control_config, snapshots)
                .await;
        endpoint.close().await;
        served.map(AsyncRuntimeServiceExit::completed)
    })
}

/// Accepts bounded Iroh connections and delegates one bidirectional control stream.
async fn serve_runtime_iroh_control_listener(
    endpoint: &RuntimeIrohEndpoint,
    handle: &AsyncRuntimeSessionHandle,
    control_config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<SnapshotRepository>,
) -> Result<u64> {
    endpoint.diagnostics.listener_started();
    let _listener_guard = RuntimeIrohListenerGuard {
        diagnostics: endpoint.diagnostics.clone(),
        endpoint_addr: endpoint.endpoint_addr.clone(),
    };
    let accepted = Arc::new(AtomicU64::new(0));
    let mut tasks = JoinSet::new();
    let mut lifecycle = handle.lifecycle_state_watcher();
    let mut endpoint_addr = endpoint.endpoint.watch_addr();
    publish_iroh_endpoint_addr(&endpoint.endpoint_addr, endpoint_addr.get());
    let mut endpoint_closed_unexpectedly = false;
    loop {
        let state = *lifecycle.borrow();
        if terminal_daemon_state(state) {
            break;
        }
        tokio::select! {
            updated = endpoint_addr.updated() => {
                match updated {
                    Ok(addr) => publish_iroh_endpoint_addr(&endpoint.endpoint_addr, addr),
                    Err(_) => {
                        endpoint.endpoint_addr.send_replace(None);
                        endpoint_closed_unexpectedly =
                            !endpoint.intentional_close.load(Ordering::Acquire)
                                && !terminal_daemon_state(*lifecycle.borrow());
                        break;
                    }
                }
            }
            incoming = endpoint.endpoint.accept(), if tasks.len() < endpoint.policy.max_connections => {
                let Some(incoming) = incoming else {
                    endpoint_closed_unexpectedly =
                        !endpoint.intentional_close.load(Ordering::Acquire)
                            && !terminal_daemon_state(*lifecycle.borrow());
                    break;
                };
                let connection_handle = handle.clone();
                let connection_snapshots = snapshots.clone();
                let diagnostics = endpoint.diagnostics.clone();
                let policy = endpoint.policy.clone();
                let authority = endpoint.authority.clone();
                let accepted = accepted.clone();
                let mut setup_lifecycle = handle.lifecycle_state_watcher();
                tasks.spawn(async move {
                    let setup_started = Instant::now();
                    let setup_deadline = tokio::time::Instant::now() + policy.setup_timeout;
                    let Ok(mut accepting) = incoming.accept() else {
                        diagnostics.record_rejected(setup_started.elapsed());
                        return Ok(0);
                    };
                    let alpn = tokio::select! {
                        result = tokio::time::timeout_at(setup_deadline, accepting.alpn()) => {
                            match result {
                                Ok(Ok(alpn)) => alpn,
                                _ => {
                                    diagnostics.record_rejected(setup_started.elapsed());
                                    return Ok(0);
                                }
                            }
                        }
                        () = wait_for_terminal_iroh_lifecycle(&mut setup_lifecycle) => {
                            return Ok(0);
                        }
                    };
                    let codec = match RuntimeIrohCompressionCodec::from_alpn(&alpn) {
                        Ok(codec) if policy.compression_codecs.contains(&codec) => codec,
                        _ => {
                            diagnostics.record_rejected(setup_started.elapsed());
                            return Ok(0);
                        }
                    };
                    let max_decoded_bytes = match control_config.max_content_length.checked_add(1024) {
                        Some(limit) => limit,
                        None => {
                            diagnostics.record_rejected(setup_started.elapsed());
                            return Ok(0);
                        }
                    };
                    let compression = match IrohCompressionPolicy::new(
                        codec,
                        policy.compression_min_bytes,
                        policy.compression_zstd_level,
                        max_decoded_bytes,
                    ) {
                        Ok(compression) => compression,
                        Err(_) => {
                            diagnostics.record_rejected(setup_started.elapsed());
                            return Ok(0);
                        }
                    };
                    let connection = tokio::select! {
                        result = tokio::time::timeout_at(setup_deadline, accepting) => {
                            match result {
                                Ok(Ok(connection)) => connection,
                                _ => {
                                    diagnostics.record_rejected(setup_started.elapsed());
                                    return Ok(0);
                                }
                            }
                        }
                        () = wait_for_terminal_iroh_lifecycle(&mut setup_lifecycle) => {
                            return Ok(0);
                        }
                    };
                    connection.set_max_concurrent_bi_streams(VarInt::from_u32(1));
                    connection.set_max_concurrent_uni_streams(VarInt::from_u32(0));
                    let connection_guard =
                        diagnostics.connection_started(&connection, setup_started.elapsed());
                    accepted.fetch_add(1, Ordering::Relaxed);
                    let result = serve_runtime_iroh_control_connection(
                        connection,
                        connection_guard,
                        &connection_handle,
                        control_config,
                        connection_snapshots.as_ref(),
                        authority,
                        compression,
                        policy.setup_timeout,
                        policy.idle_timeout,
                    )
                    .await;
                    diagnostics.record_result(&result);
                    result
                });
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                let Some(joined) = joined else {
                    continue;
                };
                let _connection_result = joined.map_err(|error| {
                    MezError::invalid_state(format!("Iroh control connection task failed: {error}"))
                })?;
            }
        }
    }

    if tokio::time::timeout(
        endpoint.policy.setup_timeout,
        drain_iroh_control_tasks(&mut tasks),
    )
    .await
    .is_err()
    {
        endpoint
            .diagnostics
            .inner
            .shutdown_aborts
            .fetch_add(tasks.len() as u64, Ordering::Relaxed);
        tasks.abort_all();
        while let Some(joined) = tasks.join_next().await {
            if let Err(error) = joined
                && !error.is_cancelled()
            {
                return Err(MezError::invalid_state(format!(
                    "Iroh control connection task failed: {error}"
                )));
            }
        }
    }
    if endpoint_closed_unexpectedly {
        return Err(MezError::invalid_state(
            "Iroh control listener closed unexpectedly while runtime remained active",
        ));
    }
    Ok(accepted.load(Ordering::Relaxed))
}

#[allow(
    clippy::too_many_arguments,
    reason = "connection ownership, diagnostics, runtime state, framing, snapshots, compression, and timeouts are independent adapter inputs"
)]
async fn serve_runtime_iroh_control_connection(
    connection: iroh::endpoint::Connection,
    connection_guard: RuntimeIrohConnectionGuard,
    handle: &AsyncRuntimeSessionHandle,
    control_config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
    authority: Option<RuntimeIrohAuthority>,
    compression: IrohCompressionPolicy,
    setup_timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
) -> Result<u64> {
    let endpoint_id = connection.remote_id().to_string();
    let (send, recv) = tokio::time::timeout(setup_timeout, connection.accept_bi())
        .await
        .map_err(|_| MezError::invalid_state("Iroh control stream setup timed out"))?
        .map_err(|error| {
            MezError::invalid_state(format!("failed to accept Iroh control stream: {error}"))
        })?;
    let compression_metrics = IrohCompressionMetrics::new(compression.codec());
    let mut bridge = IrohCompressionBridge::spawn_with_metrics(
        recv,
        send,
        compression,
        compression_metrics.clone(),
        control_config.max_content_length,
    )?;
    let mut connection_state = ControlConnectionState::new(false, false);
    connection_state.bind_x11_connection_id(format!("iroh-{}", connection.stable_id()))?;
    let (event_start_tx, event_start_rx) =
        tokio::sync::oneshot::channel::<(ClientId, u32, bool, bool)>();
    let mut event_start_tx = Some(event_start_tx);
    let (event_stop_tx, event_stop_rx) = tokio::sync::watch::channel(false);
    let event_connection = connection.clone();
    let event_handle = handle.clone();
    let event_compression_metrics = compression_metrics.clone();
    let mut event_task = tokio::spawn(async move {
        let Ok((client_id, version, client_clipboard_write, push_render)) = event_start_rx.await
        else {
            return Ok(0);
        };
        serve_runtime_iroh_event_stream(
            event_connection,
            event_handle,
            client_id,
            version,
            client_clipboard_write,
            push_render,
            compression,
            event_compression_metrics,
            setup_timeout,
            idle_timeout,
            event_stop_rx,
        )
        .await
    });
    let sampler = Arc::new(Mutex::new(
        connection_guard.sampler(compression_metrics.clone()),
    ));
    let periodic_sampler = sampler.clone();
    let periodic_connection = connection.clone();
    let mut sample_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Ok(mut sampler) = periodic_sampler.lock() {
                sampler.sample_current(&periodic_connection);
            }
        }
    });
    let sample_connection = connection.clone();
    let response_sampler = sampler.clone();
    let request_authority = authority.clone();
    let cancellation_authority = authority;
    let (principal_tx, mut principal_rx) =
        tokio::sync::watch::channel::<Option<RemotePrincipal>>(None);
    let authority_cancelled = async move {
        let Some(authority) = cancellation_authority else {
            std::future::pending::<()>().await;
            return;
        };
        let mut trust_changes = authority.trust.authority_changes();
        loop {
            let principal = principal_rx.borrow().clone();
            if principal.as_ref().is_some_and(|principal| {
                authority
                    .trust
                    .validate_bound_principal(&authority.server_endpoint_id, principal)
                    .is_err()
            }) {
                return;
            }
            tokio::select! {
                changed = trust_changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                changed = principal_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    };
    let result =
        serve_authenticated_async_runtime_control_connection_loop_with_snapshots_hooks_and_cancellation(
            bridge.stream_mut(),
            AuthenticatedPeer::iroh_endpoint(endpoint_id),
            handle,
            &mut connection_state,
            control_config,
            snapshots,
            |_, state| terminal_daemon_state(state),
            move |connection_state| {
                let Some(authority) = request_authority.as_ref() else {
                    return Ok(());
                };
                let Some(principal) = connection_state.remote_principal() else {
                    return Ok(());
                };
                authority
                    .trust
                    .validate_bound_principal(&authority.server_endpoint_id, principal)
            },
            move |connection_state| {
                principal_tx.send_replace(connection_state.remote_principal().cloned());
                if let Some(client_id) = connection_state.caller_client_id()
                    && let Ok(mut sampler) = response_sampler.lock()
                {
                    sampler.sample(&sample_connection, client_id);
                }
                if let Some(start) = connection_state.take_event_stream_start()
                    && let Some(sender) = event_start_tx.take()
                {
                    let _ = sender.send(start);
                }
                if let Some(route) = connection_state.take_x11_route_start() {
                    route.activate(
                        sample_connection.clone(),
                        compression,
                        compression_metrics.clone(),
                    )?;
                }
                Ok(())
            },
            authority_cancelled,
        )
        .await;
    let x11_route_result = connection_state.deactivate_x11_route();
    sample_task.abort();
    let _ = (&mut sample_task).await;
    let _ = event_stop_tx.send(true);
    let shutdown_deadline = tokio::time::Instant::now() + setup_timeout;
    let bridge_finish_result = bridge.finish_outbound_until(shutdown_deadline).await;
    connection.close(
        VarInt::from_u32(u32::from(result.is_err())),
        if result.is_ok() {
            b"control complete"
        } else {
            b"control failed"
        },
    );
    if tokio::time::timeout_at(shutdown_deadline, &mut event_task)
        .await
        .is_err()
    {
        event_task.abort();
        let _ = event_task.await;
    }
    let bridge_result = bridge.settle_until(shutdown_deadline).await;
    let served = result?;
    x11_route_result?;
    bridge_finish_result?;
    bridge_result?;
    Ok(served)
}

#[allow(
    clippy::too_many_arguments,
    reason = "connection ownership, client routing, framing, lifecycle, and bounded setup and idle behavior are independent event adapter inputs"
)]
async fn serve_runtime_iroh_event_stream(
    connection: iroh::endpoint::Connection,
    handle: AsyncRuntimeSessionHandle,
    caller_client_id: ClientId,
    version: u32,
    client_clipboard_write: bool,
    push_render: bool,
    compression: IrohCompressionPolicy,
    compression_metrics: IrohCompressionMetrics,
    setup_timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<u64> {
    if !matches!(version, 1..=3) {
        return Err(MezError::invalid_args(
            "unsupported Iroh event stream version",
        ));
    }
    let clipboard_route = if client_clipboard_write {
        Some(
            handle
                .register_client_clipboard_route(caller_client_id.clone())
                .await?,
        )
    } else {
        None
    };
    let clipboard_route_generation = clipboard_route
        .as_ref()
        .map(crate::host::async_runtime::ClientClipboardRouteLease::generation);
    let result = serve_registered_runtime_iroh_event_stream(
        connection,
        handle.clone(),
        caller_client_id.clone(),
        version,
        push_render,
        compression,
        compression_metrics,
        setup_timeout,
        idle_timeout,
        clipboard_route_generation,
        &mut stop,
    )
    .await;
    if let Some(clipboard_route) = clipboard_route {
        let _ = clipboard_route.close().await;
    }
    result
}

#[allow(
    clippy::too_many_arguments,
    reason = "registered stream ownership, framing, lifecycle, and bounded setup and idle behavior are independent adapter inputs"
)]
async fn serve_registered_runtime_iroh_event_stream(
    connection: iroh::endpoint::Connection,
    handle: AsyncRuntimeSessionHandle,
    caller_client_id: ClientId,
    version: u32,
    push_render: bool,
    compression: IrohCompressionPolicy,
    compression_metrics: IrohCompressionMetrics,
    setup_timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
    clipboard_route_generation: Option<u64>,
    stop: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<u64> {
    let connection_id = format!("iroh-events-{caller_client_id}");
    let mut last_delivered_event_id = 0u64;
    let mut event_delivery = handle.event_delivery_watcher();
    let mut side_effect_delivery = handle.side_effect_delivery_watcher();
    if *stop.borrow() {
        return Ok(0);
    }
    let _ = event_delivery.borrow_and_update();
    let _ = side_effect_delivery.borrow_and_update();
    let mut pending = match handle
        .event_wakeups_for_client(
            caller_client_id.clone(),
            connection_id.clone(),
            last_delivered_event_id,
            IROH_EVENT_BATCH_LIMIT,
        )
        .await
    {
        Ok(wakeups) => wakeups,
        Err(_) => return Ok(0),
    };
    let mut send = tokio::time::timeout(setup_timeout, connection.open_uni())
        .await
        .map_err(|_| MezError::invalid_state("Iroh event stream setup timed out"))?
        .map_err(|_| MezError::invalid_state("failed to open Iroh event stream"))?;
    tokio::time::timeout(
        idle_timeout,
        send.write_all(match version {
            3 => MEZZANINE_IROH_EVENT_STREAM_V3_PREFACE,
            2 => MEZZANINE_IROH_EVENT_STREAM_V2_PREFACE,
            _ => MEZZANINE_IROH_EVENT_STREAM_PREFACE,
        }),
    )
    .await
    .map_err(|_| MezError::invalid_state("Iroh event stream preface timed out"))?
    .map_err(|_| MezError::invalid_state("failed to write Iroh event stream preface"))?;
    tokio::time::timeout(idle_timeout, send.flush())
        .await
        .map_err(|_| MezError::invalid_state("Iroh event stream preface flush timed out"))?
        .map_err(|_| MezError::invalid_state("failed to flush Iroh event stream preface"))?;
    let mut stream_encoder = compression
        .is_streaming()
        .then(|| IrohStreamEncoder::new(compression))
        .transpose()?;
    let mut delivered = 0u64;
    let mut render_revision = 0u64;
    let mut sent_render_view: Option<serde_json::Value> = None;
    if push_render {
        let snapshot = handle
            .render_iroh_client_snapshot(caller_client_id.clone(), true)
            .await?
            .ok_or_else(|| {
                MezError::invalid_state("Iroh v3 client was not renderable after initialization")
            })?;
        render_revision = 1;
        let view = iroh_render_view_value(&snapshot)?;
        let frame = encode_iroh_render_snapshot_frame(&snapshot, render_revision, &view);
        let frame = match stream_encoder.as_mut() {
            Some(encoder) => encoder.encode_frame(&frame, IrohFrameCompressionMode::Eligible)?,
            None => compression.encode_frame(&frame, IrohFrameCompressionMode::Eligible)?,
        };
        compression_metrics.record_frame(
            frame.as_bytes().len(),
            frame.decoded_bytes(),
            frame.compressed(),
        );
        let write_started = std::time::Instant::now();
        tokio::time::timeout(idle_timeout, send.write_all(frame.as_bytes()))
            .await
            .map_err(|_| MezError::invalid_state("Iroh render snapshot write timed out"))?
            .map_err(|_| MezError::invalid_state("Iroh render snapshot write failed"))?;
        tokio::time::timeout(idle_timeout, send.flush())
            .await
            .map_err(|_| MezError::invalid_state("Iroh render snapshot flush timed out"))?
            .map_err(|_| MezError::invalid_state("Iroh render snapshot flush failed"))?;
        compression_metrics.record_render_update(
            false,
            snapshot.view.lines.len(),
            frame.as_bytes().len(),
            frame.decoded_bytes(),
            frame.decoded_bytes(),
        );
        compression_metrics.record_render_write_wait(write_started.elapsed());
        sent_render_view = Some(view);
        last_delivered_event_id = snapshot.event_cutoff;
        pending.clear();
    }
    loop {
        if *stop.borrow() {
            break;
        }
        if let Some(clipboard_route_generation) = clipboard_route_generation
            && let Some(write) = handle
                .take_client_clipboard_write(caller_client_id.clone(), clipboard_route_generation)
                .await?
        {
            for frame in encode_iroh_clipboard_effect_frames(&write) {
                let frame = match stream_encoder.as_mut() {
                    Some(encoder) => {
                        encoder.encode_frame(&frame, IrohFrameCompressionMode::IdentityOnly)?
                    }
                    None => compression.encode_frame(&frame, IrohFrameCompressionMode::Eligible)?,
                };
                compression_metrics.record_frame(
                    frame.as_bytes().len(),
                    frame.decoded_bytes(),
                    frame.compressed(),
                );
                tokio::time::timeout(idle_timeout, send.write_all(frame.as_bytes()))
                    .await
                    .map_err(|_| MezError::invalid_state("Iroh clipboard effect write timed out"))?
                    .map_err(|_| MezError::invalid_state("Iroh clipboard effect write failed"))?;
                if compression.is_streaming() {
                    tokio::time::timeout(idle_timeout, send.flush())
                        .await
                        .map_err(|_| {
                            MezError::invalid_state("Iroh clipboard effect flush timed out")
                        })?
                        .map_err(|_| {
                            MezError::invalid_state("Iroh clipboard effect flush failed")
                        })?;
                }
            }
            if !compression.is_streaming() {
                tokio::time::timeout(idle_timeout, send.flush())
                    .await
                    .map_err(|_| MezError::invalid_state("Iroh clipboard effect flush timed out"))?
                    .map_err(|_| MezError::invalid_state("Iroh clipboard effect flush failed"))?;
            }
        }
        if pending.is_empty() {
            let _ = event_delivery.borrow_and_update();
            pending = match handle
                .event_wakeups_for_client(
                    caller_client_id.clone(),
                    connection_id.clone(),
                    last_delivered_event_id,
                    IROH_EVENT_BATCH_LIMIT,
                )
                .await
            {
                Ok(wakeups) => wakeups,
                Err(_) => break,
            };
        }
        if push_render {
            let _ = side_effect_delivery.borrow_and_update();
            let triggers = collect_ready_iroh_render_triggers(
                &handle,
                &caller_client_id,
                &connection_id,
                last_delivered_event_id,
                std::mem::take(&mut pending),
            )
            .await?;
            delivered =
                delivered.saturating_add(u64::try_from(triggers.events).unwrap_or(u64::MAX));
            if triggers.render_required {
                let snapshot = handle
                    .render_iroh_client_snapshot(
                        caller_client_id.clone(),
                        triggers.invalidate_output,
                    )
                    .await?
                    .ok_or_else(|| {
                        MezError::invalid_state("Iroh v3 client stopped being renderable")
                    })?;
                let next_revision = render_revision.saturating_add(1);
                let update = encode_iroh_render_update_frame(
                    &snapshot,
                    sent_render_view.as_ref(),
                    render_revision,
                    next_revision,
                )?;
                let suppressed = update.is_none();
                let snapshot_fallback = update
                    .as_ref()
                    .is_some_and(|update| update.kind == IrohRenderUpdateKind::Snapshot);
                compression_metrics.record_render_coalescing(
                    triggers.ready_depth(),
                    suppressed,
                    snapshot_fallback,
                );
                if let Some(update) = update {
                    let frame = match stream_encoder.as_mut() {
                        Some(encoder) => encoder
                            .encode_frame(&update.frame, IrohFrameCompressionMode::Eligible)?,
                        None => compression
                            .encode_frame(&update.frame, IrohFrameCompressionMode::Eligible)?,
                    };
                    compression_metrics.record_frame(
                        frame.as_bytes().len(),
                        frame.decoded_bytes(),
                        frame.compressed(),
                    );
                    let write_started = std::time::Instant::now();
                    tokio::time::timeout(idle_timeout, send.write_all(frame.as_bytes()))
                        .await
                        .map_err(|_| MezError::invalid_state("Iroh render update write timed out"))?
                        .map_err(|_| MezError::invalid_state("Iroh render update write failed"))?;
                    tokio::time::timeout(idle_timeout, send.flush())
                        .await
                        .map_err(|_| MezError::invalid_state("Iroh render update flush timed out"))?
                        .map_err(|_| MezError::invalid_state("Iroh render update flush failed"))?;
                    compression_metrics.record_render_update(
                        update.kind == IrohRenderUpdateKind::Delta,
                        update.changed_rows,
                        frame.as_bytes().len(),
                        frame.decoded_bytes(),
                        update.snapshot_candidate_bytes,
                    );
                    compression_metrics.record_render_write_wait(write_started.elapsed());
                    render_revision = next_revision;
                    sent_render_view = Some(update.view);
                }
                last_delivered_event_id = snapshot.event_cutoff;
            } else if let Some(batch_last) = triggers.last_event_id {
                last_delivered_event_id = batch_last;
            }
            if triggers.events > 0 || triggers.render_required {
                continue;
            }
        }
        let mut batch_last = None;
        for wakeup in pending.drain(..) {
            for event in wakeup.events {
                let frame = encode_control_body(&encode_event_notification(&event));
                let frame = match stream_encoder.as_mut() {
                    Some(encoder) => {
                        encoder.encode_frame(&frame, IrohFrameCompressionMode::Eligible)?
                    }
                    None => compression.encode_frame(&frame, IrohFrameCompressionMode::Eligible)?,
                };
                compression_metrics.record_frame(
                    frame.as_bytes().len(),
                    frame.decoded_bytes(),
                    frame.compressed(),
                );
                tokio::time::timeout(idle_timeout, send.write_all(frame.as_bytes()))
                    .await
                    .map_err(|_| MezError::invalid_state("Iroh event stream write timed out"))?
                    .map_err(|_| MezError::invalid_state("Iroh event stream write failed"))?;
                if compression.is_streaming() {
                    tokio::time::timeout(idle_timeout, send.flush())
                        .await
                        .map_err(|_| MezError::invalid_state("Iroh event stream flush timed out"))?
                        .map_err(|_| MezError::invalid_state("Iroh event stream flush failed"))?;
                }
                batch_last = Some(event.id);
                delivered = delivered.saturating_add(1);
            }
        }
        if let Some(batch_last) = batch_last {
            if !compression.is_streaming() {
                tokio::time::timeout(idle_timeout, send.flush())
                    .await
                    .map_err(|_| MezError::invalid_state("Iroh event stream flush timed out"))?
                    .map_err(|_| MezError::invalid_state("Iroh event stream flush failed"))?;
            }
            last_delivered_event_id = batch_last;
            continue;
        }
        tokio::select! {
            changed = event_delivery.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            changed = side_effect_delivery.changed(), if push_render => {
                if changed.is_err() {
                    break;
                }
            }
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            _ = connection.closed() => break,
        }
    }
    let _ = send.finish();
    let _ = tokio::time::timeout(setup_timeout, send.stopped()).await;
    Ok(delivered)
}

/// Serves the event stream for a host-routed, already initialized session.
#[allow(
    clippy::too_many_arguments,
    reason = "routed transport ownership, actor binding, event identity, compression, timeouts, and cancellation are independent adapter inputs"
)]
pub(crate) async fn serve_host_routed_iroh_event_stream(
    connection: iroh::endpoint::Connection,
    handle: AsyncRuntimeSessionHandle,
    caller_client_id: ClientId,
    version: u32,
    client_clipboard_write: bool,
    push_render: bool,
    compression: IrohCompressionPolicy,
    setup_timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<u64> {
    let metrics = IrohCompressionMetrics::new(compression.codec());
    serve_runtime_iroh_event_stream(
        connection,
        handle,
        caller_client_id,
        version,
        client_clipboard_write,
        push_render,
        compression,
        metrics,
        setup_timeout,
        idle_timeout,
        stop,
    )
    .await
}

async fn drain_iroh_control_tasks(tasks: &mut JoinSet<Result<u64>>) -> Result<()> {
    while let Some(joined) = tasks.join_next().await {
        let _connection_result = joined.map_err(|error| {
            MezError::invalid_state(format!("Iroh control connection task failed: {error}"))
        })?;
    }
    Ok(())
}

/// Waits until the runtime enters a terminal lifecycle state or its state
/// publisher disappears, allowing peer-controlled setup to cancel promptly.
async fn wait_for_terminal_iroh_lifecycle(
    lifecycle: &mut tokio::sync::watch::Receiver<super::RuntimeLifecycleState>,
) {
    loop {
        if terminal_daemon_state(*lifecycle.borrow()) || lifecycle.changed().await.is_err() {
            return;
        }
    }
}

fn terminal_daemon_state(state: super::RuntimeLifecycleState) -> bool {
    matches!(
        state,
        super::RuntimeLifecycleState::Stopping
            | super::RuntimeLifecycleState::Killed
            | super::RuntimeLifecycleState::Failed
    )
}

fn relay_mode(policy: &RuntimeIrohRelayPolicy) -> Result<RelayMode> {
    match policy {
        RuntimeIrohRelayPolicy::Disabled => Ok(RelayMode::Disabled),
        RuntimeIrohRelayPolicy::Public => Ok(RelayMode::Default),
        RuntimeIrohRelayPolicy::Custom { urls } => {
            RelayMap::try_from_iter(urls.iter().map(String::as_str))
                .map(RelayMode::Custom)
                .map_err(|error| {
                    MezError::config(format!("invalid transport.iroh.relay_urls: {error}"))
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{RenderInvalidationReason, RuntimeSideEffect};

    // Endpoint construction starts background networking tasks; permit CI
    // scheduler contention without weakening the shorter behavior deadlines.
    const IROH_ENDPOINT_TEST_SETUP_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(10);

    /// Verifies logical render triggers retain the terminal output frame while
    /// exact-client resize effects still invalidate its unsafe geometry base.
    #[test]
    fn iroh_render_trigger_classification_preserves_output_except_for_resize() {
        assert_eq!(
            iroh_event_render_invalidation(EventKind::ClientAttached),
            Some(false)
        );
        assert_eq!(
            iroh_event_render_invalidation(EventKind::WindowChanged),
            Some(false)
        );
        assert_eq!(
            iroh_side_effect_render_invalidation(&[RuntimeSideEffect::RenderClient {
                client_id: mez_core::ids::ClientId::new('c', 1),
                reason: RenderInvalidationReason::Layout,
            }]),
            Some(false)
        );
        assert_eq!(
            iroh_side_effect_render_invalidation(&[RuntimeSideEffect::RenderClient {
                client_id: mez_core::ids::ClientId::new('c', 1),
                reason: RenderInvalidationReason::Resize,
            }]),
            Some(true)
        );
    }

    /// Verifies ready render events spanning multiple actor batches are
    /// classified together so one latest-state render retains the strongest
    /// structural invalidation from the final slice.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_ready_render_triggers_collect_all_current_event_slices() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::protocol::event::EventVisibility;
        use crate::test_support::runtime::SessionFixture;

        let session = SessionFixture::new().build();
        let mut service = crate::runtime::RuntimeSessionService::with_event_log(
            session,
            std::path::PathBuf::from("/tmp/mez-iroh-ready-trigger-test.sock"),
            1,
            128,
            4096,
        )
        .unwrap();
        let primary = service
            .attach_primary(
                "primary",
                true,
                mez_mux::layout::Size::new(80, 24).unwrap(),
                120,
            )
            .unwrap();
        let baseline = service.event_log().unwrap().latest_event_id();
        let session_id = service.session().id.to_string();
        for index in 0..69 {
            service
                .control
                .event_log_mut()
                .unwrap()
                .append(
                    EventKind::PaneChanged,
                    Some(session_id.clone()),
                    EventVisibility::SessionView,
                    format!(r#"{{"index":{index}}}"#),
                )
                .unwrap();
        }
        let expected_last = service
            .control
            .event_log_mut()
            .unwrap()
            .append(
                EventKind::WindowChanged,
                Some(session_id),
                EventVisibility::SessionView,
                r#"{"structural":true}"#,
            )
            .unwrap();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

        let client = async {
            let initial = handle
                .event_wakeups_for_client(
                    primary.clone(),
                    "iroh-events-test".to_string(),
                    baseline,
                    IROH_EVENT_BATCH_LIMIT,
                )
                .await
                .unwrap();
            let triggers = collect_ready_iroh_render_triggers(
                &handle,
                &primary,
                "iroh-events-test",
                baseline,
                initial,
            )
            .await
            .unwrap();

            assert_eq!(triggers.events, 70);
            assert_eq!(triggers.last_event_id, Some(expected_last));
            assert!(triggers.render_required);
            assert!(!triggers.invalidate_output);
            assert!(!triggers.classification_uncertain);
            handle.shutdown().await.unwrap();
        };
        let ((), _) = tokio::join!(client, actor.run());
    }

    /// Verifies a ready range reaching the collector safety bound forces an
    /// invalidating snapshot instead of claiming an ordinary delta is safe.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_ready_render_triggers_force_snapshot_at_safety_bound() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::protocol::event::EventVisibility;
        use crate::test_support::runtime::SessionFixture;

        let session = SessionFixture::new().build();
        let mut service = crate::runtime::RuntimeSessionService::with_event_log(
            session,
            std::path::PathBuf::from("/tmp/mez-iroh-ready-bound-test.sock"),
            1,
            1100,
            4096,
        )
        .unwrap();
        let primary = service
            .attach_primary(
                "primary",
                true,
                mez_mux::layout::Size::new(80, 24).unwrap(),
                120,
            )
            .unwrap();
        let baseline = service.event_log().unwrap().latest_event_id();
        let session_id = service.session().id.to_string();
        for index in 0..1024 {
            service
                .control
                .event_log_mut()
                .unwrap()
                .append(
                    EventKind::Diagnostic,
                    Some(session_id.clone()),
                    EventVisibility::SessionView,
                    format!(r#"{{"index":{index}}}"#),
                )
                .unwrap();
        }
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

        let client = async {
            let initial = handle
                .event_wakeups_for_client(
                    primary.clone(),
                    "iroh-events-bound".to_string(),
                    baseline,
                    IROH_EVENT_BATCH_LIMIT,
                )
                .await
                .unwrap();
            let triggers = collect_ready_iroh_render_triggers(
                &handle,
                &primary,
                "iroh-events-bound",
                baseline,
                initial,
            )
            .await
            .unwrap();

            assert!(triggers.events >= crate::control::MAX_EVENT_REPLAY_RETENTION);
            assert!(triggers.classification_uncertain);
            assert!(triggers.render_required);
            assert!(triggers.invalidate_output);
            handle.shutdown().await.unwrap();
        };
        let ((), _) = tokio::join!(client, actor.run());
    }

    /// Verifies one-row updates select a smaller revisioned delta, identical
    /// logical views are suppressed, and physical invalidation forces a full
    /// snapshot even when the logical view is otherwise unchanged.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_render_updates_select_row_deltas_and_safe_snapshot_fallbacks() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let mut service = RuntimeServiceFixture::new().build();
        let primary = service
            .attach_primary(
                "primary",
                true,
                mez_mux::layout::Size::new(80, 24).unwrap(),
                120,
            )
            .unwrap();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

        let client = async {
            let base = handle
                .render_iroh_client_snapshot(primary, true)
                .await
                .unwrap()
                .expect("attached primary should render");
            let base_view = iroh_render_view_value(&base).unwrap();

            let mut changed = base.clone();
            changed.invalidate_output = false;
            let row = changed
                .view
                .lines
                .first_mut()
                .expect("primary view should contain at least one row");
            *row = "changed visible row".to_string();
            let delta_update = encode_iroh_render_update_frame(&changed, Some(&base_view), 1, 2)
                .unwrap()
                .expect("changed view should produce an update");
            assert_eq!(delta_update.kind, IrohRenderUpdateKind::Delta);
            assert_eq!(delta_update.changed_rows, 1);
            let delta_frame = delta_update.frame;
            let changed_view = delta_update.view;
            let (delta_body, _) =
                crate::control::decode_control_frame(&delta_frame, 1024 * 1024).unwrap();
            assert!(
                delta_body.contains(r#""method":"render/delta""#),
                "{delta_body}"
            );
            assert!(delta_body.contains(r#""base_revision":1"#), "{delta_body}");
            assert!(delta_body.contains(r#""revision":2"#), "{delta_body}");
            let snapshot_frame = encode_iroh_render_snapshot_frame(&changed, 2, &changed_view);
            assert!(
                delta_frame.len().saturating_mul(2) <= snapshot_frame.len(),
                "representative one-row delta must reduce decoded bytes by at least 50%: delta={} snapshot={}",
                delta_frame.len(),
                snapshot_frame.len(),
            );
            assert!(
                encode_iroh_render_update_frame(&changed, Some(&changed_view), 2, 3,)
                    .unwrap()
                    .is_none()
            );

            let mut broad_change = changed.clone();
            for (index, line) in broad_change.view.lines.iter_mut().enumerate() {
                *line = format!("replacement row {index} with enough changed content");
            }
            let broad_update =
                encode_iroh_render_update_frame(&broad_change, Some(&changed_view), 2, 3)
                    .unwrap()
                    .expect("broad change should produce an update");
            assert_eq!(broad_update.kind, IrohRenderUpdateKind::Snapshot);
            let broad_frame = broad_update.frame;
            let (broad_body, _) =
                crate::control::decode_control_frame(&broad_frame, 1024 * 1024).unwrap();
            assert!(
                broad_body.contains(r#""method":"render/snapshot""#),
                "{broad_body}"
            );

            let mut invalidating = changed;
            invalidating.invalidate_output = true;
            let snapshot_update =
                encode_iroh_render_update_frame(&invalidating, Some(&changed_view), 2, 3)
                    .unwrap()
                    .expect("invalidation must force an update");
            assert_eq!(snapshot_update.kind, IrohRenderUpdateKind::Snapshot);
            let snapshot_frame = snapshot_update.frame;
            let (snapshot_body, _) =
                crate::control::decode_control_frame(&snapshot_frame, 1024 * 1024).unwrap();
            assert!(
                snapshot_body.contains(r#""method":"render/snapshot""#),
                "{snapshot_body}"
            );

            let _ = handle.shutdown().await.unwrap();
        };
        let ((), _) = tokio::join!(client, actor.run());
    }

    /// Writes a content-safe release report for render-update selection,
    /// compression cadence, and the v2/v3 serialized-request RTT model.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "release-mode report; run with `just iroh-render-bench`"]
    async fn iroh_render_update_release_benchmark() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::test_support::runtime::RuntimeServiceFixture;

        const ITERATIONS: usize = 2_000;

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
                "p50_ns": percentile(50),
                "p95_ns": percentile(95),
                "p99_ns": percentile(99),
                "max_ns": samples.last().copied().unwrap_or(0),
            })
        }

        let mut service = RuntimeServiceFixture::new().build();
        let primary = service
            .attach_primary(
                "render-benchmark",
                true,
                mez_mux::layout::Size::new(120, 40).unwrap(),
                120,
            )
            .unwrap();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let client = async {
            let snapshot = handle
                .render_iroh_client_snapshot(primary, true)
                .await
                .unwrap()
                .expect("attached benchmark primary should render");
            handle.shutdown().await.unwrap();
            snapshot
        };
        let (base, _) = tokio::join!(client, actor.run());
        let base_view = iroh_render_view_value(&base).unwrap();

        let mut one_row = base.clone();
        one_row.invalidate_output = false;
        *one_row
            .view
            .lines
            .first_mut()
            .expect("benchmark view should contain a row") = "changed benchmark row".to_string();
        let mut broad_rows = one_row.clone();
        for (index, line) in broad_rows.view.lines.iter_mut().enumerate() {
            *line = format!("benchmark replacement row {index} with changed content");
        }
        let mut invalidating = one_row.clone();
        invalidating.invalidate_output = true;
        let workloads = [
            ("one_row", one_row),
            ("broad_rows", broad_rows),
            ("invalidating", invalidating),
        ];

        let mut results = Vec::new();
        for codec in [
            RuntimeIrohCompressionCodec::None,
            RuntimeIrohCompressionCodec::Zstd,
            RuntimeIrohCompressionCodec::Lz4,
        ] {
            for (workload, snapshot) in &workloads {
                let selected = encode_iroh_render_update_frame(snapshot, Some(&base_view), 1, 2)
                    .unwrap()
                    .expect("benchmark workload should select an update");
                let selected_kind = match selected.kind {
                    IrohRenderUpdateKind::Snapshot => "snapshot",
                    IrohRenderUpdateKind::Delta => "delta",
                };
                let policy = IrohCompressionPolicy::new(
                    codec,
                    512,
                    3,
                    selected.snapshot_candidate_bytes.saturating_add(1024),
                )
                .unwrap();
                let snapshot_frame = encode_iroh_render_snapshot_frame(snapshot, 2, &selected.view);
                let snapshot_wire_bytes = policy
                    .encode_frame(&snapshot_frame, IrohFrameCompressionMode::Eligible)
                    .unwrap()
                    .as_bytes()
                    .len();
                let mut samples = Vec::with_capacity(ITERATIONS);
                let mut selected_wire_bytes = 0u64;
                for _ in 0..ITERATIONS {
                    let started = std::time::Instant::now();
                    let update = encode_iroh_render_update_frame(snapshot, Some(&base_view), 1, 2)
                        .unwrap()
                        .expect("benchmark workload should keep selecting an update");
                    let encoded = policy
                        .encode_frame(&update.frame, IrohFrameCompressionMode::Eligible)
                        .unwrap();
                    selected_wire_bytes = selected_wire_bytes.saturating_add(
                        u64::try_from(encoded.as_bytes().len()).unwrap_or(u64::MAX),
                    );
                    let decoded = policy.decode_frame(encoded.as_bytes()).unwrap();
                    assert_eq!(decoded, update.frame);
                    samples.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
                    std::hint::black_box(decoded);
                }
                results.push(serde_json::json!({
                    "codec": codec.as_str(),
                    "workload": workload,
                    "selected_kind": selected_kind,
                    "changed_rows": selected.changed_rows,
                    "selected_decoded_bytes": selected.frame.len(),
                    "selected_wire_bytes_per_update": selected_wire_bytes as f64 / ITERATIONS as f64,
                    "snapshot_candidate_decoded_bytes": selected.snapshot_candidate_bytes,
                    "snapshot_candidate_wire_bytes": snapshot_wire_bytes,
                    "decoded_fraction_of_snapshot": selected.frame.len() as f64
                        / selected.snapshot_candidate_bytes.max(1) as f64,
                    "wire_fraction_of_snapshot": selected_wire_bytes as f64
                        / ITERATIONS as f64 / snapshot_wire_bytes.max(1) as f64,
                    "selection_encode_decode_latency": latency_summary(samples),
                    "envelopes_per_update": 1,
                    "flushes_per_update": 1,
                }));
            }
        }

        let rtt_models = [0u64, 25, 75, 150]
            .into_iter()
            .map(|rtt_ms| {
                serde_json::json!({
                    "rtt_ms": rtt_ms,
                    "v2": {
                        "terminal_step_requests": 1,
                        "terminal_view_requests": 1,
                        "serialized_control_round_trips": 2,
                        "modeled_no_processing_input_to_visible_floor_ms": rtt_ms.saturating_mul(2),
                    },
                    "v3": {
                        "terminal_step_requests": 1,
                        "terminal_view_requests": 0,
                        "serialized_control_round_trips": 1,
                        "modeled_no_processing_input_to_visible_floor_ms": rtt_ms,
                    },
                    "avoidable_v2_view_fetch_ms": rtt_ms,
                })
            })
            .collect::<Vec<_>>();
        let report = serde_json::to_string_pretty(&serde_json::json!({
            "format_version": 1,
            "report_only": true,
            "iterations_per_fixture": ITERATIONS,
            "compression_min_bytes": 512,
            "compression_zstd_level": 3,
            "rtt_model": {
                "kind": "serialized_request_count_model",
                "excludes": ["server_render", "codec", "scheduling", "relay", "client_apply"],
                "matrix": rtt_models,
            },
            "results": results,
        }))
        .unwrap();
        if let Ok(path) = std::env::var("MEZ_IROH_RENDER_BENCH_REPORT") {
            std::fs::write(path, format!("{report}\n")).unwrap();
        }
        println!("{report}");
    }

    #[derive(Debug, Clone)]
    struct StallFirstHandshake {
        attempts: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl iroh::endpoint::EndpointHooks for StallFirstHandshake {
        async fn after_handshake(
            &self,
            _connection: &iroh::endpoint::Connection,
        ) -> iroh::endpoint::AfterHandshakeOutcome {
            if self.attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                self.entered.notify_one();
                self.release.notified().await;
            }
            iroh::endpoint::AfterHandshakeOutcome::Accept
        }
    }

    /// Verifies clipboard effects use bounded contiguous chunks and keep raw
    /// payload bytes out of frame metadata and debug output.
    #[test]
    fn iroh_clipboard_effect_frames_are_bounded_ordered_and_private() {
        let payload = format!("private-prefix-{}", "x".repeat(IROH_CLIPBOARD_CHUNK_BYTES));
        let write = super::super::ClientClipboardWrite::new(7, payload.clone()).unwrap();
        let frames = encode_iroh_clipboard_effect_frames(&write);

        assert_eq!(frames.len(), 4);
        let bodies = frames
            .iter()
            .map(|frame| {
                crate::control::decode_control_frame(frame, 1_048_576)
                    .unwrap()
                    .0
            })
            .collect::<Vec<_>>();
        assert!(bodies[0].contains(r#""method":"client/clipboard.begin""#));
        assert!(bodies[0].contains(r#""sequence":7"#));
        assert!(bodies[0].contains(r#""chunks":2"#));
        assert!(bodies[1].contains(r#""index":0"#));
        assert!(bodies[2].contains(r#""index":1"#));
        assert!(bodies[3].contains(r#""method":"client/clipboard.commit""#));
        assert!(bodies.iter().all(|body| !body.contains("private-prefix")));
        assert!(!format!("{write:?}").contains("private-prefix"));
    }

    /// Verifies an empty clipboard write still emits the declared single
    /// contiguous chunk so a receiver can validate and commit it exactly.
    #[test]
    fn iroh_empty_clipboard_effect_emits_one_empty_chunk() {
        let write = super::super::ClientClipboardWrite::new(3, String::new()).unwrap();
        let frames = encode_iroh_clipboard_effect_frames(&write);

        assert_eq!(frames.len(), 3);
        let chunk = crate::control::decode_control_frame(&frames[1], 1_048_576)
            .unwrap()
            .0;
        assert!(chunk.contains(r#""index":0"#), "{chunk}");
        assert!(chunk.contains(r#""data_base64":"""#), "{chunk}");
    }

    /// Verifies the shared Iroh quality classifier preserves every threshold
    /// and treats stale samples as unknown before considering measurements.
    #[test]
    fn iroh_connection_quality_classifier_covers_thresholds_and_staleness() {
        use crate::host::terminal::TerminalIrohStatusQuality;

        let classify = |rtt, jitter, loss, congestion, age_seconds| {
            classify_runtime_iroh_connection_quality(
                rtt,
                jitter,
                loss,
                congestion,
                std::time::Duration::from_secs(age_seconds),
            )
        };
        assert_eq!(
            classify(42_000, 6_000, 0, 0, 0),
            TerminalIrohStatusQuality::Good
        );
        assert_eq!(
            classify(200_000, 0, 0, 0, 0),
            TerminalIrohStatusQuality::Degraded
        );
        assert_eq!(
            classify(0, 75_000, 0, 0, 0),
            TerminalIrohStatusQuality::Degraded
        );
        assert_eq!(classify(0, 0, 1, 0, 0), TerminalIrohStatusQuality::Degraded);
        assert_eq!(
            classify(500_000, 0, 0, 0, 0),
            TerminalIrohStatusQuality::Poor
        );
        assert_eq!(classify(0, 0, 4, 0, 0), TerminalIrohStatusQuality::Poor);
        assert_eq!(
            classify(900_000, 0, 9, 9, 6),
            TerminalIrohStatusQuality::Unknown
        );
    }

    /// Verifies a client remains associated with its Iroh connection while
    /// path discovery is incomplete, allowing the periodic sampler to publish
    /// the first selected-path sample later.
    #[test]
    fn iroh_path_sampler_associates_client_before_path_selection() {
        let diagnostics = RuntimeIrohDiagnostics::default();
        let client_id = ClientId::opaque("remote-primary").unwrap();
        let guard = RuntimeIrohConnectionGuard {
            diagnostics,
            connected_at: Instant::now(),
            client_id: Arc::new(Mutex::new(None)),
        };
        let mut sampler = guard.sampler(IrohCompressionMetrics::new(
            RuntimeIrohCompressionCodec::Zstd,
        ));

        sampler.associate_client(client_id.as_str());

        assert_eq!(
            sampler.client_id.lock().unwrap().as_deref(),
            Some(client_id.as_str())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_endpoint_construction_is_disabled_by_default() {
        let endpoint = bind_runtime_iroh_endpoint(
            RuntimeIrohTransportPolicy::default(),
            SecretKey::generate(),
        )
        .await
        .unwrap();
        assert!(endpoint.is_none());
    }

    /// Verifies enabled transport startup fails within its setup bound when
    /// neither direct nor relay networking can publish a concrete route.
    ///
    /// An endpoint-id-only address must never become invitation state because
    /// clients without address lookup cannot dial it.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_endpoint_construction_rejects_route_empty_transport() {
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            relay: RuntimeIrohRelayPolicy::Disabled,
            direct_connections: false,
            setup_timeout: std::time::Duration::from_millis(100),
            ..RuntimeIrohTransportPolicy::default()
        };

        let error = bind_runtime_iroh_endpoint(policy, SecretKey::generate())
            .await
            .unwrap_err();
        assert!(
            error.message().contains("failed to bind Iroh endpoint")
                || error.message().contains("address readiness timed out"),
            "{error:?}"
        );
    }

    /// Verifies live address publication exposes concrete routes and removes
    /// stale state when a watcher update becomes route-empty.
    ///
    /// Invitation creation reads this shared channel, so route loss must be
    /// visible immediately rather than retaining a previously dialable value.
    #[test]
    fn iroh_address_publication_tracks_route_presence() {
        let endpoint_id = SecretKey::generate().public();
        let (publisher, receiver) = tokio::sync::watch::channel(None);

        publish_iroh_endpoint_addr(&publisher, iroh::EndpointAddr::new(endpoint_id));
        assert!(receiver.borrow().is_none());

        let dialable =
            iroh::EndpointAddr::new(endpoint_id).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        publish_iroh_endpoint_addr(&publisher, dialable.clone());
        assert_eq!(receiver.borrow().as_ref(), Some(&dialable));

        publish_iroh_endpoint_addr(&publisher, iroh::EndpointAddr::new(endpoint_id));
        assert!(receiver.borrow().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_endpoint_construction_uses_protected_identity_and_limits() {
        let secret_key = SecretKey::generate();
        let endpoint_id = secret_key.public();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            setup_timeout: std::time::Duration::from_secs(10),
            ..RuntimeIrohTransportPolicy::default()
        };
        let endpoint = bind_runtime_iroh_endpoint(policy.clone(), secret_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(endpoint.endpoint().id(), endpoint_id);
        assert_eq!(endpoint.policy(), &policy);
        endpoint.close().await;
    }

    /// Verifies an explicitly configured direct port is reused across a clean
    /// endpoint restart while the protected endpoint identity remains stable.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_endpoint_rebinds_stable_configured_port() {
        let reservation = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let bind_port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let secret_key = SecretKey::generate();
        let endpoint_id = secret_key.public();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            bind_port,
            setup_timeout: std::time::Duration::from_secs(10),
            ..RuntimeIrohTransportPolicy::default()
        };

        let first = bind_runtime_iroh_endpoint(policy.clone(), secret_key.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.endpoint().id(), endpoint_id);
        assert!(
            first
                .endpoint()
                .bound_sockets()
                .iter()
                .all(|addr| addr.port() == bind_port)
        );
        first.close().await;
        drop(first);

        let second = bind_runtime_iroh_endpoint(policy, secret_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.endpoint().id(), endpoint_id);
        assert!(
            second
                .endpoint()
                .bound_sockets()
                .iter()
                .all(|addr| addr.port() == bind_port)
        );
        second.close().await;
    }

    /// Verifies endpoint loss while the daemon remains running is surfaced as
    /// a listener failure rather than a successful service completion.
    ///
    /// An enabled transport must not silently degrade to Unix-only control if
    /// its endpoint closes independently of the runtime lifecycle.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_listener_reports_unexpected_endpoint_closure() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let service = RuntimeServiceFixture::new().build();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            setup_timeout: IROH_ENDPOINT_TEST_SETUP_TIMEOUT,
            ..RuntimeIrohTransportPolicy::default()
        };
        let server = bind_runtime_iroh_endpoint(policy, SecretKey::generate())
            .await
            .unwrap()
            .unwrap();
        let endpoint = server.endpoint().clone();
        let diagnostics = server.diagnostics.clone();
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await
        });
        let actor_task = tokio::spawn(actor.run());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !diagnostics.snapshot().listener_active {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        endpoint.close().await;

        let error = tokio::time::timeout(std::time::Duration::from_secs(2), listener)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            error
                .message()
                .contains("Iroh control listener closed unexpectedly"),
            "{error:?}"
        );
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            crate::runtime::RuntimeLifecycleState::Running
        );
        assert!(!diagnostics.snapshot().listener_active);

        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        actor_task.abort();
        let _ = actor_task.await;
    }

    /// Verifies intentional shutdown notifies an active peer and lets the
    /// listener finish without reporting endpoint loss as a service failure.
    ///
    /// Foreground cancellation awaits this bounded close path before the
    /// supervisor aborts any remaining services.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_shutdown_handle_notifies_active_peer_before_listener_exit() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let service = RuntimeServiceFixture::new().build();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            setup_timeout: IROH_ENDPOINT_TEST_SETUP_TIMEOUT,
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server = bind_runtime_iroh_endpoint(policy, SecretKey::generate())
            .await
            .unwrap()
            .unwrap();
        let server_addr = server.endpoint().addr();
        let shutdown = server.shutdown_handle();
        let diagnostics = server.diagnostics.clone();
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await
        });
        let actor_task = tokio::spawn(actor.run());
        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let connection = client
            .connect(server_addr, MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while diagnostics.snapshot().active_connections != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let (closed_cleanly, _peer_close) =
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::join!(shutdown.close(), connection.closed())
            })
            .await
            .unwrap();
        assert!(closed_cleanly);
        assert_eq!(listener.await.unwrap().unwrap(), 1);

        let snapshot = diagnostics.snapshot();
        assert!(!snapshot.listener_active);
        assert_eq!(snapshot.active_connections, 0);
        assert_eq!(snapshot.shutdown_aborts, 0);
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            crate::runtime::RuntimeLifecycleState::Running
        );

        client.close().await;
        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        actor_task.abort();
        let _ = actor_task.await;
    }

    /// One peer stalled after ALPN negotiation must not block a second valid
    /// handshake while listener admission capacity remains available, and
    /// runtime cancellation must not wait for that stalled setup deadline.
    #[tokio::test(flavor = "current_thread")]
    async fn iroh_listener_parallelizes_pre_session_handshakes() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let service = RuntimeServiceFixture::new().build();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 2,
            setup_timeout: std::time::Duration::from_secs(5),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .alpns(IrohCompressionPolicy::ordered_alpns(
                &policy.compression_codecs,
            ))
            .hooks(StallFirstHandshake {
                attempts: Arc::new(AtomicUsize::new(0)),
                entered: entered.clone(),
                release: release.clone(),
            })
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let initial_addr = wait_for_dialable_iroh_addr(&endpoint, &policy)
            .await
            .unwrap();
        let (endpoint_addr, _) = tokio::sync::watch::channel(Some(initial_addr));
        let server_addr = endpoint.addr();
        let diagnostics = RuntimeIrohDiagnostics::default();
        let server = RuntimeIrohEndpoint {
            endpoint,
            policy,
            diagnostics: diagnostics.clone(),
            intentional_close: Arc::new(AtomicBool::new(false)),
            endpoint_addr,
            authority: None,
        };
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            let result = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await;
            server.close().await;
            result
        });
        let actor_task = tokio::spawn(actor.run());
        let first_client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let first_endpoint = first_client.clone();
        let first_addr = server_addr.clone();
        let first_connect = tokio::spawn(async move {
            first_endpoint
                .connect(first_addr, MEZZANINE_IROH_ALPN)
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .expect("first server handshake should reach the stall hook");

        let second_client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let second_connection = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            second_client.connect(server_addr, MEZZANINE_IROH_ALPN),
        )
        .await
        .expect("valid peer should not wait for the stalled handshake")
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while diagnostics.snapshot().setup_successes != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        second_connection.close(VarInt::from_u32(0), b"test complete");
        second_client.close().await;
        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        let served = tokio::time::timeout(std::time::Duration::from_secs(1), listener)
            .await
            .expect("listener cancellation should not wait for stalled setup")
            .unwrap()
            .unwrap();
        assert_eq!(served, 1);
        first_connect.abort();
        let _ = first_connect.await;
        first_client.close().await;
        release.notify_one();
        actor_task.abort();
        let _ = actor_task.await;
    }

    async fn read_test_control_body<R>(stream: &mut R) -> String
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let mut response = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = tokio::io::AsyncReadExt::read(stream, &mut buffer)
                .await
                .unwrap();
            assert!(
                read > 0,
                "framed response must arrive before stream closure"
            );
            response.extend_from_slice(&buffer[..read]);
            if let Ok((body, _)) = crate::control::decode_control_frame(&response, 1024 * 1024) {
                return body;
            }
        }
    }

    /// Verifies a paired control and event connection remains usable after an
    /// application-silent period longer than a production-valid QUIC idle
    /// timeout. This protects persistent attaches from idle disconnection while
    /// avoiding sub-policy deadlines that turn ordinary CI scheduler stalls into
    /// transport failures.
    #[tokio::test(flavor = "current_thread")]
    async fn paired_iroh_control_and_events_survive_idle_period_over_direct_listener() {
        use secrecy::ExposeSecret;

        use crate::control::encode_control_body;
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-direct-listener-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        service.set_config_root(root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let store = RemoteTrustStore::under_config_root(&root, &session_id).unwrap();
        let invitation = store
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();

        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            max_streams_per_connection: 1,
            setup_timeout: IROH_ENDPOINT_TEST_SETUP_TIMEOUT,
            idle_timeout: std::time::Duration::from_secs(3),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server_endpoint = bind_runtime_iroh_endpoint(policy, server_secret)
            .await
            .unwrap()
            .unwrap();
        let server_addr = server_endpoint.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let shutdown_handle = handle.clone();

        let client_endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_uni_streams(VarInt::from_u32(1))
                    .build(),
            )
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "control/initialize",
            "params": {
                "requested_role": "primary",
                "requested_version": 2,
                "client_name": "remote-primary",
                "event_stream_version": 3,
                "client": {
                    "name": "remote-primary",
                    "interactive": true,
                    "terminal": {
                        "columns": 80,
                        "rows": 24,
                        "term": "xterm-256color"
                    }
                },
                "authentication": {
                    "mechanism": "extension:iroh_invitation",
                    "token": invitation.token.expose_secret()
                }
            }
        })
        .to_string();
        let list = r#"{"jsonrpc":"2.0","id":"list","method":"session/list","params":{}}"#;

        let listener_handle = handle.clone();
        drop(handle);
        let listener = async move {
            let served = serve_runtime_iroh_control_listener(
                &server_endpoint,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(1024 * 1024, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(served, 1);
            server_endpoint.close().await;
        };
        let client = async {
            let connection = client_endpoint
                .connect(server_addr, MEZZANINE_IROH_ALPN)
                .await
                .unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    connection.accept_uni(),
                )
                .await
                .is_err(),
                "the server must not open an event stream before initialization",
            );
            send.write_all(&encode_control_body(&initialize))
                .await
                .unwrap();
            send.flush().await.unwrap();
            let initialize_body = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                read_test_control_body(&mut recv),
            )
            .await
            .expect("initialization response should arrive within the test I/O deadline");
            assert!(initialize_body.contains(r#""granted_role":"primary""#));
            assert!(initialize_body.contains(r#""device_credential""#));
            let mut events =
                tokio::time::timeout(std::time::Duration::from_secs(5), connection.accept_uni())
                    .await
                    .unwrap()
                    .unwrap();
            let mut preface = vec![0u8; MEZZANINE_IROH_EVENT_STREAM_V3_PREFACE.len()];
            events.read_exact(&mut preface).await.unwrap();
            assert_eq!(preface, MEZZANINE_IROH_EVENT_STREAM_V3_PREFACE);
            let event_body = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                read_test_control_body(&mut events),
            )
            .await
            .expect("initial event snapshot should arrive within the test I/O deadline");
            assert!(
                event_body.contains(r#""method":"render/snapshot""#),
                "{event_body}"
            );
            assert!(event_body.contains(r#""revision":1"#), "{event_body}");
            assert!(event_body.contains(r#""role":"primary""#), "{event_body}");
            assert!(!event_body.contains("device_credential"), "{event_body}");
            assert!(
                !event_body.contains(invitation.token.expose_secret()),
                "{event_body}"
            );

            tokio::time::sleep(std::time::Duration::from_secs(4)).await;

            send.write_all(&encode_control_body(list)).await.unwrap();
            send.flush().await.unwrap();
            let list_body = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                read_test_control_body(&mut recv),
            )
            .await
            .expect("post-idle control response should arrive within the test I/O deadline");
            assert!(list_body.contains(r#""sessions":["#), "{list_body}");

            assert_eq!(
                shutdown_handle.shutdown().await.unwrap(),
                crate::runtime::RuntimeLifecycleState::Running
            );
            send.finish().unwrap();
            connection.close(VarInt::from_u32(0), b"test complete");
            client_endpoint.close().await;
        };

        let actor_task = tokio::spawn(actor.run());
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let ((), ()) = tokio::join!(listener, client);
        })
        .await
        .unwrap();
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    /// Verifies an observer v3 stream pushes exact-observer snapshots over a
    /// direct listener and applies observer-local resize without a view fetch.
    #[tokio::test(flavor = "current_thread")]
    async fn paired_iroh_observer_v3_pushes_resized_snapshot_over_direct_listener() {
        use secrecy::ExposeSecret;

        use crate::control::encode_control_body;
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-observer-v3-listener-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service
            .attach_primary(
                "local-primary",
                true,
                mez_mux::layout::Size::new(80, 24).unwrap(),
                1,
            )
            .unwrap();
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        service.set_config_root(root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let invitation = RemoteTrustStore::under_config_root(&root, &session_id)
            .unwrap()
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Observer,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            max_streams_per_connection: 1,
            setup_timeout: std::time::Duration::from_secs(10),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server_endpoint = bind_runtime_iroh_endpoint(policy, server_secret)
            .await
            .unwrap()
            .unwrap();
        let server_addr = server_endpoint.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let shutdown_handle = handle.clone();

        let client_endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_uni_streams(VarInt::from_u32(1))
                    .build(),
            )
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "control/initialize",
            "params": {
                "requested_role": "observer",
                "requested_version": 2,
                "client_name": "remote-observer",
                "event_stream_version": 3,
                "client": {
                    "name": "remote-observer",
                    "interactive": true,
                    "metadata": {"pushed_render_updates": true},
                    "terminal": {"columns": 70, "rows": 20, "term": "xterm-256color"}
                },
                "authentication": {
                    "mechanism": "extension:iroh_invitation",
                    "token": invitation.token.expose_secret()
                }
            }
        })
        .to_string();

        let listener_handle = handle.clone();
        drop(handle);
        let listener = async move {
            let served = serve_runtime_iroh_control_listener(
                &server_endpoint,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(1024 * 1024, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(served, 1);
            server_endpoint.close().await;
        };
        let client = async {
            let connection = client_endpoint
                .connect(server_addr, MEZZANINE_IROH_ALPN)
                .await
                .unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            send.write_all(&encode_control_body(&initialize))
                .await
                .unwrap();
            send.flush().await.unwrap();
            let initialize_body = read_test_control_body(&mut recv).await;
            assert!(initialize_body.contains(r#""granted_role":"observer""#));
            assert!(initialize_body.contains(r#""client_clipboard_write":false"#));

            let mut events =
                tokio::time::timeout(std::time::Duration::from_secs(3), connection.accept_uni())
                    .await
                    .unwrap()
                    .unwrap();
            let mut preface = vec![0u8; MEZZANINE_IROH_EVENT_STREAM_V3_PREFACE.len()];
            events.read_exact(&mut preface).await.unwrap();
            assert_eq!(preface, MEZZANINE_IROH_EVENT_STREAM_V3_PREFACE);
            let initial = read_test_control_body(&mut events).await;
            assert!(
                initial.contains(r#""method":"render/snapshot""#),
                "{initial}"
            );
            assert!(initial.contains(r#""role":"observer""#), "{initial}");
            assert!(
                initial.contains(r#""client_size":{"columns":70,"rows":20}"#),
                "{initial}"
            );

            let resize = r#"{"jsonrpc":"2.0","id":"resize","method":"terminal/resize","params":{"idempotency_key":"observer-resize","client_size":{"columns":100,"rows":30}}}"#;
            send.write_all(&encode_control_body(resize)).await.unwrap();
            send.flush().await.unwrap();
            let resize_body = read_test_control_body(&mut recv).await;
            assert!(resize_body.contains(r#""resized":true"#), "{resize_body}");
            let resized = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                read_test_control_body(&mut events),
            )
            .await
            .expect("observer resize should push an authoritative snapshot");
            assert!(
                resized.contains(r#""method":"render/snapshot""#),
                "{resized}"
            );
            assert!(resized.contains(r#""revision":2"#), "{resized}");
            assert!(resized.contains(r#""role":"observer""#), "{resized}");
            assert!(
                resized.contains(r#""client_size":{"columns":100,"rows":30}"#),
                "{resized}"
            );

            assert_eq!(
                shutdown_handle.shutdown().await.unwrap(),
                crate::runtime::RuntimeLifecycleState::Running
            );
            send.finish().unwrap();
            connection.close(VarInt::from_u32(0), b"test complete");
            client_endpoint.close().await;
        };

        let actor_task = tokio::spawn(actor.run());
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let ((), ()) = tokio::join!(listener, client);
        })
        .await
        .unwrap();
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    /// Verifies one trusted Iroh endpoint may own two independent primary
    /// clients and event streams, and closing one exact client does not close
    /// or deauthorize the other stream.
    #[tokio::test(flavor = "current_thread")]
    async fn same_iroh_endpoint_keeps_independent_primary_event_streams() {
        use secrecy::ExposeSecret;

        use crate::control::encode_control_body;
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-two-primary-events-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let store = RemoteTrustStore::under_config_root(&root, &session_id).unwrap();
        let invitation = store
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 2,
            max_streams_per_connection: 1,
            setup_timeout: std::time::Duration::from_secs(10),
            idle_timeout: std::time::Duration::from_secs(30),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server_endpoint = bind_runtime_iroh_endpoint(policy, server_secret)
            .await
            .unwrap()
            .unwrap();
        let server_addr = server_endpoint.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let listener_handle = handle.clone();
        drop(handle);

        let client_endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_uni_streams(VarInt::from_u32(1))
                    .build(),
            )
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();

        let listener = async move {
            let served = serve_runtime_iroh_control_listener(
                &server_endpoint,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(1024 * 1024, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(served, 2);
            server_endpoint.close().await;
        };
        let clients = async {
            let first_connection = client_endpoint
                .connect(server_addr.clone(), MEZZANINE_IROH_ALPN)
                .await
                .unwrap();
            let (mut first_send, mut first_recv) = first_connection.open_bi().await.unwrap();
            let first_initialize = serde_json::json!({
                "jsonrpc": "2.0",
                "id": "first-init",
                "method": "control/initialize",
                "params": {
                    "requested_role": "primary",
                    "requested_version": 2,
                    "client_name": "same-endpoint",
                    "detach_primary_on_disconnect": true,
                    "event_stream_version": 1,
                    "client": {
                        "name": "same-endpoint",
                        "interactive": true,
                        "terminal": {"columns": 80, "rows": 24, "term": "xterm-256color"}
                    },
                    "authentication": {
                        "mechanism": "extension:iroh_invitation",
                        "token": invitation.token.expose_secret()
                    }
                }
            })
            .to_string();
            first_send
                .write_all(&encode_control_body(&first_initialize))
                .await
                .unwrap();
            first_send.flush().await.unwrap();
            let first_body = read_test_control_body(&mut first_recv).await;
            let first_json: serde_json::Value = serde_json::from_str(&first_body).unwrap();
            let first_client_id = first_json["result"]["client"]["id"]
                .as_str()
                .unwrap()
                .to_string();
            let device_credential = first_json["result"]["device_credential"]
                .as_str()
                .unwrap()
                .to_string();
            let mut first_events = first_connection.accept_uni().await.unwrap();
            let mut first_preface = vec![0u8; MEZZANINE_IROH_EVENT_STREAM_PREFACE.len()];
            first_events.read_exact(&mut first_preface).await.unwrap();
            assert_eq!(first_preface, MEZZANINE_IROH_EVENT_STREAM_PREFACE);

            let second_connection = client_endpoint
                .connect(server_addr, MEZZANINE_IROH_ALPN)
                .await
                .unwrap();
            let (mut second_send, mut second_recv) = second_connection.open_bi().await.unwrap();
            let second_initialize = serde_json::json!({
                "jsonrpc": "2.0",
                "id": "second-init",
                "method": "control/initialize",
                "params": {
                    "requested_role": "primary",
                    "requested_version": 2,
                    "client_name": "same-endpoint",
                    "detach_primary_on_disconnect": true,
                    "event_stream_version": 1,
                    "client": {
                        "name": "same-endpoint",
                        "interactive": true,
                        "terminal": {"columns": 100, "rows": 30, "term": "xterm-256color"}
                    },
                    "authentication": {
                        "mechanism": "extension:iroh_device",
                        "token": device_credential
                    }
                }
            })
            .to_string();
            second_send
                .write_all(&encode_control_body(&second_initialize))
                .await
                .unwrap();
            second_send.flush().await.unwrap();
            let second_body = read_test_control_body(&mut second_recv).await;
            let second_json: serde_json::Value = serde_json::from_str(&second_body).unwrap();
            let second_client_id = second_json["result"]["client"]["id"]
                .as_str()
                .unwrap()
                .to_string();
            assert_ne!(first_client_id, second_client_id);
            let mut second_events = second_connection.accept_uni().await.unwrap();
            let mut second_preface = vec![0u8; MEZZANINE_IROH_EVENT_STREAM_PREFACE.len()];
            second_events.read_exact(&mut second_preface).await.unwrap();
            assert_eq!(second_preface, MEZZANINE_IROH_EVENT_STREAM_PREFACE);

            first_send.finish().unwrap();
            first_connection.close(VarInt::from_u32(0), b"first client complete");

            let detached_event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let body = read_test_control_body(&mut second_events).await;
                    let frame: serde_json::Value = serde_json::from_str(&body).unwrap();
                    if frame["params"]["event_type"] == "client_detached"
                        && body.contains(&first_client_id)
                    {
                        break body;
                    }
                }
            })
            .await
            .unwrap();
            assert!(detached_event.contains(&first_client_id));

            let kill = r#"{"jsonrpc":"2.0","id":"kill","method":"session/kill","params":{"force":true,"idempotency_key":"two-client-kill"}}"#;
            second_send
                .write_all(&encode_control_body(kill))
                .await
                .unwrap();
            second_send.finish().unwrap();
            let kill_body = read_test_control_body(&mut second_recv).await;
            assert!(kill_body.contains(r#""killed":true"#), "{kill_body}");
            second_connection.close(VarInt::from_u32(0), b"test complete");
            client_endpoint.close().await;
        };

        let actor_task = tokio::spawn(actor.run());
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let ((), ()) = tokio::join!(listener, clients);
        })
        .await
        .unwrap();
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_client_connector_pairs_persists_and_reconnects_without_unix_fallback() {
        use crate::cli::{IrohControlTarget, exchange_iroh_control_request};
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{
            RemoteClientProfileStore, RemoteRoleCeiling, RemoteTrustStore,
        };
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-client-connector-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let server_root = root.join("server");
        let client_root = root.join("client");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&server_root).unwrap();
        std::fs::create_dir_all(&client_root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(server_root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = RemoteTrustStore::under_config_root(&server_root, &session_id).unwrap();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let server_policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 2,
            max_streams_per_connection: 1,
            setup_timeout: std::time::Duration::from_secs(10),
            idle_timeout: std::time::Duration::from_secs(30),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server_endpoint = bind_runtime_iroh_endpoint(server_policy, server_secret)
            .await
            .unwrap()
            .unwrap();
        let client_policy = RuntimeIrohTransportPolicy {
            setup_timeout: std::time::Duration::from_secs(10),
            idle_timeout: std::time::Duration::from_secs(30),
            ..RuntimeIrohTransportPolicy::default()
        };
        assert!(!client_policy.enabled);
        let server_addr = server_endpoint.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let listener_handle = handle.clone();
        drop(handle);

        let listener = async move {
            let served = serve_runtime_iroh_control_listener(
                &server_endpoint,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(1024 * 1024, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(served, 3);
            server_endpoint.close().await;
        };
        let client = async {
            let invalid_profile_target = IrohControlTarget::Invitation {
                profile_name: "x".repeat(129),
                server_addr: server_addr.clone(),
                token: invitation.token.clone(),
                role: RemoteRoleCeiling::Primary,
                scope: crate::security::remote::RemoteClientProfileScope::LegacySession,
                expires_at_unix_seconds: invitation.expires_at_unix_seconds,
            };
            let save_error = exchange_iroh_control_request(
                &client_root,
                &client_policy,
                &invalid_profile_target,
                "window/list",
                "{}",
            )
            .await
            .unwrap_err();
            assert_eq!(save_error.kind(), crate::error::MezErrorKind::InvalidArgs);
            assert_eq!(trust.list_records().unwrap().len(), 1);
            assert!(
                RemoteClientProfileStore::under_config_root(&client_root)
                    .load("workstation")
                    .unwrap()
                    .is_none()
            );

            let invitation_target = IrohControlTarget::Invitation {
                profile_name: "workstation".to_string(),
                server_addr: server_addr.clone(),
                token: invitation.token.clone(),
                role: RemoteRoleCeiling::Primary,
                scope: crate::security::remote::RemoteClientProfileScope::LegacySession,
                expires_at_unix_seconds: invitation.expires_at_unix_seconds,
            };
            let first = exchange_iroh_control_request(
                &client_root,
                &client_policy,
                &invitation_target,
                "window/list",
                "{}",
            )
            .await
            .unwrap();
            assert!(first.contains(r#""result""#), "{first}");

            let records = trust.list_records().unwrap();
            assert_eq!(records.len(), 1);
            let profile = RemoteClientProfileStore::under_config_root(&client_root)
                .load("workstation")
                .unwrap()
                .unwrap();
            assert_eq!(profile.server_addr.id, server_addr.id);

            let stale_addr: std::net::SocketAddr = "192.0.2.55:9".parse().unwrap();
            let mut stale_profile = profile.clone();
            stale_profile.server_addr = stale_profile.server_addr.with_ip_addr(stale_addr);
            RemoteClientProfileStore::under_config_root(&client_root)
                .save(&stale_profile)
                .unwrap();

            let second = exchange_iroh_control_request(
                &client_root,
                &client_policy,
                &IrohControlTarget::Profile(stale_profile),
                "session/kill",
                r#"{"force":true,"idempotency_key":"connector-kill"}"#,
            )
            .await
            .unwrap();
            assert!(second.contains(r#""killed":true"#), "{second}");
            let refreshed = RemoteClientProfileStore::under_config_root(&client_root)
                .load("workstation")
                .unwrap()
                .unwrap();
            assert_eq!(refreshed.server_addr.id, server_addr.id);
            assert!(
                !refreshed
                    .server_addr
                    .ip_addrs()
                    .any(|addr| *addr == stale_addr)
            );
        };

        let actor_task = tokio::spawn(actor.run());
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let ((), ()) = tokio::join!(listener, client);
        })
        .await
        .unwrap();
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_listener_rejects_wrong_alpn_without_runtime_authority() {
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let service = RuntimeServiceFixture::new().build();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            setup_timeout: IROH_ENDPOINT_TEST_SETUP_TIMEOUT,
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server = bind_runtime_iroh_endpoint(policy, SecretKey::generate())
            .await
            .unwrap()
            .unwrap();
        let server_addr = server.endpoint().addr();
        let diagnostics = server.diagnostics.clone();
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            let result = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await;
            server.close().await;
            result
        });
        let actor_task = tokio::spawn(actor.run());
        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            client.connect(server_addr, b"mezzanine/wrong/1"),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(!error.to_string().is_empty());
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            crate::runtime::RuntimeLifecycleState::Running
        );
        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        assert_eq!(listener.await.unwrap().unwrap(), 0);
        let snapshot = diagnostics.snapshot();
        assert!(!snapshot.listener_active);
        assert_eq!(snapshot.active_connections, 0);
        assert_eq!(snapshot.connections_accepted, 0);
        assert!(snapshot.connections_rejected >= 1, "{snapshot:?}");
        assert_eq!(snapshot.setup_failures, snapshot.connections_rejected);
        assert_eq!(snapshot.setup_successes, 0);
        assert_eq!(snapshot.shutdown_aborts, 0);
        client.close().await;
        actor_task.abort();
        let _ = actor_task.await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_iroh_client_does_not_stop_later_authorized_control() {
        use crate::cli::{IrohControlTarget, exchange_iroh_control_request};
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-malformed-isolation-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let server_root = root.join("server");
        let client_root = root.join("client");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&server_root).unwrap();
        std::fs::create_dir_all(&client_root).unwrap();
        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(server_root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = RemoteTrustStore::under_config_root(&server_root, &session_id).unwrap();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 2,
            setup_timeout: std::time::Duration::from_secs(3),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server = bind_runtime_iroh_endpoint(policy.clone(), server_secret)
            .await
            .unwrap()
            .unwrap();
        let server_addr = server.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let listener_handle = handle.clone();
        drop(handle);
        let listener = async move {
            let served = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(served, 2);
            server.close().await;
        };
        let clients = async {
            let malformed = Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .clear_address_lookup()
                .portmapper_config(PortmapperConfig::Disabled)
                .bind()
                .await
                .unwrap();
            let connection = malformed
                .connect(server_addr.clone(), MEZZANINE_IROH_ALPN)
                .await
                .unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut send, b"Content-Length: 9999999\r\n\r\n")
                .await
                .unwrap();
            send.finish().unwrap();
            let _ = recv.read_to_end(4096).await;
            connection.close(VarInt::from_u32(1), b"malformed test");
            malformed.close().await;

            let body = exchange_iroh_control_request(
                &client_root,
                &policy,
                &IrohControlTarget::Invitation {
                    profile_name: "authorized-after-malformed".to_string(),
                    server_addr,
                    token: invitation.token,
                    role: RemoteRoleCeiling::Primary,
                    scope: crate::security::remote::RemoteClientProfileScope::LegacySession,
                    expires_at_unix_seconds: invitation.expires_at_unix_seconds,
                },
                "session/kill",
                r#"{"force":true,"idempotency_key":"malformed-isolation-kill"}"#,
            )
            .await
            .unwrap();
            assert!(body.contains(r#""killed":true"#), "{body}");
        };
        let actor_task = tokio::spawn(actor.run());
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let ((), ()) = tokio::join!(listener, clients);
        })
        .await
        .unwrap();
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_setup_timeout_isolated_and_revoked_profile_rejected() {
        use crate::cli::{IrohControlTarget, exchange_iroh_control_request};
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{
            RemoteClientProfileStore, RemoteRoleCeiling, RemoteTrustStore,
        };
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-timeout-revocation-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let server_root = root.join("server");
        let client_root = root.join("client");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&server_root).unwrap();
        std::fs::create_dir_all(&client_root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(server_root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = RemoteTrustStore::under_config_root(&server_root, &session_id).unwrap();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let mut policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 2,
            max_streams_per_connection: 1,
            setup_timeout: std::time::Duration::from_secs(3),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let mut server = bind_runtime_iroh_endpoint(policy.clone(), server_secret)
            .await
            .unwrap()
            .unwrap();
        policy.setup_timeout = std::time::Duration::from_millis(500);
        server.policy.setup_timeout = policy.setup_timeout;
        let server_addr = server.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            let served = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            server.close().await;
            served
        });
        let actor_task = tokio::spawn(actor.run());

        let stalled = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let stalled_connection = stalled
            .connect(server_addr.clone(), MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        stalled_connection.close(VarInt::from_u32(1), b"setup timeout test");
        stalled.close().await;

        let paired = exchange_iroh_control_request(
            &client_root,
            &policy,
            &IrohControlTarget::Invitation {
                profile_name: "revoked-client".to_string(),
                server_addr,
                token: invitation.token,
                role: RemoteRoleCeiling::Primary,
                scope: crate::security::remote::RemoteClientProfileScope::LegacySession,
                expires_at_unix_seconds: invitation.expires_at_unix_seconds,
            },
            "window/list",
            "{}",
        )
        .await
        .unwrap();
        assert!(paired.contains(r#""result""#), "{paired}");

        let profile = RemoteClientProfileStore::under_config_root(&client_root)
            .load("revoked-client")
            .unwrap()
            .unwrap();
        let records = trust.list_records().unwrap();
        assert_eq!(records.len(), 1);
        trust
            .revoke_record(
                &records[0].id,
                Some("revocation regression"),
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let error = exchange_iroh_control_request(
            &client_root,
            &policy,
            &IrohControlTarget::Profile(profile),
            "window/list",
            "{}",
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
        assert!(error.message().contains("trust initialization"), "{error}");

        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        assert_eq!(listener.await.unwrap(), 3);
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    /// Verifies direct-session authority watches retain a connection across an
    /// unrelated trust mutation, then promptly close its control and active
    /// X11 route when the exact bound trust record is revoked over local Unix
    /// control. A fresh connection from the revoked endpoint must also fail.
    #[tokio::test(flavor = "current_thread")]
    async fn direct_iroh_revocation_terminates_live_control_and_x11_route() {
        use secrecy::ExposeSecret;

        use crate::control::{decode_control_frame, encode_control_body};
        use crate::host::async_runtime::{
            AsyncRuntimeActorConfig, AsyncRuntimeSessionActor,
            serve_async_runtime_control_listener_with_snapshots,
        };
        use crate::security::remote::RemoteRoleCeiling;
        use crate::test_support::runtime::RuntimeServiceFixture;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-live-revocation-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let unix_path = root.join("control.sock");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = service
            .integration
            .ensure_remote_trust_store(&session_id)
            .unwrap()
            .clone();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let proxy = crate::runtime::x11::RuntimeX11Proxy::prepare(&root).unwrap();
        let proxy_handle = proxy.handle();
        let authority_path = proxy_handle.authority_path().to_path_buf();
        service.set_runtime_x11_proxy(proxy_handle.clone());

        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 2,
            max_streams_per_connection: 1,
            setup_timeout: IROH_ENDPOINT_TEST_SETUP_TIMEOUT,
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let mut server = bind_runtime_iroh_endpoint(policy, server_secret)
            .await
            .unwrap()
            .unwrap();
        server.authority = Some(RuntimeIrohAuthority {
            trust: trust.clone(),
            server_endpoint_id: server_endpoint_id.clone(),
        });
        let server_addr = server.endpoint().addr();
        let unix_listener = tokio::net::UnixListener::bind(&unix_path).unwrap();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let actor_task = tokio::spawn(actor.run());
        let proxy_task = tokio::spawn(proxy.serve());

        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            let served = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(1024 * 1024, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            server.close().await;
            served
        });

        let client_endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_bidi_streams(VarInt::from_u32(2))
                    .build(),
            )
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let connection = client_endpoint
            .connect(server_addr.clone(), MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "control/initialize",
            "params": {
                "client_name": "revoked-primary",
                "requested_version": 2,
                "requested_role": "primary",
                "x11_forwarding": {
                    "version": 2,
                    "mode": "untrusted",
                    "auth_protocol": "MIT-MAGIC-COOKIE-1",
                    "fake_cookie_base64": "EREREREREREREREREREREQ==",
                    "takeover": false
                },
                "client": {
                    "name": "revoked-primary",
                    "interactive": true,
                    "terminal": {
                        "columns": 80,
                        "rows": 24,
                        "term": "xterm-256color"
                    }
                },
                "authentication": {
                    "mechanism": "extension:iroh_invitation",
                    "token": invitation.token.expose_secret()
                }
            }
        })
        .to_string();
        send.write_all(&encode_control_body(&initialize))
            .await
            .unwrap();
        send.flush().await.unwrap();
        let initialize_body = read_test_control_body(&mut recv).await;
        let initialize_value: serde_json::Value = serde_json::from_str(&initialize_body).unwrap();
        let device_credential = initialize_value["result"]["device_credential"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            initialize_value["result"]["capabilities"]["features"]["x11_forwarding"],
            true
        );
        assert!(proxy_handle.diagnostics().route_active);
        assert!(!std::fs::read(&authority_path).unwrap().is_empty());

        trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Observer,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        send.write_all(&encode_control_body(
            r#"{"jsonrpc":"2.0","id":"list","method":"window/list","params":{}}"#,
        ))
        .await
        .unwrap();
        send.flush().await.unwrap();
        let list_body = read_test_control_body(&mut recv).await;
        assert!(list_body.contains(r#""result""#), "{list_body}");

        let mut setup = vec![0u8; 48];
        setup[0] = b'l';
        setup[2..4].copy_from_slice(&11u16.to_le_bytes());
        setup[4..6].copy_from_slice(&0u16.to_le_bytes());
        setup[6..8].copy_from_slice(&18u16.to_le_bytes());
        setup[8..10].copy_from_slice(&16u16.to_le_bytes());
        setup[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
        setup[32..48].copy_from_slice(&[0x11; 16]);
        let mut remote_x = tokio::net::TcpStream::connect((
            std::net::Ipv4Addr::LOCALHOST,
            6000 + proxy_handle.display_number(),
        ))
        .await
        .unwrap();
        remote_x.write_all(&setup).await.unwrap();
        remote_x.flush().await.unwrap();
        let (_x11_send, mut x11_recv) =
            tokio::time::timeout(std::time::Duration::from_secs(3), connection.accept_bi())
                .await
                .unwrap()
                .unwrap();
        let mut forwarded_setup = vec![0u8; crate::runtime::x11::X11_STREAM_PREFACE_BYTES + 48];
        x11_recv.read_exact(&mut forwarded_setup).await.unwrap();
        assert_eq!(
            &forwarded_setup[crate::runtime::x11::X11_STREAM_PREFACE_BYTES..],
            setup.as_slice()
        );
        assert_eq!(proxy_handle.diagnostics().active_streams, 1);

        let records = trust.list_records().unwrap();
        let record = records
            .iter()
            .find(|record| record.endpoint_id == client_endpoint.id().to_string())
            .unwrap();
        let local_initialize = r#"{"jsonrpc":"2.0","id":"unix-init","method":"control/initialize","params":{"client_name":"local-recovery","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"local-recovery","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#;
        let revoke = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "revoke",
            "method": "remote/client/revoke",
            "params": {
                "client_id": record.id,
                "reason": "live revocation regression",
                "idempotency_key": "live-revocation"
            }
        })
        .to_string();
        let unix_handle = handle.clone();
        let unix_server = tokio::spawn(async move {
            serve_async_runtime_control_listener_with_snapshots(
                &unix_listener,
                &unix_handle,
                AsyncRuntimeControlConnectionConfig::new(
                    1024 * 1024,
                    crate::runtime::current_effective_uid(),
                )
                .unwrap(),
                None,
                1,
                |accepted, _| accepted >= 1,
            )
            .await
            .unwrap()
        });
        let mut unix = tokio::net::UnixStream::connect(&unix_path).await.unwrap();
        unix.write_all(&encode_control_body(local_initialize))
            .await
            .unwrap();
        unix.write_all(&encode_control_body(&revoke)).await.unwrap();
        unix.shutdown().await.unwrap();
        let mut unix_output = Vec::new();
        unix.read_to_end(&mut unix_output).await.unwrap();
        let (local_initialize_body, consumed) =
            decode_control_frame(&unix_output, 1024 * 1024).unwrap();
        let (revoke_body, _) = decode_control_frame(&unix_output[consumed..], 1024 * 1024).unwrap();
        assert!(
            local_initialize_body.contains(r#""granted_role":"primary""#),
            "{local_initialize_body}"
        );
        assert!(
            revoke_body.contains(r#""revoked_at_unix_seconds""#),
            "{revoke_body}"
        );
        assert_eq!(unix_server.await.unwrap(), 1);

        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let diagnostics = proxy_handle.diagnostics();
                if !diagnostics.route_active && diagnostics.active_streams == 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("exact trust revocation should deactivate the direct X11 route");
        assert_eq!(std::fs::read(&authority_path).unwrap(), Vec::<u8>::new());
        let mut byte = [0u8; 1];
        let x11_closed =
            tokio::time::timeout(std::time::Duration::from_secs(3), remote_x.read(&mut byte))
                .await
                .expect("exact trust revocation should close an established X11 socket");
        assert!(matches!(x11_closed, Ok(0) | Err(_)));
        let control_closed =
            tokio::time::timeout(std::time::Duration::from_secs(3), recv.read(&mut byte))
                .await
                .expect("exact trust revocation should close the direct control stream");
        assert!(matches!(control_closed, Ok(None) | Err(_)));

        let reconnect = client_endpoint
            .connect(server_addr, MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        let (mut reconnect_send, mut reconnect_recv) = reconnect.open_bi().await.unwrap();
        let reconnect_initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "reconnect",
            "method": "control/initialize",
            "params": {
                "client_name": "revoked-primary",
                "requested_version": 2,
                "requested_role": "primary",
                "client": {
                    "name": "revoked-primary",
                    "interactive": true,
                    "terminal": {
                        "columns": 80,
                        "rows": 24,
                        "term": "xterm-256color"
                    }
                },
                "authentication": {
                    "mechanism": "extension:iroh_device",
                    "token": device_credential
                }
            }
        })
        .to_string();
        reconnect_send
            .write_all(&encode_control_body(&reconnect_initialize))
            .await
            .unwrap();
        reconnect_send.flush().await.unwrap();
        let reconnect_body = read_test_control_body(&mut reconnect_recv).await;
        assert!(reconnect_body.contains(r#""error""#), "{reconnect_body}");
        assert!(
            reconnect_body.contains(r#""mezzanine_code":"forbidden""#),
            "{reconnect_body}"
        );
        reconnect.close(VarInt::from_u32(0), b"revoked reconnect rejected");

        handle.shutdown().await.unwrap();
        drop(handle);
        assert_eq!(listener.await.unwrap(), 2);
        proxy_task.abort();
        let _ = proxy_task.await;
        actor_task.abort();
        let _ = actor_task.await;
        client_endpoint.close().await;
        let _ = std::fs::remove_dir_all(root);
    }

    /// A direct-Iroh primary with an active X11 stream must receive the
    /// terminal `:exit` response before route, socket, and control teardown.
    #[tokio::test(flavor = "current_thread")]
    async fn direct_iroh_x11_exit_flushes_response_before_teardown() {
        use secrecy::ExposeSecret;

        use crate::control::encode_control_body;
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::RemoteRoleCeiling;
        use crate::test_support::runtime::RuntimeServiceFixture;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-x11-exit-flush-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = service
            .integration
            .ensure_remote_trust_store(&session_id)
            .unwrap()
            .clone();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let proxy = crate::runtime::x11::RuntimeX11Proxy::prepare(&root).unwrap();
        let proxy_handle = proxy.handle();
        service.set_runtime_x11_proxy(proxy_handle.clone());

        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            max_streams_per_connection: 1,
            setup_timeout: IROH_ENDPOINT_TEST_SETUP_TIMEOUT,
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let mut server = bind_runtime_iroh_endpoint(policy, server_secret)
            .await
            .unwrap()
            .unwrap();
        server.authority = Some(RuntimeIrohAuthority {
            trust,
            server_endpoint_id,
        });
        let server_addr = server.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let actor_task = tokio::spawn(actor.run());
        let proxy_task = tokio::spawn(proxy.serve());
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            let served = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(1024 * 1024, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            server.close().await;
            served
        });

        let client_endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_bidi_streams(VarInt::from_u32(2))
                    .build(),
            )
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let connection = client_endpoint
            .connect(server_addr, MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "control/initialize",
            "params": {
                "client_name": "exit-primary",
                "requested_version": 2,
                "requested_role": "primary",
                "x11_forwarding": {
                    "version": 2,
                    "mode": "untrusted",
                    "auth_protocol": "MIT-MAGIC-COOKIE-1",
                    "fake_cookie_base64": "EREREREREREREREREREREQ==",
                    "takeover": false
                },
                "client": {
                    "name": "exit-primary",
                    "interactive": true,
                    "terminal": {
                        "columns": 80,
                        "rows": 24,
                        "term": "xterm-256color"
                    }
                },
                "authentication": {
                    "mechanism": "extension:iroh_invitation",
                    "token": invitation.token.expose_secret()
                }
            }
        })
        .to_string();
        send.write_all(&encode_control_body(&initialize))
            .await
            .unwrap();
        send.flush().await.unwrap();
        let initialize_body = read_test_control_body(&mut recv).await;
        assert!(
            initialize_body.contains(r#""x11_forwarding":true"#),
            "{initialize_body}"
        );
        assert!(proxy_handle.diagnostics().route_active);

        let mut setup = vec![0u8; 48];
        setup[0] = b'l';
        setup[2..4].copy_from_slice(&11u16.to_le_bytes());
        setup[4..6].copy_from_slice(&0u16.to_le_bytes());
        setup[6..8].copy_from_slice(&18u16.to_le_bytes());
        setup[8..10].copy_from_slice(&16u16.to_le_bytes());
        setup[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
        setup[32..48].copy_from_slice(&[0x11; 16]);
        let mut remote_x = tokio::net::TcpStream::connect((
            std::net::Ipv4Addr::LOCALHOST,
            6000 + proxy_handle.display_number(),
        ))
        .await
        .unwrap();
        remote_x.write_all(&setup).await.unwrap();
        remote_x.flush().await.unwrap();
        let (_x11_send, mut x11_recv) =
            tokio::time::timeout(std::time::Duration::from_secs(3), connection.accept_bi())
                .await
                .unwrap()
                .unwrap();
        let mut forwarded_setup = vec![0u8; crate::runtime::x11::X11_STREAM_PREFACE_BYTES + 48];
        x11_recv.read_exact(&mut forwarded_setup).await.unwrap();
        assert_eq!(proxy_handle.diagnostics().active_streams, 1);

        let exit = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"exit","method":"terminal/step","params":{"idempotency_key":"direct-iroh-x11-exit","client_size":{"columns":80,"rows":24},"render":false,"input_bytes":[1,58,101,120,105,116,13]}}"#,
        );
        send.write_all(&exit).await.unwrap();
        send.flush().await.unwrap();
        let exit_body = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            read_test_control_body(&mut recv),
        )
        .await
        .expect("terminal exit response must precede direct-Iroh control teardown");
        assert!(
            exit_body.contains(r#""session_terminated":true"#),
            "{exit_body}"
        );

        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let diagnostics = proxy_handle.diagnostics();
                if !diagnostics.route_active && diagnostics.active_streams == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("X11 teardown must follow the terminal exit response");
        let mut byte = [0u8; 1];
        let x11_closed =
            tokio::time::timeout(std::time::Duration::from_secs(3), remote_x.read(&mut byte))
                .await
                .expect("terminal exit must close the active X11 socket");
        assert!(matches!(x11_closed, Ok(0) | Err(_)));

        drop(handle);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(3), listener)
                .await
                .expect("direct-Iroh listener must drain after terminal exit")
                .unwrap(),
            1
        );
        proxy_task.abort();
        let _ = proxy_task.await;
        actor_task.abort();
        let _ = actor_task.await;
        connection.close(VarInt::from_u32(0), b"test complete");
        client_endpoint.close().await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn iroh_abrupt_loss_detaches_primary_and_blocks_extra_stream() {
        use secrecy::ExposeSecret;

        use crate::control::{decode_control_frame, encode_control_body};
        use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
        use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
        use crate::test_support::runtime::RuntimeServiceFixture;

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-abrupt-loss-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = RemoteTrustStore::under_config_root(&root, &session_id).unwrap();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            max_streams_per_connection: 1,
            setup_timeout: IROH_ENDPOINT_TEST_SETUP_TIMEOUT,
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let server = bind_runtime_iroh_endpoint(policy, server_secret)
            .await
            .unwrap()
            .unwrap();
        let server_addr = server.endpoint().addr();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let listener_handle = handle.clone();
        let listener = tokio::spawn(async move {
            let served = serve_runtime_iroh_control_listener(
                &server,
                &listener_handle,
                AsyncRuntimeControlConnectionConfig::new(4096, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            server.close().await;
            served
        });
        let actor_task = tokio::spawn(actor.run());

        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let connection = client
            .connect(server_addr, MEZZANINE_IROH_ALPN)
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let second_stream =
            tokio::time::timeout(std::time::Duration::from_millis(250), connection.open_bi()).await;
        assert!(
            second_stream.is_err(),
            "a second concurrent control stream must be blocked"
        );

        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "control/initialize",
            "params": {
                "client_name": "abrupt-primary",
                "requested_version": 2,
                "requested_role": "primary",
                "detach_primary_on_disconnect": true,
                "client": {
                    "name": "abrupt-primary",
                    "interactive": true,
                    "terminal": {
                        "columns": 80,
                        "rows": 24,
                        "term": "xterm-256color"
                    }
                },
                "authentication": {
                    "mechanism": "extension:iroh_invitation",
                    "token": invitation.token.expose_secret()
                }
            }
        })
        .to_string();
        tokio::io::AsyncWriteExt::write_all(&mut send, &encode_control_body(&initialize))
            .await
            .unwrap();
        let initialize_body = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut response = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = tokio::io::AsyncReadExt::read(&mut recv, &mut buffer)
                    .await
                    .unwrap();
                assert!(read > 0, "initialize response must precede stream closure");
                response.extend_from_slice(&buffer[..read]);
                if let Ok((body, _)) = decode_control_frame(&response, 1024 * 1024) {
                    break body;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            initialize_body.contains(r#""granted_role":"primary""#),
            "{initialize_body}"
        );
        connection.close(VarInt::from_u32(1), b"abrupt client loss");
        client.close().await;

        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if handle.lifecycle_state().await.unwrap()
                    == crate::runtime::RuntimeLifecycleState::Detached
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let _ = handle.shutdown().await.unwrap();
        drop(handle);
        assert_eq!(listener.await.unwrap(), 1);
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unix_and_iroh_control_remain_simultaneously_available() {
        use crate::cli::{IrohControlTarget, exchange_iroh_control_request};
        use crate::control::{decode_control_frame, encode_control_body};
        use crate::host::async_runtime::{
            AsyncRuntimeActorConfig, AsyncRuntimeSessionActor,
            serve_async_runtime_control_listener_with_snapshots,
        };
        use crate::security::remote::{RemoteRoleCeiling, RemoteTrustStore};
        use crate::test_support::runtime::RuntimeServiceFixture;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = std::env::temp_dir().join(format!(
            "mez-iroh-unix-coexistence-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let server_root = root.join("server");
        let client_root = root.join("client");
        let unix_path = root.join("control.sock");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&server_root).unwrap();
        std::fs::create_dir_all(&client_root).unwrap();

        let mut service = RuntimeServiceFixture::new().build();
        service.set_config_root(server_root.clone());
        let session_id = service.session().id.to_string();
        let (server_secret, server_endpoint_id) = {
            let identity = service
                .integration
                .ensure_remote_endpoint_identity(&session_id)
                .unwrap();
            (
                identity.secret_key().clone(),
                identity.endpoint_id().to_string(),
            )
        };
        let trust = RemoteTrustStore::under_config_root(&server_root, &session_id).unwrap();
        let invitation = trust
            .create_invitation(
                &server_endpoint_id,
                RemoteRoleCeiling::Primary,
                600,
                crate::runtime::current_unix_seconds(),
            )
            .unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            max_connections: 1,
            max_streams_per_connection: 1,
            setup_timeout: std::time::Duration::from_secs(3),
            idle_timeout: std::time::Duration::from_secs(5),
            ..RuntimeIrohTransportPolicy::default()
        };
        let iroh_server = bind_runtime_iroh_endpoint(policy.clone(), server_secret)
            .await
            .unwrap()
            .unwrap();
        let server_addr = iroh_server.endpoint().addr();
        let unix_listener = tokio::net::UnixListener::bind(&unix_path).unwrap();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let actor_task = tokio::spawn(actor.run());

        let unix_handle = handle.clone();
        let unix_server = async {
            let served = serve_async_runtime_control_listener_with_snapshots(
                &unix_listener,
                &unix_handle,
                AsyncRuntimeControlConnectionConfig::new(
                    1024 * 1024,
                    crate::runtime::current_effective_uid(),
                )
                .unwrap(),
                None,
                u64::MAX,
                |accepted, _| accepted >= 1,
            )
            .await
            .unwrap();
            assert_eq!(served, 1);
        };
        let unix_client = async {
            let mut stream = tokio::net::UnixStream::connect(&unix_path).await.unwrap();
            let initialize = encode_control_body(
                r#"{"jsonrpc":"2.0","id":"unix-init","method":"control/initialize","params":{"client_name":"local-recovery","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"local-recovery","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
            );
            let status = encode_control_body(
                r#"{"jsonrpc":"2.0","id":"unix-status","method":"remote/status","params":{}}"#,
            );
            stream.write_all(&initialize).await.unwrap();
            stream.write_all(&status).await.unwrap();
            stream.shutdown().await.unwrap();
            let mut output = Vec::new();
            stream.read_to_end(&mut output).await.unwrap();
            let (initialize_body, consumed) = decode_control_frame(&output, 1024 * 1024).unwrap();
            let (status_body, _) = decode_control_frame(&output[consumed..], 1024 * 1024).unwrap();
            assert!(initialize_body.contains(r#""granted_role":"primary""#));
            assert!(status_body.contains(r#""endpoint_id""#), "{status_body}");
        };

        let iroh_handle = handle.clone();
        let iroh_server_task = tokio::spawn(async move {
            let served = serve_runtime_iroh_control_listener(
                &iroh_server,
                &iroh_handle,
                AsyncRuntimeControlConnectionConfig::new(1024 * 1024, 0).unwrap(),
                None,
            )
            .await
            .unwrap();
            iroh_server.close().await;
            served
        });

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let ((), ()) = tokio::join!(unix_server, unix_client);
        })
        .await
        .unwrap();
        let remote_body = exchange_iroh_control_request(
            &client_root,
            &policy,
            &IrohControlTarget::Invitation {
                profile_name: "coexisting-remote".to_string(),
                server_addr,
                token: invitation.token,
                role: RemoteRoleCeiling::Primary,
                scope: crate::security::remote::RemoteClientProfileScope::LegacySession,
                expires_at_unix_seconds: invitation.expires_at_unix_seconds,
            },
            "session/kill",
            r#"{"force":true,"idempotency_key":"coexistence-kill"}"#,
        )
        .await
        .unwrap();
        assert!(remote_body.contains(r#""killed":true"#), "{remote_body}");

        drop(handle);
        assert_eq!(iroh_server_task.await.unwrap(), 1);
        actor_task.abort();
        let _ = actor_task.await;
        let _ = std::fs::remove_dir_all(root);
    }
}
