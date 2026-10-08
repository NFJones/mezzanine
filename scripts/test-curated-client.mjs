/** Deterministic curated SDK contract tests. No vendor, socket or user config.
 * The same compiled source body is wrapped after next(e); timer callbacks are
 * driven explicitly so failed transport, overlap and frozen epoch are testable. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createCuratedLifetime } from "../crates/mezzanine/src/integrations/bootstrap/curated_lifetime.mjs";

const template = readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/curated_client.mjs", import.meta.url), "utf8");
const render = (instance, replacing, owned = false) => template.replace(/__MEZ_[A-Z]+__/g, marker => ({
  __MEZ_OWNED__: owned ? "true" : "false", __MEZ_LIFETIME__: owned ? "observerLifetime" : "undefined",
  __MEZ_INTERVAL__: "10000", __MEZ_HELPER__: JSON.stringify("/owned/mez"),
  __MEZ_INSTANCE__: JSON.stringify(instance), __MEZ_REPLACING__: replacing ? "true" : "false",
  __MEZ_PREDECESSOR__: replacing ? "previousObserver" : "undefined",
})[marker]);
const source = render("mez-curated-client-1", false);
const handler = new (Object.getPrototypeOf(async function () {}).constructor)("$", "e", "next", `const result = await next(e); ${source}\nreturn result;`);
const successorHandler = new (Object.getPrototypeOf(async function () {}).constructor)("$", "e", "next", "previousObserver", `const result = await next(e); ${render("module-b", true)}\nreturn result;`);
const ownedHandler = new (Object.getPrototypeOf(async function () {}).constructor)("$", "e", "next", "observerLifetime", `const result = await next(e); ${render("mez-curated-client-1", false, true)}\nreturn result;`);
const event = () => ({ session_id: "session-a", source: "startup", prompt: "private-prompt", transcript_path: "/private/transcript" });
const registration = () => ({ protocol: "external-agent/1", registered: true, controls: [],
  agent_id: "external-a", generation: 7, observer_witness: "a".repeat(64), run_id: 1,
  observer_epoch: 1, observer_instance: "mez-curated-client-1", external_session_id: "session-a",
  usage: "unavailable-source-continuity", observer_transport: "unavailable-curated-freshness",
  expires_at_unix_seconds: 123, lease_seconds: 60 });
const output = value => ({ exitCode: 0, stdout: JSON.stringify(value), stderr: "private-error" });

/** The SDK surface captures only fixed argv and one explicit timer. */
function fixture(first = registration(), proof) {
  const calls = [];
  const timers = [];
  const sdk = {
    process: { run: async (argv, options) => {
      const capsule = JSON.parse(argv[2]);
      calls.push({ argv, options, capsule });
      if (!capsule.operation) return output(first);
      return proof ? proof(capsule) : output({ observed: true, sequence: capsule.sequence, changed: true });
    } },
    clock: { every: (delay, callback) => { timers.push({ delay, callback }); return { cancel() {} }; } },
  };
  return { calls, timers, sdk };
}

/** Owned mode cannot degrade to unowned admission when its scope is missing.
 * Downstream vendor rejection still propagates unchanged before telemetry. */
test("owned curated source requires its owner and preserves downstream errors", async () => {
  const f = fixture();
  const result = {};
  assert.equal(await ownedHandler(f.sdk, event(), async () => result, undefined), result);
  assert.equal(f.calls.length, 0);
  const error = new Error("vendor error");
  await assert.rejects(ownedHandler(f.sdk, event(), async () => { throw error; }, createCuratedLifetime()), caught => caught === error);
  assert.equal(f.calls.length, 0);
});

/** Caller-retained ownership blocks duplicate admission and guards queued timer
 * callbacks after stop, preserving each middleware result without extra I/O. */
