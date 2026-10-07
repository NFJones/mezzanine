/** Client-local OpenCode TUI observer. The server event bus is not authority:
 * only this client's current route plus cached root metadata chooses a session.
 * No navigation, input/permission mutation, network client calls, messages/parts
 * reads or transcript access. Counter observations remain unavailable until
 * durable continuity is implemented. Module loading performs no I/O. */
import { randomUUID } from "node:crypto";
import { createPersistentTelemetryClient, discoverPersistentRoute } from "./persistent_client.mjs";
import { createOpenCodeObserver } from "./opencode_observer.mjs";

const id = value => typeof value === "string" && /^[A-Za-z0-9_.:-]{1,128}$/.test(value);
const ownerKey = Symbol.for("mezzanine.opencode.tui-owner.v1");
const eventTypes = ["session.created", "session.updated", "session.deleted", "session.status",
  "session.idle", "session.error", "permission.asked", "permission.replied", "question.asked",
  "question.replied", "question.rejected", "message.updated"];

/** Snapshot only inert client-selected root metadata. Other API fields may
 * contain prompts/paths; they are neither serialized nor retained. */
function selection(api) {
  try {
    if (api.state.ready !== true) return;
    const route = api.route.current;
    if (route?.name !== "session") return;
    const session = route.params?.sessionID;
    if (!id(session)) return;
    const info = api.state.session.get(session);
    const observed = info?.id;
    const parent = info?.parentID;
    if (observed !== session || parent != null) return;
    const status = api.state.session.status(session)?.type;
    const waits = [];
    let completeWaitCache = true;
    for (const kind of ["permission", "question"]) {
      const read = api.state.session[kind];
      if (typeof read !== "function") { completeWaitCache = false; continue; }
      const entries = read.call(api.state.session, session);
      if (!Array.isArray(entries) || entries.length + waits.length > 256) return;
      for (const entry of entries) {
        const request = entry?.id;
        const observedSession = entry?.sessionID;
        if (!id(request) || observedSession !== session) return;
        waits.push({ kind, request });
      }
    }
    return { session, waits, status: completeWaitCache && ["busy", "retry", "idle"].includes(status) ? status : undefined };
  } catch { /* stale/unsupported cache is unavailable, not a guessed session */ }
}

/** Initializes only under a genuine client-local TUI API. Injectable factories
 * are offline test seams, never vendor payload or process-visible authority. */
