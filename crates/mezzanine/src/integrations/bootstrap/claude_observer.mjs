/** Transport-free Claude classic-event middleware contract for curated mods.
 * Docs: plugins/mods/{events,reference}, public claude-code.d.ts. No Node/DOM
 * imports, vendor APIs, timers, files, network, transcript/content or credentials.
 * A code-owned producer supplies an immutable session selector and a bounded
 * nonblocking enqueue function. These facts grant no enrollment authority.
 * Mod-load session.start is deliberately not a conversation-start callback.
 * This library is not an installed mod, Unix transport, accounting producer or
 * proof of vendor-loader support; unsandboxed mod enablement remains separate. */

const identifier = value => typeof value === "string"
  && /^[A-Za-z0-9_.:-]{1,128}$/.test(value);
const starts = new Set(["startup", "resume", "clear", "compact", "fork"]);
const ends = new Set(["clear", "resume", "logout", "prompt_input_exit", "other"]);

const projectors = Object.freeze({
  SessionStart: event => {
    const reason = event.source;
    return starts.has(reason) ? { type: "session_start", reason } : undefined;
  },
  UserPromptSubmit: () => ({ type: "prompt_submit" }),
  Notification: event => event.notification_type === "permission_prompt"
    ? { type: "permission_wait" } : undefined,
  Stop: event => {
    const active = event.stop_hook_active;
    // Stop may ask for continuation. This is a boundary, not final completion.
    return typeof active === "boolean" ? { type: "stop", active } : undefined;
  },
  StopFailure: () => ({ type: "stop_failure" }),
  SessionEnd: event => {
    const reason = event.reason;
    return ends.has(reason) ? { type: "session_end", reason } : undefined;
  },
});

/** Captures only fresh frozen facts, without retaining input or middleware/API
 * handles. Real curated mods must keep literal on/next calls at their call sites
 * and publish captured facts only after downstream resolves unchanged. Unknown
 * or prototype event names, child/wrong-session and malformed inputs are inert. */
export function projectClaudeEvent(boundSession, name, event) {
  try {
    if (!identifier(boundSession) || typeof name !== "string" || !Object.hasOwn(projectors, name)
        || !event || typeof event !== "object" || Array.isArray(event)) return;
    const session = event.session_id;
    const declared = event.hook_event_name;
    const child = event.agent_id;
    const loop = event.agentId;
    if (session !== boundSession || declared !== name || child != null || loop != null) return;
    const data = projectors[name](event);
    if (data) return Object.freeze({ session: boundSession, event: Object.freeze(data) });
  } catch { /* malformed/stale metadata is unavailable, never forwarded */ }
}

/** Creates a generic middleware adapter for code-owned host integrations.
 * Each callback captures only known inert fields, calls next(original) exactly
 * once, preserves its result/error, and publishes only after downstream success.
 * Enqueue throws/rejected promises are telemetry loss, never a vendor decision.
 * Caller must create a fresh session binding rather than rebind these closures.
 * Curated mod entrypoints use projectClaudeEvent inside literal wrappers instead
 * of passing their privileged $/next handles into factory-produced callbacks. */
export function createClaudeObserver(boundSession, enqueue) {
  if (!identifier(boundSession) || typeof enqueue !== "function") {
    throw new Error("Claude observer binding unavailable");
  }
  const callback = name => async (_api, event, next) => {
    const fact = projectClaudeEvent(boundSession, name, event);
    const result = await next(event);
    if (fact) {
      try { Promise.resolve(enqueue(fact)).catch(() => {}); }
      catch { /* bounded producer enqueue failure cannot change middleware */ }
    }
    return result;
  };
  return Object.freeze({
    sessionStart: callback("SessionStart"),
    promptSubmit: callback("UserPromptSubmit"),
    notification: callback("Notification"),
    stop: callback("Stop"),
    stopFailure: callback("StopFailure"),
    sessionEnd: callback("SessionEnd"),
  });
}

/** Bind a bounded presentation-only sender to one actual public observer.
 * The fixed-helper runner is caller-owned; no raw callback, credential, lease,
 * accounting operation or newest-identity lookup is exposed. Neutral helper
 * output is not delivery evidence, and observed owner loss is permanent. */
export function createClaudeLifecycleSender({ observer, isCurrent, send } = {}) {
  const unavailable = () => Object.freeze({ submitted: false, delivered: false });
  let original;
  try {
    const fields = ["external_session_id", "generation", "observer_witness", "run_id", "observer_epoch", "agent_id"];
    if (observer && typeof observer === "object" && !Array.isArray(observer)
        && Object.keys(observer).length === fields.length && Object.keys(observer).every(key => fields.includes(key))) {
      const snapshot = { external_session_id: observer.external_session_id, generation: observer.generation,
        observer_witness: observer.observer_witness, run_id: observer.run_id,
        observer_epoch: observer.observer_epoch, agent_id: observer.agent_id };
      const positive = value => Number.isSafeInteger(value) && value > 0;
      if (identifier(snapshot.external_session_id) && identifier(snapshot.agent_id)
          && positive(snapshot.generation) && positive(snapshot.run_id) && positive(snapshot.observer_epoch)
          && typeof snapshot.observer_witness === "string" && /^[a-f0-9]{64}$/.test(snapshot.observer_witness)) original = Object.freeze(snapshot);
    }
  } catch { /* malformed selector getters cannot publish an observer */ }
  let stopped = !original || typeof isCurrent !== "function" || typeof send !== "function";
  let sequence = 0, latest = 0, serial = Promise.resolve();
  const tickets = new Map();
  const current = () => {
    if (stopped) return false;
    try { if (isCurrent() === true) return true; } catch { /* unavailable owner */ }
    stopped = true;
    tickets.clear();
    return false;
  };
  const states = Object.freeze({ session_start: "ready", prompt_submit: "running",
    permission_wait: "approval-wait", stop: "ready", stop_failure: "failed", session_end: "complete" });
  return Object.freeze({
    capture(fact) {
      let state;
      try {
        const session = fact?.session, type = fact?.event?.type;
        if (!original || session !== original.external_session_id || !Object.hasOwn(states, type)) return;
        state = states[type];
      } catch { return; }
      if (!current() || tickets.size >= 16 || !Number.isSafeInteger(sequence + 1)) return;
      const ticket = Object.freeze({ sequence: ++sequence });
      tickets.set(ticket, { state, publishing: false });
      return ticket;
    },
    publish(ticket) {
      const captured = tickets.get(ticket);
      if (!captured || captured.publishing) return Promise.resolve(unavailable());
      captured.publishing = true;
      const result = serial.then(async () => {
        if (!current() || !tickets.has(ticket) || ticket.sequence <= latest) return unavailable();
        latest = ticket.sequence;
        const envelope = Object.freeze({ operation: "helper-observe", harness: "claude",
          generation: original.generation, observer_witness: original.observer_witness,
          external_session_id: original.external_session_id,
          data: Object.freeze({ sequence: ticket.sequence, state: captured.state, title: "Claude" }) });
        const response = await send(envelope);
        return current() && response?.exitCode === 0 && response.stdout === "{}\n"
          ? Object.freeze({ submitted: true, delivered: false }) : unavailable();
      }).catch(unavailable);
      serial = result.then(() => { tickets.delete(ticket); }, () => { tickets.delete(ticket); });
      return result;
    },
    observe(fact) { return this.publish(this.capture(fact)); },
    abandon(ticket) { if (!tickets.get(ticket)?.publishing) tickets.delete(ticket); },
    stop() { stopped = true; tickets.clear(); },
  });
}