test("owned curated source bounds duplicate admission and stopped callbacks", async () => {
  const owner = createCuratedLifetime();
  const f = fixture();
  let cancelled = 0;
  f.sdk.clock.every = (delay, callback) => { f.timers.push({ delay, callback }); return { cancel() { cancelled++; } }; };
  const result = {};
  assert.equal(await ownedHandler(f.sdk, event(), async () => result, owner), result);
  assert.equal(await ownedHandler(f.sdk, event(), async () => result, owner), result);
  assert.equal(f.calls.length, 1);
  assert.equal(f.timers.length, 1);
  owner.stop();
  await f.timers[0].callback();
  assert.equal(f.calls.length, 1);
  assert.equal(cancelled, 1);
  assert.equal(owner.receipt, undefined);
});

/** Stopping while helper response is withheld prevents valid late registration
 * from acquiring a timer or reviving local state; native retirement is separate. */
test("owned curated source rejects late admission after stop", async () => {
  const owner = createCuratedLifetime();
  const f = fixture();
  const run = f.sdk.process.run;
  let release;
  f.sdk.process.run = (argv, options) => new Promise(resolve => { release = () => run(argv, options).then(resolve); });
  const result = {};
  const pending = ownedHandler(f.sdk, event(), async () => result, owner);
  await Promise.resolve();
  owner.stop();
  release();
  assert.equal(await pending, result);
  assert.equal(f.timers.length, 0);
  assert.equal(owner.receipt, undefined);
  await ownedHandler(f.sdk, event(), async () => result, owner);
  assert.equal(f.calls.length, 1);
});

/** SDK timer loss/late attachment cannot leave helper-launch authority behind.
 * A lost handle may be impossible to cancel, but queued callbacks remain inert. */
test("owned curated source stops after timer creation or attachment failure", async () => {
  for (const mode of ["throw", "late", "invalid"]) {
    const owner = createCuratedLifetime();
    const f = fixture();
    let callback;
    let cancelled = 0;
    f.sdk.clock.every = (_delay, fn) => {
      callback = fn;
      if (mode === "throw") throw new Error("unavailable");
      if (mode === "late") owner.stop();
      return mode === "invalid" ? {} : { cancel() { cancelled++; } };
    };
    const result = {};
    assert.equal(await ownedHandler(f.sdk, event(), async () => result, owner), result);
    await callback();
    assert.equal(f.calls.length, 1);
    assert.equal(owner.receipt, undefined);
    assert.equal(cancelled, mode === "late" ? 1 : 0);
  }
});

/** Handoff requires the actual prior public identity and a matching same-run
 * successor receipt. Old and new callbacks hold disjoint immutable selectors,
 * independent sequence owners, and preserve the exact vendor middleware result. */
test("curated successor pins its new epoch without adopting mutable prior state", async () => {
  const prior = registration();
  const old = fixture(prior);
  const result = {};
  await handler(old.sdk, event(), async () => result);
  await old.timers[0].callback();
  const receipt = { ...registration(), generation: 11, observer_witness: "b".repeat(64), observer_epoch: 2, observer_instance: "module-b" };
  const fresh = fixture(receipt);
  assert.equal(await successorHandler(fresh.sdk, event(), async () => result, prior), result);
  assert.deepEqual(fresh.calls[0].capsule, { external_session_id: "session-a", observer_instance: "module-b", session_boundary: "startup", predecessor_generation: 7 });
  prior.generation = 99;
  receipt.generation = 100;
  await old.timers[0].callback();
  await fresh.timers[0].callback();
  assert.equal(old.calls.at(-1).capsule.generation, 7);
  assert.equal(old.calls.at(-1).capsule.sequence, 2);
  assert.deepEqual(fresh.calls.at(-1).capsule, { operation: "curated-heartbeat", external_session_id: "session-a", generation: 11, observer_witness: "b".repeat(64), sequence: 1 });
});

/** A suspended helper cannot let mutable caller metadata change the predecessor
 * or matching receipt checks after capture. This exercises the exact before-
 * await snapshot rather than only mutations after successful initialization. */
