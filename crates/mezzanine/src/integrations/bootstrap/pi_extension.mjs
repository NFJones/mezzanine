/**
 * Session-scoped Pi 1.0.2 extension wiring for a private observer channel.
 *
 * A launcher supplies an immutable authorized session and a synchronous opener
 * for its own inherited observer stream. Neither function receives a daemon
 * capability. Factory loading registers callbacks only; session_start opens
 * resources and session_shutdown closes only this observer's channel. Reload
 * requires a newly supplied opener/epoch outside this extension instance.
 * This injectable factory is not a production-certified installation artifact.
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
    const close = () => {
      sink?.dispose();
      const previous = channel;
      channel = undefined;
      sink = undefined;
      try { previous?.close(); } catch { /* telemetry-only cleanup */ }
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
        sink = createObserverStreamSink(opened.stream, boundSession);
        const ownedSink = sink;
        opened.stream.on("close", () => ownedSink.releaseAfterClose());
      } catch { close(); }
    });
    createPiObserver(boundSession, (item) => sink?.enqueue(item))(pi);
    // Registered after forwarding the teardown fact. Cleanup never changes
    // session decisions, injects continuation, or retires daemon authority.
    pi.on("session_shutdown", () => { close(); });
  };
}
