/** Offline live-usage projection and finite delivery regressions.
 * Synthetic metadata never invokes an SDK/vendor/provider. Native writer and
 * ledger evidence belongs to installed Rust fixtures, not these callbacks.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { setImmediate as settle } from "node:timers/promises";
import { projectCopilotUsage, createCopilotUsageReporter } from "../crates/mezzanine/src/integrations/bootstrap/copilot_usage.mjs";

const event = (extra = {}) => ({ type: "assistant.usage", agentId: "child", data: {
  accounting: { sourceSessionId: "source-a", sequence: 1, usageId: "usage-a" },
  model: "fixture-model", inputTokens: 10, outputTokens: 4, ...extra,
} });
const ack = { observed: true, sequence: 1, changed: true };
const durable = { accepted: true, durable: true };

/** Virtual timers expose every finite retry without wall-clock waits. */
function fixture(send = async () => durable, gap = async reason => ({ observed: true, reason })) {
  const timers = new Map(), reports = [], gaps = [];
  let id = 0;
  const reporter = createCopilotUsageReporter({
    send: async report => { reports.push(report); return await send(report); },
    gap: async reason => { gaps.push(reason); return await gap(reason); },
    setTimeout: (callback, delay) => { assert.equal(delay, 500); timers.set(++id, callback); return id; },
    clearTimeout: key => timers.delete(key),
  });
  return { reporter, reports, gaps, timers, async retry() {
    assert.equal(timers.size, 1); const [key, callback] = timers.entries().next().value;
    timers.delete(key); callback(); await settle();
  } };
}

/** Full accounting identity prevents provider-ID reuse merging distinct calls;
 * content, billing, timing and parent aggregates never survive projection.
 */
test("Copilot projects only immutable inclusive per-call metadata", () => {
  const source = event({ cacheReadTokens: 3, cacheWriteTokens: 2, reasoningTokens: 1,
    cost: 999, prompt: "PRIVATE", duration: 999, copilotUsage: { totalNanoAiu: 999 } });
  const report = projectCopilotUsage(source);
  assert.deepEqual(Object.keys(report).sort(), ["call_id", "counters", "model"]);
  assert.deepEqual(report.counters, { input_tokens: 10, output_tokens: 4,
    reasoning_tokens: 1, cached_input_tokens: 3, cache_write_input_tokens: 2 });
  assert(Object.isFrozen(report) && Object.isFrozen(report.counters));
  assert(!JSON.stringify(report).includes("PRIVATE"));
  source.data.inputTokens = 100;
  assert.equal(report.counters.input_tokens, 10);
  for (const identity of [{ sourceSessionId: "source-b", sequence: 1, usageId: "usage-a" },
    { sourceSessionId: "source-a", sequence: 2, usageId: "usage-a" },
    { sourceSessionId: "source-a", sequence: 1, usageId: "usage-b" }]) {
    assert.notEqual(projectCopilotUsage(event({ accounting: identity })).call_id, report.call_id);
  }
  assert.equal(projectCopilotUsage(event()).call_id, report.call_id);
  for (const type of ["session.shutdown", "assistant.turn_end", "session.usage_info", "assistant.message"]) {
    assert.equal(projectCopilotUsage({ ...source, type }), undefined);
  }
});

/** Unknown optional categories remain absent, explicit zero is retained, and
 * missing/invalid totals never become a successful zero-expense record.
 */
test("Copilot refuses unavailable identity, unsafe counters and subset errors", () => {
  assert.deepEqual(projectCopilotUsage(event()).counters, { input_tokens: 10, output_tokens: 4 });
  assert.deepEqual(projectCopilotUsage(event({ inputTokens: 0, outputTokens: 0, cacheReadTokens: 0 })).counters,
    { input_tokens: 0, output_tokens: 0, cached_input_tokens: 0 });
  for (const accounting of [undefined, {}, { sourceSessionId: "s", sequence: 0, usageId: "u" },
    { sourceSessionId: "s", sequence: 1, usageId: "u".repeat(4097) }]) {
    assert.equal(projectCopilotUsage(event({ accounting })).gap, "missing-identity");
  }
  assert.equal(projectCopilotUsage(event({ inputTokens: undefined })).gap, "counters-unavailable");
  for (const extra of [{ inputTokens: -1 }, { outputTokens: null }, { inputTokens: 1.5 },
    { outputTokens: Number.MAX_SAFE_INTEGER + 1 }, { reasoningTokens: 5 },
    { cacheReadTokens: 8, cacheWriteTokens: 3 }, { cacheReadTokens: null }, { model: "bad\u202e" }]) {
    assert.equal(projectCopilotUsage(event(extra)).gap, "invalid-counters");
  }
});

