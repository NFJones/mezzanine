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
