/** Bounded metadata-only child-to-original-parent association for Claude mods.
 * A successful classic SubagentStart supplies session/child identity absent
 * from turn.step. No SDK API, lifecycle capability, native authority, content,
 * transcript, path, timer, credential or transport enters this registry.
 * At most128 child IDs retain their original session, including failed starts;
 * no eviction can make an old child eligible for another parent session. */
const identifier = value => typeof value === "string" && /^[A-Za-z0-9._:-]{1,128}$/.test(value);

/** Public observer objects are opaque epoch identity, not structural authority.
 * The caller must separately fence current native-parent ownership around its
 * unchanged downstream call. Latest exact attempt wins; copies cannot commit. */
export function createClaudeChildScopes() {
  const children = new Map();
  const attempts = new WeakMap();
  const observerIdentity = value => value !== null && typeof value === "object";
  return Object.freeze({
    prepare(session, event, observer) {
      try {
        if (!identifier(session) || !observerIdentity(observer) || !event || typeof event !== "object" || Array.isArray(event)) return;
        const type = event.hook_event_name, observed = event.session_id, agentId = event.agent_id;
        if (type !== "SubagentStart" || observed !== session || !identifier(agentId)) return;
        let entry = children.get(agentId);
        const reason = entry && entry.session !== session ? "request-readiness"
          : !entry && children.size >= 128 ? "capacity" : undefined;
        const ticket = Object.freeze({});
        if (!reason) {
          if (!entry) { entry = { session }; children.set(agentId, entry); }
          entry.pending = ticket;
        }
        attempts.set(ticket, { entry, observer, reason });
        return ticket;
      } catch { /* malformed metadata never changes vendor behavior */ }
    },
    finish(ticket) {
      const attempt = attempts.get(ticket);
      if (!attempt) return;
      attempts.delete(ticket);
      if (attempt.reason) return attempt.reason;
      if (attempt.entry.pending === ticket) {
        attempt.entry.pending = undefined;
        attempt.entry.observer = attempt.observer;
      }
    },
    cancel(ticket) {
      const attempt = attempts.get(ticket);
      attempts.delete(ticket);
      if (attempt?.entry?.pending === ticket) attempt.entry.pending = undefined;
    },
    current(session, agentId, observer) {
      if (!observerIdentity(observer)) return false;
      const entry = children.get(agentId);
      return entry !== undefined && entry.session === session && entry.observer === observer;
    },
  });
}
