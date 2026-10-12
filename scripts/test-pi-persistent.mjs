/** Ordinary Pi callback wiring fixtures. No vendor/provider or native authority
 * is fabricated: injectable clients test only neutral projection/queue ownership. */
import assert from "node:assert/strict";
import test from "node:test";
import { registerOrdinaryPiObserver } from "../crates/mezzanine/src/integrations/bootstrap/pi_persistent.mjs";

const env = { MEZ: "/tmp/fixture.sock\x1fsession=a\x1fwindow=@1\x1fpane=%1\x1fprotocol=mez-control/1", MEZ_PANE: "%1" };

/** Tracks resource opens and content-free facts without retaining callbacks. */
function clients() {
  const records = [];
  const create = options => {
    const record = { options, events: [], opens: 0, detached: false };
    records.push(record);
    return {
      async start() { record.opens++; return true; },
      async piObservation(event) { record.events.push({ ...event }); return { delivered: true }; },
      successor(instance) { return create({ ...options, instance, predecessor: "fixture" }); },
      detach() { record.detached = true; },
    };
  };
  return { records, create };
}

/** Factory exposes only observational on() subscriptions; each callback must
 * remain synchronous/undefined and never mutate the supplied vendor event. */
function load(owner, createClient, extra = {}) {
  const handlers = new Map();
  const pi = new Proxy({ on(type, callback) { handlers.set(type, callback); } }, {
    get(target, key) { assert.equal(key, "on", "vendor mutation API accessed"); return target[key]; },
  });
  registerOrdinaryPiObserver(pi, { owner, createClient, env, peerHelper: "/fixture/mez", ...extra });
  return (event, session = "session-a") => {
    const before = JSON.stringify(event);
    assert.equal(handlers.get(event.type)?.(event, { sessionManager: { getSessionId: () => session } }), undefined);
    assert.equal(JSON.stringify(event), before);
  };
}

test("ordinary genuine session opens once, drops content and fences duplicate/child callbacks", async () => {
  const owner = {};
  const { records, create } = clients();
  const main = load(owner, create);
  const duplicate = load(owner, create);
  assert.equal(records.length, 0, "loading performed resource I/O");
  main({ type: "session_start", reason: "startup", path: "PRIVATE" });
  duplicate({ type: "session_start", reason: "startup" });
  duplicate({ type: "agent_start", prompt: "PRIVATE" });
  main({ type: "agent_start", prompt: "PRIVATE" }, "child-session");
  main({ type: "agent_start", prompt: "PRIVATE" });
  main({ type: "agent_before_settle", outcome: "completed", text: "PRIVATE" });
  main({ type: "agent_settled", error: "PRIVATE" });
  await owner.tail;
  assert.equal(records.length, 1);
  assert.deepEqual(records[0].events, [{ type: "session_start", reason: "startup" },
    { type: "agent_start" }, { type: "agent_before_settle", outcome: "completed" }, { type: "agent_settled" }]);
  assert(!JSON.stringify(records).includes("PRIVATE"));
  main({ type: "session_shutdown", reason: "quit" });
  await owner.tail;
  assert.equal(owner.closed, true);
  assert.equal(records[0].detached, true);
});

test("reload successor and new/resume/fork handoffs are ordered and old closures inert", async () => {
  const owner = {};
  const { records, create } = clients();
  const old = load(owner, create);
  old({ type: "session_start", reason: "startup" });
  old({ type: "agent_start" });
  old({ type: "session_shutdown", reason: "reload" });
  const replacement = load(owner, create);
  replacement({ type: "session_start", reason: "reload" });
  old({ type: "agent_settled" });
  replacement({ type: "agent_start" });
  await owner.tail;
  assert.equal(records.length, 2);
  assert.equal(records[0].detached, true);
  assert.deepEqual(records[1].events, [{ type: "session_start", reason: "reload" }, { type: "agent_start" }]);
  assert.equal(records[1].options.predecessor, "fixture");
  let session = "session-a";
  for (const [reason, next] of [["new", "session-b"], ["resume", "session-a"], ["fork", "branch"]]) {
    replacement({ type: "session_shutdown", reason }, session);
    replacement({ type: "agent_start" }, session);
    replacement({ type: "session_start", reason }, next);
    await owner.tail;
    assert.equal(records.at(-1).options.session, next);
    assert.deepEqual(records.at(-1).events, [{ type: "session_start", reason }]);
    session = next;
  }
});

test("outside Mez and invalid/stale context stay neutral without opening a client", () => {
  let opened = 0;
  const owner = {};
  const inactive = load(owner, () => { opened++; }, { env: {} });
  inactive({ type: "session_start", reason: "startup" });
  const active = load(owner, () => { opened++; });
  active({ type: "session_start", reason: "unknown" });
  active({ type: "session_start", reason: "startup" }, "../private-path");
  assert.equal(opened, 0);
});

/** Persisted turn_end, not early message_end, supplies actual request expense.
 * The real ordinary callback entry must obtain readiness first and project only
 * normalized inert counters; lifecycle settlement remains a separate event. */