test("curated successor freezes prior and event metadata before awaiting transport", async () => {
  const prior = registration();
  const e = event();
  const receipt = { ...registration(), generation: 11, observer_witness: "b".repeat(64), observer_epoch: 2, observer_instance: "module-b" };
  const f = fixture(receipt);
  const run = f.sdk.process.run;
  let release;
  f.sdk.process.run = (argv, options) => JSON.parse(argv[2]).operation ? run(argv, options)
    : new Promise(resolve => { release = () => run(argv, options).then(resolve); });
  const result = {};
  const pending = successorHandler(f.sdk, e, async () => result, prior);
  await Promise.resolve();
  assert.equal(typeof release, "function");
  Object.assign(prior, { generation: 99, run_id: 99, observer_epoch: 99, agent_id: "foreign",
    external_session_id: "foreign", observer_witness: "c".repeat(64) });
  Object.assign(e, { session_id: "foreign", source: "compact" });
  release();
  assert.equal(await pending, result);
  assert.equal(f.calls[0].capsule.predecessor_generation, 7);
  assert.equal(f.calls[0].capsule.external_session_id, "session-a");
  assert.equal(f.timers.length, 1);
  await f.timers[0].callback();
  assert.equal(f.calls.at(-1).capsule.generation, 11);
});

/** Lost or malformed prior evidence cannot silently become an initial capsule.
 * A returned incompatible run/agent/epoch or stale witness cannot start a proof
 * timer, even if transport/neutral callback execution itself reports success. */
test("curated successor rejects unavailable prior or incompatible handoff receipts", async () => {
  const invalid = [undefined, null, {}, { ...registration(), external_session_id: "foreign" },
    { ...registration(), generation: 0 }, { ...registration(), generation: 9007199254740992 },
    { ...registration(), observer_epoch: Number.MAX_SAFE_INTEGER }, { ...registration(), observer_witness: "bad" }];
  for (const prior of invalid) {
    const f = fixture();
    const result = {};
    assert.equal(await successorHandler(f.sdk, event(), async () => result, prior), result);
    assert.equal(f.calls.length, 0);
  }
  const receipt = { ...registration(), generation: 11, observer_witness: "b".repeat(64), observer_epoch: 2, observer_instance: "module-b" };
  for (const [key, value] of [["generation", 7], ["observer_witness", "a".repeat(64)],
    ["run_id", 2], ["agent_id", "foreign-agent"], ["observer_epoch", 3]]) {
    const f = fixture({ ...receipt, [key]: value });
    await successorHandler(f.sdk, event(), async () => undefined, registration());
    assert.equal(f.calls.length, 1);
    assert.equal(f.timers.length, 0);
  }
});

/** Successful callback and repeated timers preserve the vendor object exactly,
 * project no prompt/path/error fields, and keep immutable original selectors. */
test("curated client preserves next and projects only original epoch metadata", async () => {
  const receipt = registration();
  const f = fixture(receipt);
  const e = event();
  const result = { vendor: "unchanged" };
  let nextCalls = 0;
  assert.equal(await handler(f.sdk, e, async passed => { assert.equal(passed, e); nextCalls++; return result; }), result);
  assert.equal(nextCalls, 1);
  assert.equal(f.timers.length, 1);
  assert.equal(f.timers[0].delay, 10000);
  e.session_id = "replacement";
  receipt.generation = 99;
  for (let sequence = 1; sequence <= 3; sequence++) {
    await f.timers[0].callback();
    assert.deepEqual(f.calls.at(-1).capsule, { operation: "curated-heartbeat",
      external_session_id: "session-a", generation: 7, observer_witness: "a".repeat(64), sequence });
  }
  for (const call of f.calls) {
    assert.deepEqual(call.argv.slice(0, 2), ["/owned/mez", "harness-source"]);
    assert.deepEqual(call.options, { timeoutMs: 3000 });
    assert(!call.argv[2].includes("private"));
  }
});

/** Timer overlap cannot allocate a queue or concurrent helper. Once the one
 * helper settles, later timer cadence uses a new sequence, never re-enrollment. */
