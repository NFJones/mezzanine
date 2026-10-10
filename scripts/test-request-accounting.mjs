/** Pure offline request-client contract regressions. Transport is injected:
 * no vendors, providers, credentials, socket authority or user config is used.
 * Real native admission remains covered by the installed-client Rust fixture. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { createRequestAccountingClient } from "../crates/mezzanine/src/integrations/bootstrap/request_accounting.mjs";

const marker = "a".repeat(64);
const report = () => ({ event_id: "b".repeat(64), observed_at: 101, provider: "fixture", model: "model",
  counters: { input_tokens: 10, output_tokens: 3, cached_input_tokens: 2 } });
const ready = id => ({ protocol: "external-request/1", accepted: true, request_id: id,
  admitted_at: 100, source_witness: "c".repeat(64), origin: "unattributed", controls: [] });
const committed = applied => ({ accepted: true, durable: true, applied, revision: 1,
  coverage: { source: "external-reported", reasoning_known: false } });

/** The injected transport core must run in a restricted SDK realm with neither
 * Node Buffer nor DOM TextEncoder. Exact UTF-8 limits remain byte limits, not
 * code-unit/grapheme limits; oversized identifiers never reach transport. */
test("request accounting needs no Node or DOM globals and preserves UTF-8 bounds", async () => {
  const source = readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/request_accounting.mjs", import.meta.url), "utf8");
  const create = runInNewContext(source.replace("export function createRequestAccountingClient", "function createRequestAccountingClient")
    + "\nif (typeof Buffer !== 'undefined' || typeof TextEncoder !== 'undefined') throw new Error('fixture globals'); createRequestAccountingClient;", {});
  for (const [model, allowed] of [["m".repeat(128), true], ["m".repeat(129), false],
    ["é".repeat(64), true], ["é".repeat(65), false], ["界".repeat(42) + "ab", true],
    ["界".repeat(43), false], ["😀".repeat(32), true], ["😀".repeat(33), false]]) {
    const requests = [];
    const client = create({ enqueue: operation => Promise.resolve().then(operation),
      exchange: async (method, params) => {
        requests.push({ method, params });
        return method.endsWith("begin") ? ready(params.request_id) : committed(true);
      } });
    const ticket = await client.beginRequest(marker);
    const result = await client.endRequest(ticket, { ...report(), model });
    assert.equal(result.durable, allowed, `UTF-8 bound for ${model.length} code units`);
    assert.equal(requests.filter(request => request.method.endsWith("end")).length, allowed ? 1 : 0);
  }
});

/** Retains the real serialized ordering boundary without performing I/O. */
function fixture(respond) {
  let serial = Promise.resolve();
  const requests = [];
  const client = createRequestAccountingClient({
    enqueue(operation) { const next = serial.then(operation); serial = next.catch(() => {}); return next; },
    async exchange(method, params) { requests.push({ method, params }); return respond(method, params); },
  });
  return { client, requests };
}

test("durable ACK only; lost reply retries one immutable report without provider work", async () => {
  let endings = 0;
  const f = fixture((method, params) => method.endsWith("begin") ? ready(params.request_id)
    : ++endings === 1 ? undefined : committed(false));
  const ticket = await f.client.beginRequest(marker);
  assert(Object.isFrozen(ticket));
  const input = report();
  const promise = f.client.endRequest(ticket, input);
  input.counters.input_tokens = 999;
  const result = await promise;
  assert.equal(result.durable, true);
  assert.equal(result.applied, false);
  const reports = f.requests.filter(request => request.method.endsWith("end"));
  assert.equal(reports.length, 2);
  assert.deepEqual(reports[0].params, reports[1].params);
  assert.equal(reports[0].params.counters.input_tokens, 10);
  assert.equal(f.client.accountingStatus(), "request-bound-external-reported");
  const count = f.requests.length;
  assert.equal((await f.client.endRequest(ticket, report())).durable, true);
  assert.equal(f.requests.length, count, "known durable retry started new work");
  const changed = report(); changed.counters.input_tokens = 11;
  assert.equal((await f.client.endRequest(ticket, changed)).reason, "immutable-report-conflict");
});

test("accepted/neutral response is not durable evidence; failures remain bounded", async () => {
  const f = fixture((method, params) => method.endsWith("begin") ? ready(params.request_id) : { accepted: true });
  const ticket = await f.client.beginRequest(marker);
  assert.equal((await f.client.endRequest(ticket, report())).durable, false);
  assert.equal(f.requests.filter(request => request.method.endsWith("end")).length, 2);
  assert.equal(f.client.accountingStatus(), "unavailable-source-continuity");
});

test("source namespace change rejects even with coincident admitted timestamp", async () => {
  let begins = 0;
  const f = fixture((method, params) => method.endsWith("begin")
    ? { ...ready(params.request_id), source_witness: (++begins === 1 ? "c" : "d").repeat(64) }
    : committed(true));
  const ticket = await f.client.beginRequest(marker);
  assert.equal((await f.client.endRequest(ticket, report())).reason, "request-source-changed");
  assert.equal(f.requests.filter(request => request.method.endsWith("end")).length, 0);
});

