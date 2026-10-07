/** Content-free v2 session proposals over one inherited observation channel.
 * A process owner supplies exact instance leases/epochs. Events propose session
 * identity only: the Rust parent must fence ordering and separately authorize
 * every new/resume/fork binding. No daemon token, vendor files or mutation API.
 */
import { createPiObserver } from "./pi_observer.mjs";
import { createObserverStreamSink } from "./pi_observer_stream.mjs";

const valid = (value) => typeof value === "string" && /^[A-Za-z0-9_.:-]{1,128}$/.test(value);

/** Register callbacks without opening a resource until a context-bound start. */
export function createPiBindingExtension(initialSession, openChannel) {
  if (!valid(initialSession) || typeof openChannel !== "function") throw new Error("Pi binding unavailable");
  return (pi) => {
    let channel;
    let sink;
    let session;
    let expected = "startup";
    let retired = false;
    let epoch;
    let projected;
    const close = (reason) => {
      sink?.dispose(true);
      sink = undefined;
      const previous = channel;
      channel = undefined;
      try { previous?.close(reason); } catch { /* neutral telemetry loss */ }
    };
    pi.on("session_start", (event, ctx) => {
      try {
        if (retired || channel) return;
        const observed = ctx.sessionManager.getSessionId();
        if (!valid(observed) || !["startup", "reload", "new", "resume", "fork"].includes(event.reason)) return;
        if (event.reason === "startup" && observed !== initialSession) return;
        if (expected !== event.reason && expected !== "startup") return;
        channel = openChannel(event.reason);
        epoch = channel.epoch;
        if (!Number.isSafeInteger(epoch) || epoch < 1) throw new Error("Pi epoch unavailable");
        session = observed;
        // The generic sink still reprojects exact allowlisted event fields. Only
        // its final write receives the separately bounded session/epoch envelope.
        const stream = channel.stream;
        const wire = {
          get writableLength() { return stream.writableLength; },
          on: (...args) => stream.on(...args), off: (...args) => stream.off(...args),
          write: (frame) => {
            const wrapped = JSON.stringify({ session, epoch, event: JSON.parse(frame) }) + "\n";
            if (Buffer.byteLength(wrapped) > 2049 || stream.writableLength + Buffer.byteLength(wrapped) > 32768) return false;
            return stream.write(wrapped);
          },
        };
        sink = createObserverStreamSink(wire, session, () => channel?.fail?.());
        projected = new Map();
        createPiObserver(session, (item) => sink?.enqueue(item))({ on(kind, fn) { projected.set(kind, fn); } });
        sink.enqueue({ session, event: { type: "session_start", reason: event.reason } });
        expected = undefined;
      } catch { close("unavailable"); retired = true; }
    });
    // Reuse the existing projector without retaining vendor event payloads.
    const types = ["agent_start", "ui_prompt_start", "ui_prompt_end", "agent_before_settle", "agent_settled", "session_shutdown"];
    for (const type of types) {
      pi.on(type, (event, ctx) => {
        try {
          if (!channel || ctx.sessionManager.getSessionId() !== session) return;
          projected.get(type)?.(event, ctx);
          if (type === "session_shutdown") {
            if (!["quit", "reload", "new", "resume", "fork"].includes(event.reason)) return;
            expected = event.reason;
            close(event.reason);
            if (event.reason === "quit" || event.reason === "reload") retired = true;
          }
        } catch { close("unavailable"); retired = true; }
      });
    }
  };
}
