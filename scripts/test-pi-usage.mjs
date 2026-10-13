/** Pi source-contract projection/normalization tests. Synthetic counts only;
 * no transcripts, credentials, vendor execution or provider requests. */
import assert from "node:assert/strict";
import test from "node:test";
import { normalizePiUsage, projectPiUsage, createPiUsageObserver } from "../crates/mezzanine/src/integrations/bootstrap/pi_usage.mjs";
import { createRequestAccountingClient } from "../crates/mezzanine/src/integrations/bootstrap/request_accounting.mjs";
import { registerOrdinaryPiObserver } from "../crates/mezzanine/src/integrations/bootstrap/pi_persistent.mjs";

const usage = () => ({ input: 5, output: 7, cacheRead: 2, cacheWrite: 3 });
const message = () => ({ role: "assistant", provider: "provider", model: "requested", responseModel: "actual", stopReason: "stop", usage: usage() });
const event = (msg, id = "entry-a", turnIndex = 0) => ({ type: "turn_end", turnIndex, messageEntryId: id, message: msg });

/** Required unknown input partitions cannot become plausible zero input. Zero
 * and optional subset absence remain distinct, and all arithmetic is exact. */
test("Pi partitions are checked inclusive input and output subsets", () => {
  assert.deepEqual(normalizePiUsage(usage()), { input_tokens: 10, output_tokens: 7, cached_input_tokens: 2, cache_write_input_tokens: 3 });
  assert.equal(Object.hasOwn(normalizePiUsage(usage()), "reasoning_tokens"), false);
  assert.equal(normalizePiUsage({ ...usage(), reasoning: 0 }).reasoning_tokens, 0);
  assert.equal(normalizePiUsage({ input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }).input_tokens, 0);
  assert.equal(normalizePiUsage({ ...usage(), reasoning: 7, cacheWrite1h: 3 }).output_tokens, 7);
  for (const field of ["input", "output", "cacheRead", "cacheWrite"]) {
    const missing = usage(); delete missing[field]; assert.equal(normalizePiUsage(missing), undefined);
    for (const bad of [-1, 0.5, NaN, Infinity, null, "0", Number.MAX_SAFE_INTEGER + 1]) assert.equal(normalizePiUsage({ ...usage(), [field]: bad }), undefined);
  }
  for (const bad of [{ reasoning: 8 }, { reasoning: null }, { cacheWrite1h: 4 }, { cacheWrite1h: -1 }, { input: Number.MAX_SAFE_INTEGER }, { output: Number.MAX_SAFE_INTEGER }]) assert.equal(normalizePiUsage({ ...usage(), ...bad }), undefined);
});

/** Only persisted assistant IDs and actual response attribution enter a report.
 * Getters for raw content, totals, costs and diagnostic text are never read. */
test("Pi finalized projector ignores private content and unsupported pending outcomes", () => {
  const msg = message();
  for (const field of ["content", "errorMessage", "timestamp", "deferred"]) Object.defineProperty(msg, field, { get() { throw new Error("PRIVATE accessed"); } });
  for (const field of ["totalTokens", "cost"]) Object.defineProperty(msg.usage, field, { get() { throw new Error("PRIVATE accessed"); } });
  const report = projectPiUsage("session-a", event(msg));
  assert.equal(report.model, "actual"); assert.equal(report.provider, "provider"); assert.match(report.event_id, /^[a-f0-9]{64}$/);
  for (const stopReason of ["stop", "length", "toolUse", "error", "aborted"]) assert(projectPiUsage("session-a", event({ ...message(), stopReason })));
  for (const stopReason of ["pending", "deferred", "unknown"]) assert.equal(projectPiUsage("session-a", event({ ...message(), stopReason })), undefined);
  assert.equal(projectPiUsage("session-a", { ...event(message()), messageEntryId: undefined }), undefined);
  assert.notEqual(projectPiUsage("session-b", event(message())).event_id, report.event_id);
});

/** Controlled client fixtures expose immutable report/retry ownership only.
 * Enqueue behavior matches the ordinary callback's asynchronous serial tail. */
