/** Deterministic curated SDK contract tests. No vendor, socket or user config.
 * The same compiled source body is wrapped after next(e); timer callbacks are
 * driven explicitly so failed transport, overlap and frozen epoch are testable. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const template = readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/curated_client.mjs", import.meta.url), "utf8");
const source = template.replaceAll("__MEZ_INTERVAL__", "10000").replaceAll("__MEZ_HELPER__", JSON.stringify("/owned/mez"));
const handler = new (Object.getPrototypeOf(async function () {}).constructor)("$", "e", "next", `const result = await next(e); ${source}\nreturn result;`);
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