test("ordinary finalized Pi request emits checked inclusive usage with stable identity", async () => {
  const owner = {};
  const reports = [];
  let starts = 0;
  const main = load(owner, () => ({
    async start() { return true; },
    async piObservation() { return { delivered: true }; },
    async beginRequest(id) { starts++; return Object.freeze({ request_id: id, admitted_at: 1, source_witness: "a".repeat(64), origin: "unattributed" }); },
    async endRequest(ticket, report) { reports.push({ ticket, report }); return { delivered: true, durable: true, applied: reports.length === 1, revision: 1 }; },
    abandonRequest() {}, detach() {},
  }));
  main({ type: "session_start", reason: "startup" });
  await owner.tail;
  main({ type: "turn_start", turnIndex: 0, timestamp: 1000 });
  await owner.tail;
  const message = { role: "assistant", provider: "actual-provider", model: "requested-model", responseModel: "concrete-model", stopReason: "error", usage: { input: 5, output: 7, cacheRead: 2, cacheWrite: 3, reasoning: 4, cacheWrite1h: 1 }, content: [{ type: "text", text: "PRIVATE" }], errorMessage: "PRIVATE" };
  main({ type: "message_end", message });
  assert.equal(reports.length, 0);
  main({ type: "turn_end", turnIndex: 0, messageEntryId: "persisted-id-a", message, toolResults: [{ private: "PRIVATE" }] });
  await owner.tail;
  assert.equal(starts, 1);
  assert.equal(reports.length, 1);
  assert.equal(reports[0].report.provider, "actual-provider");
  assert.equal(reports[0].report.model, "concrete-model");
  assert.deepEqual(reports[0].report.counters, { input_tokens: 10, output_tokens: 7, cached_input_tokens: 2, cache_write_input_tokens: 3, reasoning_tokens: 4 });
  assert.match(reports[0].report.event_id, /^[a-f0-9]{64}$/);
  assert(!JSON.stringify(reports).includes("PRIVATE"));
});

test("lost replacement reply retains exact attempted instance for next genuine reload", async () => {
  const owner = {};
  const records = [];
  const create = options => {
    const record = { options, starts: 0, detached: false, facts: [] };
    records.push(record);
    return {
      async start() { record.starts++; return !(records.indexOf(record) === 1 && record.starts === 1); },
      successor(instance) { return create({ ...options, instance, from: options.instance }); },
      async piObservation(event) { record.facts.push({ ...event }); return { delivered: true }; },
      detach() { record.detached = true; },
    };
  };
  const first = load(owner, create);
  first({ type: "session_start", reason: "startup" });
  await owner.tail;
  first({ type: "session_shutdown", reason: "reload" });
  const lost = load(owner, create);
  lost({ type: "session_start", reason: "reload" });
  await owner.tail;
  assert.equal(records.length, 2);
  assert.equal(records[1].detached, false, "failed attempted instance was discarded");
  lost({ type: "session_shutdown", reason: "reload" });
  const next = load(owner, create);
  next({ type: "session_start", reason: "reload" });
  await owner.tail;
  assert.equal(records[1].starts, 2, "exact lost attempt was not recovered");
  assert.equal(records[2].options.from, records[1].options.instance);
  assert.equal(records[1].detached, true);
  assert.deepEqual(records[2].facts, [{ type: "session_start", reason: "reload" }]);
});

test("bounded callback queue loses telemetry without vendor mutation or unbounded facts", async () => {
  const owner = {};
  const { records, create } = clients();
  const main = load(owner, create);
  main({ type: "session_start", reason: "startup" });
  for (let index = 0; index < 100; index++) main({ type: "agent_start", prompt: "PRIVATE" });
  assert(owner.pending <= 32);
  await owner.tail;
  assert.equal(records.length, 0, "canceled queued activation opened a client");
  assert(!JSON.stringify(records).includes("PRIVATE"));
});

test("overflow during asynchronous activation disposes transport and publishes no start", async () => {
  const owner = {};
  const events = [];
  let release;
  let started;
  let stopped = false;
  const entered = new Promise(resolve => { started = resolve; });
  const create = () => ({
    start() { started(); return new Promise(resolve => { release = resolve; }); },
    async piObservation(event) { events.push(event); return { delivered: true }; },
    detach() { stopped = true; },
    disconnect() { stopped = true; },
  });
  const main = load(owner, create);
  main({ type: "session_start", reason: "startup" });
  await entered;
  for (let index = 0; index < 100; index++) main({ type: "agent_start" });
  release(true);
  await owner.tail;
  assert.equal(stopped, true, "canceled activation retained a transport");
  assert.deepEqual(events, [], "canceled activation published fresh telemetry");
  assert.equal(owner.pending, 0);
});

/** Failed durable usage reporting remains disclosed after genuine reload. The
 * successor re-publishes only the sticky reason, never inherits/replays an old
 * client ticket or uses lifecycle settlement as accounting acknowledgment. */