function fixture(extra = {}) {
  let tail = Promise.resolve(); const reports = []; const markers = []; let clock = 20;
  const client = { async piObservation() { return { delivered: true }; }, async beginRequest(id) { markers.push(id); return { request_id: id, admitted_at: 10 }; },
    async endRequest(ticket, report) { reports.push({ ticket, report }); return { delivered: true, durable: true, applied: true, revision: 1 }; }, abandonRequest() {}, ...extra };
  const owned = { session: "session-a", client }; const recent = new Map();
  const observer = createPiUsageObserver(owned, operation => { tail = tail.then(operation); return true; }, recent, () => clock);
  const start = (index = 0) => observer.turnStart({ type: "turn_start", turnIndex: index });
  const end = (msg, id = "entry-a", index = 0) => { observer.messageEnd({ type: "message_end", message: msg }); observer.turnEnd(event(msg, id, index)); };
  return { observer, owned, recent, reports, markers, start, end, drain: () => tail, clock: value => { clock = value; } };
}

/** Readiness may finish after the vendor callback. Projected counters cannot be
 * changed by the later vendor object, and lost ACK retries preserve exact UTC. */
test("Pi finalized counters and publication clock freeze across delayed readiness and ACK loss", async () => {
  let release; let entered; const started = new Promise(resolve => { entered = resolve; }); let attempts = 0;
  const f = fixture({ beginRequest(id) { entered(); return new Promise(resolve => { release = () => resolve({ request_id: id, admitted_at: 10 }); }); },
    async endRequest(ticket, report) { f.reports.push({ ticket, report }); return { delivered: ++attempts > 1, durable: attempts > 1, applied: true, revision: 1 }; } });
  f.start(); await started; const msg = message(); f.end(msg); msg.usage.input = 999; msg.model = "changed";
  release(); await f.drain(); assert.equal(f.reports[0].report.counters.input_tokens, 10);
  const original = f.reports[0].report; f.clock(99); f.observer.turnEnd(event(message())); await f.drain();
  assert.equal(f.reports.length, 2); assert.deepEqual(f.reports[1].report, original); assert.equal(f.owned.usageCoverage, "finalized-assistant-external-reported");
});

/** Old/reordered finalized callbacks cannot borrow a new same-index ticket.
 * Reload's bounded recent metadata rejects changed IDs without transferring the
 * original instance-local ticket; eviction never enables historical import. */
test("Pi historical and conflicting callbacks cannot attach to current request tickets", async () => {
  const f = fixture(); f.start(); f.end(message()); await f.drain();
  f.start(); f.observer.turnEnd(event(message())); await f.drain(); assert.equal(f.reports.length, 1);
  f.observer.turnEnd(event(message(), "unknown-historical")); await f.drain(); assert.equal(f.reports.length, 1);
  const fresh = message(); f.observer.messageEnd({ type: "message_end", message: fresh });
  f.observer.turnEnd(event(message()));
  f.observer.turnEnd(event(fresh, "entry-b")); await f.drain(); assert.equal(f.reports.length, 2);
  const changed = message(); changed.usage.input = 8; f.observer.turnEnd(event(changed, "entry-b")); await f.drain(); assert.equal(f.reports.length, 2);
  assert.equal(f.owned.usageCoverage, "unavailable-immutable-observation-conflict");
  for (let index = 1; index <= 260; index++) { f.start(index); f.end(message(), `entry-${index}`, index); await f.drain(); }
  assert(f.recent.size <= 128); const before = f.reports.length; f.start(0); f.observer.turnEnd(event(message())); await f.drain(); assert.equal(f.reports.length, before);
});

/** A preceding message_end extension returns another object; Pi then mutates
 * its original finalized object before persistence/turn_end. Only an independently
 * seen provider-native response ID may bridge that object replacement. */
