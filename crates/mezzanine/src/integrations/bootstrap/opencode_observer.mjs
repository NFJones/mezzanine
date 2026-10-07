/** Transport-free OpenCode plugin event observation for an explicitly bound root.
 * Only inert lifecycle facts and one completed assistant-message usage source
 * reach a supplied bounded queue. No client API, transcript, permission mutation,
 * continuation, credential or process is accessed. Unavailable counters remain
 * unavailable; this producer does not claim acknowledgment or installation.
 */
const id = (value) => typeof value === "string" && /^[A-Za-z0-9_.:-]{1,128}$/.test(value);
const text = (value) => typeof value === "string" && value.length > 0 && value.length <= 128
  && !/[\p{Cc}\u202a-\u202e\u2066-\u2069]/u.test(value);
const count = (value) => Number.isSafeInteger(value) && value >= 0;
const sum = (...values) => values.every(count) && Number.isSafeInteger(values.reduce((total, value) => total + value, 0));
const statuses = new Map([["busy", "running"], ["retry", "running"], ["idle", "ready"]]);
const activities = new Map([["session.idle", "ready"], ["session.error", "failed"]]);
const asks = new Map([["permission.asked", "permission"], ["question.asked", "question"]]);
const replies = new Map([["permission.replied", "permission"], ["question.replied", "question"], ["question.rejected", "question"]]);

/** Registration-only observer; its enqueue contract is synchronous and bounded. */
export function createOpenCodeObserver(boundSession, enqueue) {
  if (!id(boundSession) || typeof enqueue !== "function") throw new Error("OpenCode observer binding unavailable");
  let baseState = "ready";
  let available = true;
  let created = false;
  // Pending and completed IDs share one finite history: replies before asks and
  // repeated asks cannot reopen a resolved wait. Exhaustion reports loss rather
  // than evicting identities and guessing that unknown requests have settled.
  const waits = new Map();
  const waitState = () => {
    if ([...waits.values()].some(wait => wait.pending && wait.kind === "permission")) return "approval-wait";
    if ([...waits.values()].some(wait => wait.pending && wait.kind === "question")) return "input-wait";
    return baseState;
  };
  const offer = (value) => {
    try { if (enqueue(Object.freeze({ session: boundSession, ...value })) === false) available = false; }
    catch { available = false; /* telemetry loss never changes vendor behavior */ }
  };
  const remember = (key, kind, pending) => {
    if (!waits.has(key) && waits.size === 256) {
      offer({ kind: "unavailable", reason: "wait-capacity" });
      available = false;
      return false;
    }
    waits.set(key, { kind, pending });
    return true;
  };
  return {
    async event(input) {
      try {
        if (!available) return;
        const event = input?.event;
        const properties = event?.properties;
        const info = properties?.info;
        if (["session.created", "session.updated", "session.deleted"].includes(event?.type)) {
          if (info?.id !== boundSession || info.parentID != null) return;
          // Updated metadata (including title) is not proof that activity ended.
          if (event.type === "session.updated") return;
          if (event.type === "session.created" && created) return;
          created = true;
          offer({ kind: "status", state: event.type === "session.deleted" ? "retire" : waitState() });
          if (event.type === "session.deleted") available = false;
          return;
        }
        if (event?.type === "message.updated") {
          if (info?.sessionID !== boundSession || info.role !== "assistant" || !id(info.id)
              || !text(info.providerID) || !text(info.modelID) || !count(info.time?.completed)) return;
          const tokens = info.tokens;
          if (!tokens || !sum(tokens.input, tokens.cache?.read, tokens.cache?.write)
              || !sum(tokens.output, tokens.reasoning)) return;
          // Exact known upstream categories stay disjoint here. The existing
          // Rust projection restores inclusive totals once and owns ledger identity.
          const message = Object.freeze({ role: "assistant", sessionID: boundSession, id: info.id,
            providerID: info.providerID, modelID: info.modelID,
            time: Object.freeze({ completed: info.time.completed }),
            tokens: Object.freeze({ input: tokens.input, output: tokens.output, reasoning: tokens.reasoning,
              cache: Object.freeze({ read: tokens.cache.read, write: tokens.cache.write }) }) });
          offer({ kind: "usage", message });
          return;
        }
        if (properties?.sessionID !== boundSession) return;
        const kind = asks.get(event.type) ?? replies.get(event.type);
        if (kind) {
          const requested = asks.has(event.type);
          const request = requested ? properties.id : properties.requestID ?? properties.permissionID;
          if (!id(request)) return;
          if (!requested && properties.requestID !== undefined && properties.permissionID !== undefined
              && properties.requestID !== properties.permissionID) return;
          const key = `${kind}:${request}`;
          const prior = waits.get(key);
          if (requested) {
            if (prior || !remember(key, kind, true)) return;
          } else {
            if (!remember(key, kind, false) || !prior?.pending) return;
          }
          offer({ kind: "status", state: waitState() });
          return;
        }
        const state = event.type === "session.status" ? statuses.get(properties.status?.type) : activities.get(event.type);
        if (state) {
          baseState = state;
          offer({ kind: "status", state: waitState() });
        }
      } catch { /* untrusted getters/queues cannot change callback result */ }
    },
  };
}