/** Early events survive first pulse ACK loss. A later actual duplicate positive
 * ACK enables the exact retained metadata once; no fabricated ready signal.
 */
test("Copilot waits for a real positive pulse ACK on any tick", async () => {
  const f = fixture(); f.reporter.observe(event());
  f.reporter.acknowledgePulse(undefined, 1);
  f.reporter.acknowledgePulse({ ...ack, sequence: 2 }, 1);
  await settle(); assert.equal(f.reports.length, 0);
  f.reporter.acknowledgePulse({ ...ack, changed: false }, 1);
  await settle(); assert.equal(f.reports.length, 1);
  f.reporter.acknowledgePulse(ack, 1); await settle(); assert.equal(f.reports.length, 1);
  f.reporter.stop();
});

/** Ambiguous delivery retries the same frozen object at most3 times, then emits
 * one fixed sticky gap without any provider retry or arbitrary diagnostic text.
 */
test("Copilot bounds retries and records unavailable delivery", async () => {
  const f = fixture(async () => undefined);
  f.reporter.observe(event()); f.reporter.acknowledgePulse(ack, 1); await settle();
  await f.retry(); await f.retry();
  assert.equal(f.reports.length, 3);
  assert(f.reports.every(report => report === f.reports[0]));
  assert.deepEqual(f.gaps, ["delivery-unavailable"]); assert.equal(f.timers.size, 0);
  f.reporter.observe(event({ inputTokens: undefined })); await settle();
  f.reporter.observe(event({ inputTokens: undefined })); await settle();
  assert.deepEqual(f.gaps, ["delivery-unavailable", "counters-unavailable"]);
  f.reporter.stop();
});

/** Queue pressure is finite even before enrollment. Gap attempts also cap at3
 * and cannot starve all retained reports indefinitely when diagnostics fail.
 */
test("Copilot bounds pending metadata and unavailable gap delivery", async () => {
  const f = fixture(undefined, async () => undefined);
  for (let sequence = 1; sequence <= 1000; sequence++) {
    f.reporter.observe(event({ accounting: { sourceSessionId: "s", sequence, usageId: "u" } }));
  }
  f.reporter.acknowledgePulse(ack, 1); await settle(); await f.retry(); await f.retry();
  assert.deepEqual(f.gaps, ["queue-pressure", "queue-pressure", "queue-pressure"]);
  assert.equal(f.reports.length, 64); assert.equal(f.timers.size, 0);
  f.reporter.stop();
});

/** Stop clears timers/queued work and fences an actual in-flight result. Native
 * durable expense already admitted is intentionally not undone by local stop.
 */
test("Copilot shutdown fences early queues, retries and late results", async () => {
  let finish;
  const f = fixture(() => new Promise(resolve => { finish = resolve; }));
  f.reporter.observe(event()); f.reporter.observe(event());
  f.reporter.acknowledgePulse(ack, 1); await settle(); assert.equal(f.reports.length, 1);
  f.reporter.stop(); finish(durable); await settle();
  f.reporter.observe(event()); f.reporter.acknowledgePulse(ack, 1); await settle();
  assert.equal(f.reports.length, 1); assert.equal(f.timers.size, 0);
  const g = fixture(async () => undefined); g.reporter.observe(event());
  g.reporter.acknowledgePulse(ack, 1); await settle(); assert.equal(g.timers.size, 1);
  g.reporter.stop(); assert.equal(g.timers.size, 0);
  const h = fixture(); h.reporter.observe(event()); h.reporter.stop();
  h.reporter.acknowledgePulse(ack, 1); await settle(); assert.equal(h.reports.length, 0);
});

/** A malformed blank model must be rejected before any terminal IPC. Its fixed
 * gap cannot prevent subsequent genuine per-call metadata from being delivered.
 * This regression failed against the original projector before the trim fence.
 */
test("Copilot rejects blank models locally and continues valid delivery", async () => {
  const f = fixture();
  for (const model of [" ", "\u2003", "\t"]) {
    assert.equal(projectCopilotUsage(event({ model })).gap, "invalid-counters");
    f.reporter.observe(event({ model }));
  }
  f.reporter.observe(event()); f.reporter.acknowledgePulse(ack, 1); await settle();
  assert.deepEqual(f.gaps, ["invalid-counters"]);
  assert.equal(f.reports.length, 1); assert.equal(f.reports[0].model, "fixture-model");
  assert.equal(f.timers.size, 0); f.reporter.stop();
});