test("Pi earlier/later extension replacement requires exact native correlation", async () => {
  for (const before of [true, false]) {
    const f = fixture(); f.start();
    const original = { ...message(), responseId: "native-response-a" };
    f.observer.messageIdentity({ type: "message_start", message: { ...original } });
    const replacement = { ...original, usage: { ...usage(), input: 3 } };
    f.observer.messageEnd({ type: "message_end", message: before ? replacement : original });
    Object.assign(original, replacement);
    f.observer.turnEnd(event(original)); await f.drain();
    assert.equal(f.reports.length, 1); assert.equal(f.reports[0].report.counters.input_tokens, 8);
  }
  for (const altered of [{ responseId: undefined }, { responseId: "different" }, { provider: "different" }]) {
    const f = fixture(); f.start(); const original = { ...message(), responseId: "native-response-a" };
    f.observer.messageIdentity({ type: "message_update", message: { ...original } });
    f.observer.messageEnd({ type: "message_end", message: { ...original } });
    Object.assign(original, altered); f.observer.turnEnd(event(original)); await f.drain(); assert.equal(f.reports.length, 0);
  }
});

/** The real request client retains undurable tickets. Recent-map eviction must
 * abandon the original unknown-delivery ticket before the next begin so recovery
 * cannot remain stuck forever at its 128-ticket boundary. */
test("Pi capacity releases original undurable tickets before admitting recovery", async () => {
  let healthy = false; let admitted = 0; let charged = 0; let tail = Promise.resolve();
  const enqueue = operation => { const result = tail.then(operation); tail = result.then(() => {}); return result; };
  const client = { ...createRequestAccountingClient({ enqueue, async exchange(method, payload) {
    if (method.endsWith("usage-begin")) { admitted++; return { protocol: "external-request/1", accepted: true, request_id: payload.request_id, admitted_at: 10, source_witness: "a".repeat(64), origin: "unattributed", controls: [] }; }
    if (!healthy) return { accepted: true, durable: false };
    charged++; return { accepted: true, durable: true, applied: true, revision: charged, coverage: { source: "external-reported", reasoning_known: false } };
  } }), async piObservation() { return { delivered: true }; } };
  const owned = { session: "session-a", client }; const recent = new Map(); let callbackTail = Promise.resolve();
  const observer = createPiUsageObserver(owned, operation => { callbackTail = callbackTail.then(operation); return true; }, recent, () => 20);
  const request = async index => { observer.turnStart({ type: "turn_start", turnIndex: index }); const msg = message(); observer.messageEnd({ type: "message_end", message: msg }); observer.turnEnd(event(msg, `capacity-${index}`, index)); await callbackTail; };
  for (let index = 0; index < 128; index++) await request(index);
  healthy = true; await request(128); await request(129);
  assert.equal(charged, 2); assert(recent.size <= 128); assert(admitted > 128);
});

/** Missing counters after a successful expense must publish a bounded inert
 * usage-gap fact, not just set a private flag or claim the earlier totals are
 * complete. Publication carries no amount, response ID or vendor content. */
test("Pi loss after prior durable expense publishes only allowlisted coverage reason", async () => {
  const facts = [];
  const f = fixture({ async piObservation(fact) { facts.push(fact); return { delivered: true }; } });
  f.start(); f.end(message()); await f.drain();
  f.start(1); const missing = message(); delete missing.usage.cacheRead;
  f.end(missing, "missing", 1); await f.drain();
  assert.deepEqual(facts, [{ type: "usage_gap", reason: "finalized-counters" }]);
  assert.equal(f.reports.length, 1);
  assert(!JSON.stringify(facts).includes("provider"));
});

/** A missing correlator in one prompt cannot pin all later turnIndex=0
 * prompts forever. Only genuine agent_start releases unfinished readiness;
 * an unrelated/reordered finalized callback cannot perform that reset. */
test("Pi genuine run boundary recovers after uncorrelatable extension replacement", async () => {
  const f = fixture(); f.observer.runStart(); f.start();
  f.observer.messageEnd({ type: "message_end", message: { ...message() } });
  f.observer.turnEnd(event(message(), "uncorrelatable")); await f.drain();
  assert.equal(f.reports.length, 0);
  for (const id of ["healthy-b", "healthy-c"]) {
    f.observer.runStart(); f.start(); f.end(message(), id); await f.drain();
  }
  assert.equal(f.reports.length, 2); assert.equal(f.markers.length, 3);
});

