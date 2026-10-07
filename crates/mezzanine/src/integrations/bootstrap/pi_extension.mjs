/**
 * Session-scoped Pi 1.0.2 extension wiring for a private observer channel.
 *
 * A launcher supplies an immutable authorized session and a synchronous opener
 * for its own inherited observer stream. Neither function receives a daemon
 * capability. Factory loading registers callbacks only; session_start opens
 * resources and session_shutdown releases only this instance's channel lease.
 * A process-owned persistent channel survives same-session reload; the parent
 * separately confirms its replacement epoch. No vendor mutation is performed.
 */
import { createPiObserver } from "./pi_observer.mjs";
import { createObserverStreamSink } from "./pi_observer_stream.mjs";

/** Construct registration-only wiring; do not open resources during loading. */
export function createPiStreamExtension(boundSession, openChannel) {
  if (typeof boundSession !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(boundSession)
      || typeof openChannel !== "function") {
    throw new Error("Pi extension binding unavailable");
  }
  return (pi) => {
    let channel;
    let sink;
    let attempted = false;
    const close = (reason = "unavailable") => {
      sink?.dispose(channel?.persistent === true);
      const previous = channel;
      channel = undefined;
      sink = undefined;
      try { previous?.close(reason); } catch { /* telemetry-only cleanup */ }
    };
    // Registered before the observer so the initial session fact can be sent.
    pi.on("session_start", (event, ctx) => {
      try {
        if (ctx.sessionManager.getSessionId() !== boundSession
            || !["startup", "reload", "new", "resume", "fork"].includes(event.reason)) {
          close();
          return;
        }
        if (attempted) return; // duplicate load/start cannot reopen authority
        attempted = true;
        const opened = openChannel();
        if (!opened || typeof opened.close !== "function") {
          throw new Error("Pi observer channel unavailable");
        }
        channel = opened;
        sink = createObserverStreamSink(opened.stream, boundSession, () => opened.fail?.());
        const ownedSink = sink;
        if (!opened.persistent) opened.stream.once("close", () => ownedSink.releaseAfterClose());
      } catch { close(); }
    });
    createPiObserver(boundSession, (item) => sink?.enqueue(item))(pi);
    // Registered after forwarding the teardown fact. Cleanup never changes
    // session decisions, injects continuation, or retires daemon authority.
    pi.on("session_shutdown", (event) => { close(event.reason); });
  };
}
