/** Fixed metadata-only Claude coverage facts, separate from expense/lifecycle.
 * One original native observer owns at most four pending fixed reasons. Repeated
 * current facts coalesce only while queued/in flight; no timer, provider retry,
 * transcript, Node API, credential transfer or source activation occurs here. */
const reasons = new Set(["request-readiness", "finalized-counters", "durable-acknowledgment", "capacity"]);

/** Capture public selectors once and fence every queued/late operation against
 * the original owner. A diagnostic receipt is custody, never an expense ACK or
 * proof of observer freshness. Failures remain unavailable and vendor-neutral. */
export function createClaudeUsageGapSender({ observer, isCurrent, send } = {}) {
  let original;
  try {
    const session = observer?.external_session_id, generation = observer?.generation, witness = observer?.observer_witness;
    if (typeof session === "string" && /^[A-Za-z0-9._:-]{1,128}$/.test(session)
        && Number.isSafeInteger(generation) && generation > 0
        && typeof witness === "string" && /^[a-f0-9]{64}$/.test(witness)) {
      original = Object.freeze({ external_session_id: session, generation, observer_witness: witness });
    }
  } catch { /* malformed public metadata cannot create an observation */ }
  let stopped = !original || typeof isCurrent !== "function" || typeof send !== "function";
  let serial = Promise.resolve();
  const pending = new Map();
  const current = () => {
    if (stopped) return false;
    try { if (isCurrent() === true) return true; } catch { /* fence unavailable ownership */ }
    stopped = true;
    return false;
  };
  return Object.freeze({
    observe(reason) {
      if (!reasons.has(reason) || !current()) return Promise.resolve(false);
      if (pending.has(reason)) return pending.get(reason);
      const result = serial.then(async () => {
        if (!current()) return false;
        const raw = await send(Object.freeze({ operation: "curated-usage-gap", ...original, reason }));
        if (!current() || typeof raw !== "string" || raw.length > 128) return false;
        // The fixed helper emits canonical keys. Exact grammar rejects duplicate,
        // escaped-alias, extra/private keys and untyped neutral success output.
        return new RegExp('^\\s*\\{\\s*"observed"\\s*:\\s*true\\s*,\\s*"reason"\\s*:\\s*"'
          + reason + '"\\s*\\}\\s*$').test(raw);
      }).catch(() => false);
      pending.set(reason, result);
      serial = result.then(() => { pending.delete(reason); });
      return result;
    },
  });
}