/** Undurable or lost ACK results carry a content-free disclosure through the
 * production observation method, not merely a private coverage flag. */
test("Pi failed durable ACK emits sticky observer gap", async () => {
  const facts = [];
  const f = fixture({ async piObservation(fact) { facts.push(fact); return { delivered: true }; },
    async endRequest() { return { delivered: false, durable: false }; } });
  f.start(); f.end(message()); await f.drain(); await f.drain();
  assert.deepEqual(facts, [{ type: "usage_gap", reason: "durable-acknowledgment" }]);
  assert.equal(f.owned.usageCoverage, "unavailable-durable-acknowledgment");
});

/** Composes the real request core and ordinary callback owner with separate
 * ordered queues. Only transport replies are injected; this tests retained
 * tickets/latches, not native admission or ledger deduplication authority. */
function callbackRetryFixture(gapDelivered = true, options = {}) {
  const owner = {}; const requests = []; const facts = []; const tickets = [];
  const handlers = new Map(); let wire = Promise.resolve(); let clock = 20;
  let original; let ends = 0; let charges = 0;
  const core = createRequestAccountingClient({
    enqueue(operation) { const next = wire.then(operation); wire = next.catch(() => {}); return next; },
    async exchange(method, payload) {
      requests.push({ method, payload: structuredClone(payload) });
      if (method.endsWith("usage-begin")) {
        original ??= payload.request_id;
        return { protocol: "external-request/1", accepted: true, request_id: payload.request_id,
          admitted_at: 10, source_witness: "c".repeat(64), origin: "unattributed", controls: [] };
      }
      if (++ends === 1) charges++;
      if (ends <= 2) return undefined;
      return { accepted: true, durable: true, applied: payload.request_id !== original,
        revision: payload.request_id === original ? 1 : 2,
        coverage: { source: "external-reported", reasoning_known: false } };
    },
  });
  const client = { ...core, async start() { return true; }, detach() {},
    async piObservation(fact) { facts.push({ ...fact }); return { delivered: fact.type !== "usage_gap" || gapDelivered }; },
    endRequest(ticket, report) { tickets.push(ticket); return core.endRequest(ticket, report); } };
  registerOrdinaryPiObserver({ on(type, callback) { handlers.set(type, callback); } }, {
    owner, createClient: () => client, peerHelper: "/fixture/mez", now: () => clock,
    env: { MEZ: "/tmp/fixture.sock\x1fsession=a\x1fwindow=@1\x1fpane=%1\x1fprotocol=mez-control/1", MEZ_PANE: "%1" },
    ...options,
  });
  return { owner, requests, facts, tickets, charges: () => charges, clock(value) { clock = value; },
    emit(value) {
      const before = JSON.stringify(value);
      assert.equal(handlers.get(value.type)?.(value, { sessionManager: { getSessionId: () => "session-a" } }), undefined);
      assert.equal(JSON.stringify(value), before);
    },
    async drain() {
      for (let remaining = 32; remaining > 0; remaining--) {
        const tail = owner.tail; await tail;
        if (tail === owner.tail) return;
      }
      assert.fail("ordinary retry callback queue did not settle");
    },
  };
}

/** Deterministic telemetry-owner time is independent of incurred report UTC.
 * Timer handles model cancellation and unref without keeping Node alive. The
 * fixture advances one due callback at a time so observer queue drains remain
 * explicit; it supplies no admission, accounting or native ownership rights. */
function retrySchedulerFixture() {
  let clock = 0; let next = 0; const pending = new Map();
  const scheduler = {
    now: () => clock,
    setTimeout(callback, delay) {
      const handle = { id: ++next, at: clock + delay, callback, unreferenced: false,
        unref() { this.unreferenced = true; } };
      pending.set(handle.id, handle); return handle;
    },
    clearTimeout(handle) { pending.delete(handle.id); },
  };
  return { scheduler, pending,
    advance(value) {
      clock = value;
      const due = [...pending.values()].filter(handle => handle.at <= clock)
        .sort((a, b) => a.at - b.at || a.id - b.id)[0];
      if (due) { pending.delete(due.id); due.callback(); }
    },
  };
}