test("ordinary Pi lost-ACK disclosure survives genuine reload without ticket transfer", async () => {
  const owner = {}; const records = [];
  const create = options => {
    const record = { options, events: [], begins: 0 }; records.push(record);
    return { async start() { return true; }, async piObservation(event) { record.events.push(event); return { delivered: true }; },
      async beginRequest(id) { record.begins++; return { request_id: id, admitted_at: 1 }; },
      async endRequest() { return { delivered: false, durable: false }; }, abandonRequest() {}, detach() {},
      successor(instance) { return create({ ...options, instance }); } };
  };
  const main = load(owner, create); main({ type: "session_start", reason: "startup" }); await owner.tail;
  main({ type: "agent_start" }); main({ type: "turn_start", turnIndex: 0 });
  const message = { role: "assistant", provider: "provider", model: "model", stopReason: "error", usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0 } };
  main({ type: "message_end", message }); main({ type: "turn_end", turnIndex: 0, messageEntryId: "persisted", message });
  await owner.tail; await owner.tail;
  assert(records[0].events.some(event => event.type === "usage_gap" && event.reason === "durable-acknowledgment"));
  main({ type: "session_shutdown", reason: "reload" });
  const replacement = load(owner, create); replacement({ type: "session_start", reason: "reload" }); await owner.tail;
  assert.deepEqual(records[1].events, [{ type: "session_start", reason: "reload" }, { type: "usage_gap", reason: "durable-acknowledgment" }]);
  replacement({ type: "turn_end", turnIndex: 0, messageEntryId: "persisted", message }); await owner.tail;
  assert.equal(records[1].begins, 0);
});

/** Genuine run/settlement/shutdown boundaries must disclose unavailable final
 * counters before abandoning the original request and before native retirement.
 * The callback never waits for readiness, changes vendor results, or invents cost. */
test("ordinary unfinished Pi request discloses loss before boundary retirement", async () => {
  for (const boundary of [{ type: "agent_start" }, { type: "agent_settled" },
    ...["quit", "reload", "new", "resume", "fork"].map(reason => ({ type: "session_shutdown", reason }))]) {
    const owner = {}; const events = []; let abandoned = 0; let expense = 0; let release; let entered;
    const begun = new Promise(resolve => { entered = resolve; });
    const main = load(owner, () => ({ async start() { return true; },
      async piObservation(event) { events.push(event); return { delivered: true }; },
      beginRequest(id) { entered(); return new Promise(resolve => { release = () => resolve({ request_id: id, admitted_at: 1 }); }); },
      async endRequest() { expense++; return { delivered: true, durable: true, applied: true, revision: 1 }; },
      abandonRequest() { events.push({ type: "abandon" }); abandoned++; }, detach() { events.push({ type: "detach" }); } }));
    main({ type: "session_start", reason: "startup" }); await owner.tail;
    main({ type: "agent_start" }); main({ type: "turn_start", turnIndex: 0 }); await begun;
    main(boundary); release(); await owner.tail; await owner.tail;
    const gap = events.findIndex(event => event.type === "usage_gap" && event.reason === "finalized-counters");
    const retirement = events.findIndex(event => event.type === boundary.type && (event.reason === boundary.reason) && (boundary.type !== "agent_start" || events.indexOf(event) > 1));
    assert(gap >= 0, JSON.stringify({ boundary, events }));
    assert(gap < events.findIndex(event => event.type === "abandon"));
    assert(gap < retirement, JSON.stringify({ boundary, events }));
    assert.equal(abandoned, 1); assert.equal(expense, 0);
  }
});

/** A complete finalized report must not acquire a false loss at settlement,
 * while a reordered historical end cannot cancel the genuine current ticket. */
test("ordinary Pi completed and reordered callbacks preserve current ticket without false boundary loss", async () => {
  const owner = {}; const events = []; let abandoned = 0; let expense = 0;
  const main = load(owner, () => ({ async start() { return true; },
    async piObservation(event) { events.push(event); return { delivered: true }; },
    async beginRequest(id) { return { request_id: id, admitted_at: 1 }; },
    async endRequest() { expense++; return { delivered: true, durable: true, applied: true, revision: 1 }; },
    abandonRequest() { abandoned++; }, detach() {} }));
  main({ type: "session_start", reason: "startup" }); await owner.tail;
  main({ type: "agent_start" }); main({ type: "turn_start", turnIndex: 0 });
  const message = { role: "assistant", provider: "provider", model: "model", stopReason: "stop", usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0 } };
  main({ type: "message_end", message }); main({ type: "turn_end", turnIndex: 0, messageEntryId: "complete", message });
  main({ type: "agent_settled" }); await owner.tail;
  assert.equal(expense, 1); assert.equal(abandoned, 0); assert(!events.some(event => event.type === "usage_gap"));
  main({ type: "agent_start" }); main({ type: "turn_start", turnIndex: 0 });
  main({ type: "turn_end", turnIndex: 99, messageEntryId: "historical", message: { ...message } });
  await owner.tail; assert.equal(abandoned, 0);
  const current = { ...message }; main({ type: "message_end", message: current });
  main({ type: "turn_end", turnIndex: 0, messageEntryId: "current", message: current });
  main({ type: "agent_settled" }); await owner.tail;
  assert.equal(expense, 2); assert.equal(abandoned, 0);
});