test("forged/copied/successor tickets and private callback fields never cross transport", async () => {
  const f = fixture((method, params) => method.endsWith("begin") ? ready(params.request_id) : committed(true));
  const ticket = await f.client.beginRequest(marker);
  const count = f.requests.length;
  assert.equal((await f.client.endRequest({ ...ticket }, report())).durable, false);
  const successor = fixture(() => committed(true));
  assert.equal((await successor.client.endRequest(ticket, report())).durable, false);
  for (const bad of [{ ...report(), prompt: "PRIVATE" }, { ...report(), counters: { ...report().counters, tool: "PRIVATE" } },
    { ...report(), observed_at: 99 }, { ...report(), counters: { input_tokens: -1, output_tokens: 0 } },
    { ...report(), counters: { input_tokens: 1, output_tokens: 1, cached_input_tokens: 2 } }]) {
    assert.equal((await f.client.endRequest(ticket, bad)).durable, false);
  }
  assert.equal(f.requests.length, count);
});

test("callback getters are captured once before validation and transport", async () => {
  const f = fixture((method, params) => method.endsWith("begin") ? ready(params.request_id) : committed(true));
  const ticket = await f.client.beginRequest(marker);
  const input = report();
  let modelReads = 0;
  let inputReads = 0;
  Object.defineProperty(input, "model", { enumerable: true, get() { return ++modelReads === 1 ? "model" : "PRIVATE\ncontent"; } });
  Object.defineProperty(input.counters, "input_tokens", { enumerable: true, get() { return ++inputReads === 1 ? 10 : -1; } });
  assert.equal((await f.client.endRequest(ticket, input)).durable, true);
  const wire = f.requests.find(request => request.method.endsWith("end")).params;
  assert.equal(wire.model, "model");
  assert.equal(wire.counters.input_tokens, 10);
  assert.equal(modelReads, 1);
  assert.equal(inputReads, 1);
});

test("queued abandonment is terminal and never sends or resurrects reporting", async () => {
  const f = fixture((method, params) => method.endsWith("begin") ? ready(params.request_id) : committed(true));
  const ticket = await f.client.beginRequest(marker);
  const pending = f.client.endRequest(ticket, report());
  assert.equal(f.client.abandonRequest(ticket).reason, "unreported-request-abandoned");
  assert.equal((await pending).durable, false);
  assert.equal(f.requests.filter(request => request.method.endsWith("end")).length, 0);
  assert.equal((await f.client.endRequest(ticket, report())).durable, false);
});

test("abandon during marker restoration prevents the later report send", async () => {
  let begins = 0, started, release;
  const entered = new Promise(resolve => { started = resolve; });
  const held = new Promise(resolve => { release = resolve; });
  const f = fixture((method, params) => {
    if (method.endsWith("begin")) { if (++begins === 1) return ready(params.request_id); started(); return held; }
    return committed(true);
  });
  const ticket = await f.client.beginRequest(marker);
  const pending = f.client.endRequest(ticket, report());
  await entered;
  assert.equal(f.client.abandonRequest(ticket).reason, "unreported-request-abandoned");
  release(ready(marker));
  assert.equal((await pending).durable, false);
  assert.equal(f.requests.filter(request => request.method.endsWith("end")).length, 0);
});

test("in-flight reporting cannot be claimed definitely abandoned or unreported", async () => {
  let started, release;
  const entered = new Promise(resolve => { started = resolve; });
  const held = new Promise(resolve => { release = resolve; });
  const f = fixture((method, params) => {
    if (method.endsWith("begin")) return ready(params.request_id);
    started(); return held;
  });
  const ticket = await f.client.beginRequest(marker);
  const pending = f.client.endRequest(ticket, report());
  await entered;
  assert.equal(f.client.abandonRequest(ticket).reason, "request-report-in-flight");
  release(committed(true));
  assert.equal((await pending).durable, true);
});

test("abandoning an unacknowledged attempted report discloses unknown delivery", async () => {
  const f = fixture((method, params) => method.endsWith("begin") ? ready(params.request_id) : undefined);
  const ticket = await f.client.beginRequest(marker);
  assert.equal((await f.client.endRequest(ticket, report())).durable, false);
  assert.equal(f.client.abandonRequest(ticket).reason, "request-delivery-unknown-abandoned");
  assert.equal((await f.client.endRequest(ticket, report())).durable, false);
});

test("finite active tickets refuse rather than evict; explicit abandonment frees capacity", async () => {
  const f = fixture((method, params) => ready(params.request_id));
  const tickets = [];
  for (let index = 0; index < 128; index++) tickets.push(await f.client.beginRequest(index.toString(16).padStart(64, "0")));
  assert(tickets.every(Boolean));
  assert.equal(await f.client.beginRequest("f".repeat(64)), undefined);
  assert.equal(f.requests.length, 128);
  assert.equal(f.client.abandonRequest(tickets[0]).reason, "unreported-request-abandoned");
  assert(await f.client.beginRequest("f".repeat(64)));
});