/** Desired recovery uses the SAME ordinary owner and real RequestCore after
 * two actual missing transport ACKs, without another SDK/vendor callback. The
 * transport fixture models a single committed observation and an inert replay
 * acknowledgment; it is explicitly not proof of native admission or SQLite
 * deduplication. Scheduler time cannot rewrite the original reporting UTC. */
test("ordinary finite scheduler recovers retained report without another vendor callback", async () => {
  const timer = retrySchedulerFixture();
  const f = callbackRetryFixture(true, { retryScheduler: timer.scheduler });
  f.emit({ type: "session_start", reason: "startup" }); f.emit({ type: "agent_start" });
  f.emit({ type: "turn_start", turnIndex: 0 });
  const original = message(); f.emit({ type: "message_end", message: original }); f.emit(event(original));
  await f.drain();
  const initial = f.requests.filter(row => row.method.endsWith("usage-end"));
  assert.equal(initial.length, 2);
  assert.equal(JSON.stringify(initial[0].payload) === JSON.stringify(initial[1].payload), true);
  assert.equal(f.owner.lease.failed, false);
  assert.equal(f.owner.lease.usageCoverage, "unavailable-durable-acknowledgment");
  assert.equal(f.owner.lease.lastUsageGap, "durable-acknowledgment");
  f.clock(99); timer.advance(250); await f.drain();
  const recovered = f.requests.filter(row => row.method.endsWith("usage-end"));
  assert.equal(recovered.length, 3, "finite telemetry scheduler did not retry retained report");
  assert.equal(JSON.stringify(recovered[2].payload) === JSON.stringify(initial[0].payload), true);
  assert.equal(recovered[2].payload.observed_at, 20);
  assert.equal(f.tickets[1] === f.tickets[0], true);
  const begins = f.requests.filter(row => row.method.endsWith("usage-begin"));
  assert.equal(begins.length, 4);
  assert.equal(begins.every(row => row.payload.request_id === begins[0].payload.request_id), true);
  assert.equal(f.charges(), 1);
  assert.equal(f.owner.lease.failed, false);
  assert.equal(f.owner.lease.usageCoverage, "finalized-assistant-external-reported");
  assert.equal(f.owner.lease.lastUsageGap, "durable-acknowledgment");
  assert.deepEqual(f.facts.filter(fact => fact.type === "usage_gap"), [{ type: "usage_gap", reason: "durable-acknowledgment" }]);
  assert.equal(timer.pending.size, 0);
  const count = f.requests.length; timer.advance(10000); await f.drain();
  assert.equal(f.requests.length, count);
});

/** Two immediate ACK losses exhaust one real endRequest operation. A later
 * exact finalized callback retries its same instance-local ticket and frozen
 * report, not a provider request, new marker, timestamp or copied ticket. The
 * truthful epoch loss fact remains recorded even after a durable replay ACK. */
