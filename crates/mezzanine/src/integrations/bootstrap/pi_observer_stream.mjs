/**
 * Bounded writer for a launcher-supplied inherited observer stream.
 * No connection discovery, daemon token, timer, path or vendor mutation API.
 * Construct at session start, not factory loading; dispose on observer teardown.
 * A failed/full stream loses telemetry and is never automatically reconnected.
 */
export function createObserverStreamSink(stream, boundSession, onFailure = () => {}) {
  if (!stream || typeof stream.write !== "function" || typeof stream.on !== "function"
      || typeof stream.off !== "function" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(boundSession)) {
    throw new Error("Pi observer stream unavailable");
  }
  let active = true;
  let closed = false;
  const fail = () => {
    if (!active) return;
    active = false;
    try { onFailure(); } catch { /* observational failure stays neutral */ }
  };
  const close = () => { active = false; closed = true; };
  stream.on("error", fail);
  stream.on("close", close);
  const enqueue = (item) => {
    if (!active || item?.session !== boundSession) return false;
    try {
      // The caller must use the observer's newly allocated allowlisted facts.
      // Reproject them here too: arbitrary objects are never serialized wholesale.
      const e = item.event;
      let data;
      switch (e?.type) {
        case "agent_start": case "agent_settled": data = { type: e.type }; break;
        case "session_start":
          if (!["startup", "reload", "new", "resume", "fork"].includes(e.reason)) return false;
          data = { type: e.type, reason: e.reason }; break;
        case "session_shutdown":
          if (!["quit", "reload", "new", "resume", "fork"].includes(e.reason)) return false;
          data = { type: e.type, reason: e.reason }; break;
        case "ui_prompt_start": case "ui_prompt_end":
          if (e.reason !== "ui_prompt" || !["select", "confirm", "input", "editor", "custom"].includes(e.kind)) return false;
          data = { type: e.type, reason: "ui_prompt", kind: e.kind }; break;
        case "agent_before_settle":
          if (!["completed", "aborted", "error"].includes(e.outcome)) return false;
          data = { type: e.type, outcome: e.outcome }; break;
        default: return false;
      }
      const frame = JSON.stringify(data) + "\n";
      const length = Buffer.byteLength(frame);
      if (length > 1025 || !Number.isSafeInteger(stream.writableLength)
          || stream.writableLength < 0 || stream.writableLength + length > 32768) { fail(); return false; }
      if (!stream.write(frame)) fail();
      return true;
    } catch { fail(); return false; }
  };
  return {
    enqueue,
    // Keep the neutral error listener through final stream teardown: removing
    // it while an inherited write is outstanding can cause an uncaught error.
    dispose(detach = false) {
      active = false;
      // A process-owned channel retains its own permanent neutral listener.
      // Instance reload may therefore remove only this sink's listeners.
      if (detach) { stream.off("error", fail); stream.off("close", close); }
    },
    releaseAfterClose() {
      active = false;
      if (closed) { stream.off("error", fail); stream.off("close", close); }
    },
  };
}
