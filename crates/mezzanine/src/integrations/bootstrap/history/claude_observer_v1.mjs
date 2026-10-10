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