test("ordinary exact finalized callback retries the real core after two lost ACKs", async () => {
  const f = callbackRetryFixture();
  f.emit({ type: "session_start", reason: "startup" });
  f.emit({ type: "agent_start" });
  f.emit({ type: "turn_start", turnIndex: 0 });
  const original = message(); f.emit({ type: "message_end", message: original });
  f.emit(event(original)); await f.drain();
  let begins = f.requests.filter(row => row.method.endsWith("usage-begin"));
  let ends = f.requests.filter(row => row.method.endsWith("usage-end"));
  assert.equal(begins.length, 3); assert.equal(ends.length, 2);
  assert.deepEqual(ends[0].payload, ends[1].payload);
  assert.equal(ends[0].payload.observed_at, 20);
  assert.equal(Object.isFrozen(f.tickets[0]), true);
  assert.equal(f.owner.lease.failed, false);
  assert.equal(f.owner.lease.usageCoverage, "unavailable-durable-acknowledgment");
  assert.deepEqual(f.facts.filter(fact => fact.type === "usage_gap"), [{ type: "usage_gap", reason: "durable-acknowledgment" }]);
  f.clock(99); f.emit(event(message())); await f.drain();
  begins = f.requests.filter(row => row.method.endsWith("usage-begin"));
  ends = f.requests.filter(row => row.method.endsWith("usage-end"));
  assert.equal(begins.length, 4); assert.equal(ends.length, 3);
  assert.deepEqual(ends[2].payload, ends[0].payload);
  assert.equal(f.tickets[1], f.tickets[0]);
  assert.equal(begins.every(row => row.payload.request_id === begins[0].payload.request_id), true);
  assert.equal(f.owner.lease.failed, false);
  assert.equal(f.owner.lease.usageCoverage, "finalized-assistant-external-reported");
  assert.equal(f.facts.filter(fact => fact.type === "usage_gap").length, 1);
  const known = f.requests.length;
  f.emit(event(message())); await f.drain();
  assert.equal(f.requests.length, known, "known durable callback replay started new transport work");
  f.emit({ type: "agent_start" }); f.emit({ type: "turn_start", turnIndex: 0 });
  const next = message(); f.emit({ type: "message_end", message: next }); f.emit(event(next, "entry-b"));
  await f.drain();
  ends = f.requests.filter(row => row.method.endsWith("usage-end"));
  assert.equal(ends.length, 4);
  assert.notEqual(ends[3].payload.request_id, ends[0].payload.request_id);
  assert.notEqual(ends[3].payload.event_id, ends[0].payload.event_id);
  assert.equal(ends[3].payload.observed_at, 99);
});

/** A failed truthful gap delivery latches the ordinary owner. Neither exact
 * retry, changed or historical callbacks, nor a genuine subsequent turn can
 * clear it or initiate reporting; the vendor callback return stays neutral. */
test("failed gap after two ACK losses suppresses all later ordinary request work", async () => {
  const f = callbackRetryFixture(false);
  f.emit({ type: "session_start", reason: "startup" }); f.emit({ type: "agent_start" });
  f.emit({ type: "turn_start", turnIndex: 0 });
  const original = message(); f.emit({ type: "message_end", message: original }); f.emit(event(original));
  await f.drain();
  assert.equal(f.owner.lease.failed, true);
  assert.equal(f.owner.lease.usageCoverage, "unavailable-durable-acknowledgment");
  const count = f.requests.length; const facts = f.facts.length;
  f.emit(event(message()));
  const changed = message(); changed.usage.input = 8; f.emit(event(changed));
  f.emit(event(message(), "historical", 99));
  f.emit({ type: "agent_start" }); f.emit({ type: "turn_start", turnIndex: 1 });
  const next = message(); f.emit({ type: "message_end", message: next }); f.emit(event(next, "entry-b", 1));
  await f.drain();
  assert.equal(f.requests.length, count); assert.equal(f.facts.length, facts);
  assert.equal(f.owner.lease.failed, true);
  assert.equal(f.requests.filter(row => row.method.endsWith("usage-end")).length, 2);
});

/** Healthy owner status is not permission to borrow an old expense slot for a
 * historical ID or changed finalized counters. Such callbacks disclose their
 * exact fixed diagnostic, leave the original undurable report unchanged, and
 * cannot create another end operation or readiness marker. */
test("historical and changed callbacks cannot replace the undurable real-core report", async () => {
  for (const historical of [false, true]) {
    const f = callbackRetryFixture();
    f.emit({ type: "session_start", reason: "startup" }); f.emit({ type: "agent_start" });
    f.emit({ type: "turn_start", turnIndex: 0 });
    const original = message(); f.emit({ type: "message_end", message: original }); f.emit(event(original));
    await f.drain(); const count = f.requests.length;
    const rejected = message(); if (!historical) rejected.usage.input = 8;
    f.emit(event(rejected, historical ? "historical" : "entry-a", historical ? 99 : 0));
    await f.drain();
    assert.equal(f.requests.length, count);
    assert.equal(f.requests.filter(row => row.method.endsWith("usage-end")).length, 2);
    assert.equal(f.facts.at(-1).reason, historical ? "unmatched-finalized-event" : "immutable-observation-conflict");
    assert.equal(f.owner.lease.failed, false);
  }
});
