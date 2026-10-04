/**
 * Transport-free Pi 1.0.2 lifecycle observer groundwork.
 *
 * The caller supplies an already-authorized session and a synchronous bounded
 * enqueue function. This module never acquires credentials, starts resources,
 * sends RPCs, reads files, or calls Pi mutation APIs. A future launcher owns
 * delivery, sequencing, renewal and reload lifetime; this is not an installable
 * certified adapter. Callback inputs are not serialized or forwarded wholesale.
 */

const validSession = (value) => typeof value === "string"
  && value.length > 0 && value.length <= 128 && /^[A-Za-z0-9_.:-]+$/.test(value);
const startReasons = new Set(["startup", "reload", "new", "resume", "fork"]);
const stopReasons = new Set(["quit", "reload", "new", "resume", "fork"]);
const promptKinds = new Set(["select", "confirm", "input", "editor", "custom"]);
const outcomes = new Set(["completed", "aborted", "error"]);

/** Create a registration-only factory with no I/O or timers during loading. */
export function createPiObserver(boundSession, enqueue) {
  if (!validSession(boundSession) || typeof enqueue !== "function") {
    throw new Error("Pi observer binding unavailable");
  }
  return (pi) => {
    const observe = (type, project) => {
      pi.on(type, (event, ctx) => {
        // Context access deliberately remains inside every callback: the
        // released runner rejects old context after replacement/invalidation.
        try {
          if (ctx.sessionManager.getSessionId() !== boundSession) return;
          const data = project(event);
          if (data === undefined) return;
          // Only newly allocated inert facts reach the injected queue. Returning
          // undefined preserves observational boundary/UI callback semantics.
          enqueue(Object.freeze({ session: boundSession, event: Object.freeze(data) }));
        } catch {
          // Missing/stale context, queue pressure and telemetry loss must not
          // alter a vendor decision, emit content or synthesize continuation.
        }
      });
    };
    observe("session_start", (e) => startReasons.has(e.reason)
      ? { type: "session_start", reason: e.reason } : undefined);
    observe("agent_start", () => ({ type: "agent_start" }));
    for (const type of ["ui_prompt_start", "ui_prompt_end"]) {
      observe(type, (e) => e.reason === "ui_prompt" && promptKinds.has(e.kind)
        ? { type, reason: "ui_prompt", kind: e.kind } : undefined);
    }
    observe("agent_before_settle", (e) => outcomes.has(e.outcome)
      ? { type: "agent_before_settle", outcome: e.outcome } : undefined);
    observe("agent_settled", () => ({ type: "agent_settled" }));
    observe("session_shutdown", (e) => stopReasons.has(e.reason)
      ? { type: "session_shutdown", reason: e.reason } : undefined);
  };
}
