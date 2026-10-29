/** Live Copilot per-call metadata projection and finite best-effort delivery.
 * No SDK, socket, timer or provider work occurs on import. The callback retains
 * only frozen allowlisted scalars, never events/content or raw accounting IDs.
 * Terminal observations cannot establish original request time or project.
 */
import { createHash } from "node:crypto";

const integer = value => Number.isSafeInteger(value) && value >= 0;
const text = (value, maximum) => typeof value === "string" && value.trim().length > 0
  && Buffer.byteLength(value) <= maximum && !/[\p{Cc}\u202a-\u202e\u2066-\u2069]/u.test(value);
const gaps = Object.freeze(["missing-identity", "counters-unavailable", "invalid-counters",
  "queue-pressure", "delivery-unavailable", "observer-lost"]);

/** Project one live API-call event; unrelated/aggregate events return nothing.
 * Required totals must be known, caches are subsets of inclusive raw input,
 * and optional unknown fields stay absent. Billing multipliers are never money.
 */
export function projectCopilotUsage(event) {
  try {
    if (event?.type !== "assistant.usage") return;
    const data = event.data, identity = data?.accounting;
    if (!text(identity?.sourceSessionId, 4096) || !text(identity?.usageId, 4096)
        || !integer(identity?.sequence) || identity.sequence === 0) {
      return Object.freeze({ gap: "missing-identity" });
    }
    if (data.inputTokens === undefined || data.outputTokens === undefined || data.model === undefined) {
      return Object.freeze({ gap: "counters-unavailable" });
    }
    if (!text(data.model, 128) || !integer(data.inputTokens) || !integer(data.outputTokens)) {
      return Object.freeze({ gap: "invalid-counters" });
    }
    const counters = { input_tokens: data.inputTokens, output_tokens: data.outputTokens };
    for (const [source, target, total] of [["reasoningTokens", "reasoning_tokens", data.outputTokens],
      ["cacheReadTokens", "cached_input_tokens", data.inputTokens],
      ["cacheWriteTokens", "cache_write_input_tokens", data.inputTokens]]) {
      const value = data[source];
      if (value !== undefined) {
        if (!integer(value) || value > total) return Object.freeze({ gap: "invalid-counters" });
        counters[target] = value;
      }
    }
    if (counters.cached_input_tokens !== undefined && counters.cache_write_input_tokens !== undefined
        && counters.cached_input_tokens > data.inputTokens - counters.cache_write_input_tokens) {
      return Object.freeze({ gap: "invalid-counters" });
    }
    const call_id = createHash("sha256").update("mez-copilot-terminal-v1\0")
      .update(JSON.stringify([identity.sourceSessionId, identity.sequence, identity.usageId])).digest("hex");
    return Object.freeze({ call_id, model: data.model, counters: Object.freeze(counters) });
  } catch { return Object.freeze({ gap: "invalid-counters" }); }
}

/** Hold at most64 projected reports until an actual positive pulse ACK.
 * Each immutable report/gap has at most3 serialized attempts,500ms apart;
 * fixed transports own their3s deadlines. Sticky reasons have six bounded slots.
 * Stop fences late replies/timers; already admitted durable expense is retained.
 */
export function createCopilotUsageReporter({ send, gap, setTimeout, clearTimeout } = {}) {
  const queue = [], reasons = new Map();
  let stopped = false, qualified = false, busy = false, timer;
  const note = reason => {
    if (gaps.includes(reason) && !reasons.has(reason)) reasons.set(reason, { attempts: 0, done: false });
  };
  const stop = () => {
    stopped = true;
    queue.length = 0;
    try { if (timer !== undefined) clearTimeout(timer); } catch { /* private teardown */ }
    timer = undefined;
  };
  const pump = async () => {
    if (stopped || !qualified || busy || timer !== undefined) return;
    const reason = [...reasons].find(([, state]) => !state.done && state.attempts < 3);
    const item = reason ? undefined : queue[0];
    if (!reason && !item) return;
    busy = true;
    let reply;
    try {
      if (reason) { reason[1].attempts++; reply = await gap(reason[0]); }
      else { item.attempts++; reply = await send(item.report); }
    } catch { /* unavailable delivery never affects provider behavior */ }
    finally { busy = false; }
    if (stopped) return;
    if (reply?.terminal === true) { note("observer-lost"); stop(); return; }
    let retry;
    if (reason) {
      reason[1].done = reply?.observed === true && reply.reason === reason[0];
      retry = !reason[1].done && reason[1].attempts < 3;
    } else {
      const accepted = reply?.accepted === true && reply.durable === true;
      retry = !accepted && item.attempts < 3;
      if (!retry) {
        queue.shift();
        if (!accepted) note("delivery-unavailable");
      }
    }
    if (retry) {
      try { timer = setTimeout(() => { timer = undefined; void pump(); }, 500); timer?.unref?.(); }
      catch { stop(); }
    } else void pump();
  };
  if ([send, gap, setTimeout, clearTimeout].some(value => typeof value !== "function")) stop();
  return Object.freeze({
    /** Snapshot live metadata before SDK event callback returns. */
    observe(event) {
      if (stopped) return;
      const report = projectCopilotUsage(event);
      if (!report) return;
      if (report.gap) note(report.gap);
      else if (queue.length >= 64) note("queue-pressure");
      else queue.push({ report, attempts: 0 });
      void pump();
    },
    /** First actual positive ACK of any pulse tick enables delivery once. */
    acknowledgePulse(reply, sequence) {
      if (stopped || qualified || reply?.observed !== true || reply.sequence !== sequence
          || !Number.isSafeInteger(sequence) || sequence <= 0 || typeof reply.changed !== "boolean") return;
      qualified = true;
      void pump();
    },
    stop,
  });
}