test("curated client bounds concurrent helpers and survives unavailable proof", async () => {
  let finish;
  const f = fixture(registration(), () => new Promise(resolve => { finish = resolve; }));
  await handler(f.sdk, event(), async () => undefined);
  const pending = f.timers[0].callback();
  await f.timers[0].callback();
  assert.equal(f.calls.length, 2);
  finish(output({ observed: false }));
  await pending;
  const second = f.timers[0].callback();
  assert.equal(f.calls.at(-1).capsule.sequence, 2);
  finish(output({ observed: true, sequence: 2, changed: false }));
  await second;
  assert.equal(f.calls.filter(call => !call.capsule.operation).length, 1);
});

/** Missing, private, foreign, malformed and unsafe receipts never activate a
 * timer. Fail-open observation does not invent a zero counter or control role. */
test("curated client rejects unsupported registration receipts neutrally", async () => {
  const invalid = [null, [], { registered: false }];
  for (const [key, value] of [
    ["generation", 0], ["generation", 9007199254740992], ["external_session_id", "foreign"],
    ["observer_instance", "foreign"], ["controls", ["input"]], ["lease_seconds", 99],
    ["launch_token", "private"], ["usage", { prompt: "private" }], ["observer_witness", "invalid"],
  ]) invalid.push({ ...registration(), [key]: value });
  for (const receipt of invalid) {
    const f = fixture(receipt);
    const result = {};
    assert.equal(await handler(f.sdk, event(), async () => result), result);
    assert.equal(f.timers.length, 0);
  }
});

/** Vendor errors retain exact identity and prevent telemetry. SDK policy errors,
 * malformed helper output and rejected timers cannot change successful next. */
test("curated client preserves vendor errors and neutralizes SDK failures", async () => {
  const f = fixture();
  const error = new Error("vendor error");
  await assert.rejects(handler(f.sdk, event(), async () => { throw error; }), caught => caught === error);
  assert.equal(f.calls.length, 0);
  const result = {};
  for (const response of [{ exitCode: 1, stdout: "" }, { exitCode: 0, stdout: "not-json" },
    { exitCode: 0, stdout: " ".repeat(4097) }]) {
    const sdk = { process: { run: async () => response } };
    assert.equal(await handler(sdk, event(), async () => result), result);
  }
  assert.equal(await handler({ process: { run: async () => { throw error; } } }, event(), async () => result), result);
  const rejected = fixture();
  rejected.sdk.clock.every = () => { throw error; };
  assert.equal(await handler(rejected.sdk, event(), async () => result), result);
});

/** Unsupported boundary/content-shaped IDs produce no helper call. Proof errors
 * release the busy owner so the next timer remains live without replaying work. */
test("curated client filters initial metadata and releases failed helpers", async () => {
  for (const e of [{ session_id: "private prompt", source: "startup" },
    { session_id: "session-a\n", source: "startup" },
    { session_id: "session-a", source: "compact" }, { session_id: "x".repeat(129), source: "startup" }]) {
    const f = fixture();
    await handler(f.sdk, e, async () => undefined);
    assert.equal(f.calls.length, 0);
  }
  const f = fixture(registration(), async () => { throw new Error("unavailable"); });
  await handler(f.sdk, event(), async () => undefined);
  await f.timers[0].callback();
  await f.timers[0].callback();
  assert.deepEqual(f.calls.slice(1).map(call => call.capsule.sequence), [1, 2]);
});

/** Only this lexical test seam seeds the otherwise private counter near its
 * exact JS-safe bound. The final valid proof is sent once; further cadence
 * allocates no helper, queue, replacement owner or unsafe rounded sequence. */
test("curated client stops allocating proofs at JS-safe sequence exhaustion", async () => {
  assert.equal(source.split("let sequence = 0;").length, 2);
  const bounded = source.replace("let sequence = 0;", "let sequence = 9007199254740990;");
  const exhausted = new (Object.getPrototypeOf(async function () {}).constructor)("$", "e", "next", `const result = await next(e); ${bounded}\nreturn result;`);
  const f = fixture();
  await exhausted(f.sdk, event(), async () => undefined);
  await f.timers[0].callback();
  assert.equal(f.calls.at(-1).capsule.sequence, Number.MAX_SAFE_INTEGER);
  await f.timers[0].callback();
  await f.timers[0].callback();
  assert.equal(f.calls.length, 2);
});
