/** Ordinary Pi callback wiring. Registration/loading does no I/O; each genuine
 * start acquires one process lease, then uses the native-qualified shared client.
 * Existing createPiObserver projects content-free facts; daemon LifecycleOwner
 * owns provisional/final status and UI restoration. No vendor mutation/launch,
 * transcript access, forced session selectors or credential environment. */
import { randomUUID } from "node:crypto";
import { createPiObserver } from "./pi_observer.mjs";
import { createPersistentTelemetryClient, discoverPersistentRoute } from "./persistent_client.mjs";

const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
const sessionId = value => typeof value === "string" && /^[A-Za-z0-9_.:-]{1,128}$/.test(value);
const starts = new Set(["startup", "reload", "new", "resume", "fork"]);
const stops = new Set(["quit", "reload", "new", "resume", "fork"]);

/** Injectable resource-free registration boundary for offline event fixtures.
 * Only options selected by compiled adapter/test code reach the client factory. */
export function registerOrdinaryPiObserver(pi, options = {}) {
  const env = options.env ?? process.env;
  if (!discoverPersistentRoute(env)) return;
  let state = options.owner;
  let lease;
  let projected;
  let retired = false;
  const owner = () => state ??= globalThis[ownerKey] ??= {};
  const disconnect = client => {
    if (typeof client?.disconnect === "function") client.disconnect();
    else client?.detach();
  };
  const cancel = owned => {
    owned.failed = true;
    owned.cancelled = true;
    // Close only this process-owned telemetry transport, retaining exact
    // instance/predecessor metadata for a later genuine reload recovery.
    disconnect(owned.client ?? owner().client);
  };
  const queue = (owned, operation) => {
    const state = owner();
    const pending = state.pending ?? 0;
    if (pending >= 32) { cancel(owned); return false; }
    state.pending = pending + 1;
    state.tail = (state.tail ?? Promise.resolve()).then(operation)
      .catch(() => { cancel(owned); })
      .finally(() => { state.pending--; });
    return true;
  };
  const forward = item => {
    const owned = lease;
    if (!owned || owner().lease !== owned || owned.failed || item.session !== owned.session) return false;
    return queue(owned, async () => {
      if (owned.failed || !owned.client) return;
      const result = await owned.client.piObservation(item.event);
      if (!result?.delivered) owned.failed = true;
    });
  };
  pi.on("session_start", (event, ctx) => {
    try {
      const reason = event.reason;
      if (retired || lease || !starts.has(reason)) return;
      const session = ctx.sessionManager.getSessionId();
      if (!sessionId(session)) return;
      const state = owner();
      if (state.lease || state.closed || (state.transition && state.transition !== reason)) return;
      const owned = { session, instance: randomUUID(), failed: false, cancelled: false };
      state.lease = owned;
      state.transition = undefined;
      lease = owned;
      projected = new Map();
      createPiObserver(session, forward)({ on(type, callback) { projected.set(type, callback); } });
      queue(owned, async () => {
        if (owned.cancelled) return;
        let previous = state.client;
        let client;
        if (reason === "reload" && previous && state.session === session) {
          // Recover a lost enrollment reply under the exact old instance before
          // requesting a new epoch; never guess a predecessor or reuse old IDs.
          const recovered = await previous.start();
          if (owned.cancelled) { disconnect(previous); return; }
          if (!recovered) { owned.failed = true; return; }
          client = previous.successor(owned.instance);
        } else if (reason === "reload" && previous) {
          owned.failed = true;
          return;
        } else {
          const peerHelper = options.peerHelper ?? (await import("./peer_helper.mjs")).peerHelper;
          if (owned.cancelled) return;
          client = (options.createClient ?? createPersistentTelemetryClient)({ harness: "pi",
            session, instance: owned.instance, version: "best-effort", displayName: "Pi",
            peerHelper, env });
        }
        if (owned.cancelled) { disconnect(client); return; }
        if (!client) { owned.failed = true; return; }
        // Keep the exact attempted instance/predecessor even after reply loss.
        // A later genuine reload can recover that same attempt rather than
        // guessing from an old handle which may already have been replaced.
        owned.client = client;
        state.client = client;
        state.session = session;
        if (previous && previous !== client) previous.detach();
        const started = await client.start();
        if (owned.cancelled) { disconnect(client); return; }
        if (!started) { owned.failed = true; return; }
        const result = await client.piObservation({ type: "session_start", reason });
        if (!result?.delivered) owned.failed = true;
      });
    } catch { /* invalid/stale context does not alter Pi */ }
  });
  for (const type of ["agent_start", "ui_prompt_start", "ui_prompt_end", "agent_before_settle", "agent_settled", "session_shutdown"]) {
    pi.on(type, (event, ctx) => {
      try {
        const owned = lease;
        if (!owned || owner().lease !== owned || ctx.sessionManager.getSessionId() !== owned.session) return;
        if (type === "session_shutdown" && !stops.has(event.reason)) return;
        projected.get(type)?.(event, ctx);
        if (type === "session_shutdown") {
          const reason = event.reason;
          queue(owned, async () => {
            // Keep reload predecessor ownership until its successor is admitted.
            // Other shutdown facts retire this exact session in the daemon.
            if (reason !== "reload") owned.client?.detach();
          });
          owner().lease = undefined;
          owner().transition = reason;
          if (reason === "quit") owner().closed = true;
          lease = undefined;
          projected = undefined;
          if (reason === "quit" || reason === "reload") retired = true;
        }
      } catch { /* all callbacks remain synchronous, observational and neutral */ }
    });
  }
}
