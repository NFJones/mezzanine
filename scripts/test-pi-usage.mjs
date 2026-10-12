/** Pi source-contract projection/normalization tests. Synthetic counts only;
 * no transcripts, credentials, vendor execution or provider requests. */
import assert from "node:assert/strict";
import test from "node:test";
import { normalizePiUsage, projectPiUsage, createPiUsageObserver } from "../crates/mezzanine/src/integrations/bootstrap/pi_usage.mjs";
import { createRequestAccountingClient } from "../crates/mezzanine/src/integrations/bootstrap/request_accounting.mjs";

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