export function registerOpenCodeTuiObserver(api, options = {}) {
  const env = options.env ?? process.env;
  if (!discoverPersistentRoute(env) || !api?.route || !api?.state?.session
      || typeof api.state.session.get !== "function" || typeof api.state.session.status !== "function"
      || typeof api.event?.on !== "function" || typeof api.lifecycle?.onDispose !== "function"
      || !api.lifecycle.signal) return;
  const owner = options.owner ?? (globalThis[ownerKey] ??= {});
  if (owner.lease) return;
  const lease = {};
  owner.lease = lease;
  let current;
  let disposed = false;
  let cleaned = false;
  const now = options.now ?? Date.now;
  const unsubscribers = [];
  const disconnect = (source, reason = "transport") => {
    source.cancelled = true;
    source.lossReason = reason;
    source.retryAfter = now() + 5000;
    source.client?.disconnect();
  };
  const queue = (source, operation) => {
    if (disposed || (owner.pending ?? 0) >= 16) { disconnect(source, "capacity"); return false; }
    owner.pending = (owner.pending ?? 0) + 1;
    owner.tail = (owner.tail ?? Promise.resolve()).then(operation)
      .catch(() => { disconnect(source); }).finally(() => { owner.pending--; });
    return true;
  };
  const associated = source => !disposed && current === source
    && selection(api)?.session === source.session;
  const stillSelected = source => !source.cancelled && associated(source);
  const retire = source => queue(source, async () => {
    // Cleanup uses only the already-issued handle; no new enrollment is created
    // to reconstruct a lost reply. Observer loss otherwise follows lease expiry.
    if (source.client?.status().phase === "enrolled") await source.client.end();
    source.client?.detach();
  });
  const refresh = () => {
    const chosen = selection(api);
    if (current && current.session === chosen?.session && !current.cancelled
        && current.client?.status().phase === "connection-lost") disconnect(current);
    if (current && current.session === chosen?.session && current.lossReason === "transport"
        && (current.retries ?? 0) < 4 && now() >= current.retryAfter && !current.recovering) {
      const source = current;
      source.recovering = true;
      source.retries = (source.retries ?? 0) + 1;
      queue(source, async () => {
        const started = await source.client?.start();
        source.recovering = false;
        if (disposed || current !== source || selection(api)?.session !== source.session) {
          source.client?.disconnect(); return;
        }
        if (!started) { disconnect(source); return; }
        source.cancelled = false;
        source.lossReason = undefined;
        // Reuse exact wait identities; sample only actual current cache state,
        // never manufacture idle or re-read message/transcript history.
        const latest = selection(api);
        if (latest?.status) {
          source.knownBase = true;
          await source.observer.event({ event: { type: "session.status", properties: {
            sessionID: source.session, status: { type: latest.status } } } });
        }
      });
      return source;
    }
    if (current?.session === chosen?.session
        && !(current?.lossReason === "unknown-base" && chosen?.status)) return current;
    const previous = current;
    if (previous) {
      previous.cancelled = true;
      retire(previous);
    }
    current = undefined;
    if (!chosen || disposed) return;
    const source = { session: chosen.session, instance: randomUUID(), cancelled: false,
      knownBase: chosen.status !== undefined };
    current = source;
    source.observer = createOpenCodeObserver(source.session, fact => {
      if (!associated(source)) return false;
      if (fact.kind === "usage") return true; // no unsafe ordinary expense admission
      if (fact.kind === "unavailable") { disconnect(source, "capacity"); return false; }
      if (fact.kind !== "status") return true;
      if (fact.state === "retire") { source.cancelled = true; retire(source); return true; }
      if (source.cancelled) {
        // Keep observed reply-before-ask/tombstone history locally during a
        // Mezzanine transport outage; no publication or expense is attempted.
        return source.lossReason === "transport";
      }
      if (!source.knownBase && !["approval-wait", "input-wait"].includes(fact.state)) {
        source.lossReason = "unknown-base";
        source.cancelled = true;
        retire(source);
        return true;
      }
      source.nextStatus = fact.state;
      if (source.statusQueued) return true;
      source.statusQueued = true;
      return queue(source, async () => {
        source.statusQueued = false;
        const state = source.nextStatus;
        source.nextStatus = undefined;
        if (!stillSelected(source) || !source.client) return;
        // SDK presentation can auto-start; detect loss before invoking it so
        // quiet closures and event-driven closures share the same retry budget.
        if (source.client.status().phase !== "enrolled") { disconnect(source); return; }
        const delivered = await source.client.presentation(state, true);
        if (!delivered?.delivered) disconnect(source);
      });
    });
    queue(source, async () => {
      if (!stillSelected(source)) return;
      const peerHelper = options.peerHelper ?? (await import("./peer_helper.mjs")).peerHelper;
      if (!stillSelected(source)) return;
      source.client = (options.createClient ?? createPersistentTelemetryClient)({ harness: "opencode",
        version: "best-effort", session: source.session, instance: source.instance,
        displayName: "OpenCode", peerHelper, env });
      const started = await source.client.start();
      if (!stillSelected(source)) {
        if (started) await source.client.end();
        source.client.detach();
        return;
      }
      if (!started) { disconnect(source); return; }
    });
    // Queue the actual cache baseline before live facts, not after asynchronous
    // enrollment (which could otherwise overwrite a newer idle/running event).
    if (chosen.status) void source.observer.event({ event: { type: "session.status",
      properties: { sessionID: source.session, status: { type: chosen.status } } } });
    for (const wait of chosen.waits) void source.observer.event({ event: {
      type: `${wait.kind}.asked`, properties: { sessionID: source.session, id: wait.request } } });
    return source;
  };
  for (const type of eventTypes) {
    try {
      const unsubscribe = api.event.on(type, event => {
        try {
          if (disposed) return;
          const source = refresh();
          if (!source || !associated(source)
              || (source.cancelled && source.lossReason !== "transport")) return;
          // Metadata/global creation events only wake selection sampling. They
          // cannot fabricate idle state for an already selected active session.
          if (type === "session.created" || type === "session.updated") return;
          if (event?.properties?.sessionID === source.session
              && (["session.idle", "session.error"].includes(type)
                || (type === "session.status" && ["busy", "retry", "idle"].includes(event.properties.status?.type)))) {
            source.knownBase = true;
          }
          // The existing projector allocates only allowlisted facts. Its input
          // object is consumed synchronously and never queued/stored wholesale.
          void source.observer.event({ event }).catch(() => { disconnect(source); });
        } catch { /* optional observational callback cannot affect the TUI */ }
      });
      if (typeof unsubscribe === "function") unsubscribers.push(unsubscribe);
    } catch { disposed = true; break; }
  }
  let timer;
  const dispose = () => {
    if (cleaned) return;
    cleaned = true;
    disposed = true;
    api.lifecycle.signal.removeEventListener("abort", dispose);
    clearInterval(timer);
    timer = undefined;
    for (const unsubscribe of unsubscribers.splice(0)) { try { unsubscribe(); } catch {} }
    if (current) current.cancelled = true;
    const source = current;
    if (owner.lease === lease) owner.lease = undefined;
    // One finalizer is serialized with this process's accepted work and any
    // successor scope. Do not return a network promise to the vendor disposer.
    owner.tail = (owner.tail ?? Promise.resolve()).then(async () => {
      if (source?.client?.status().phase === "enrolled") await source.client.end();
      source?.client?.detach();
    }).catch(() => { source?.client?.detach(); });
  };
  try {
    api.lifecycle.onDispose(dispose);
    api.lifecycle.signal.addEventListener("abort", dispose, { once: true });
    if (!disposed && !api.lifecycle.signal.aborted) {
      // A scoped unreferenced cache sampler detects route changes even during
      // silent sessions; it never polls a server or keeps the vendor alive.
      timer = setInterval(() => { try { refresh(); } catch {} }, 250);
      timer.unref();
      refresh();
    } else dispose();
  } catch { dispose(); }
  return Object.freeze({ dispose, async settled() {
    let observed;
    do { observed = owner.tail; if (!observed) return; await observed; } while (observed !== owner.tail);
  } });
}

/** The inspected loader requires one default {id,tui}, not server/tui mixing. */
export default Object.freeze({ id: "mezzanine-observer", tui(api) {
  registerOpenCodeTuiObserver(api);
} });
