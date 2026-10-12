/** Unchanged installed Claude module through genuine ordinary native helpers.
 * Timer callbacks are explicitly driven for deterministic terminal-rejection
 * coverage, not advertised as real SDK/wall-clock idle evidence. The separately
 * owned gate holds this normal Node creator alive after telemetry retirement;
 * no PID, token, initial receipt, vendor content or provider work is injected. */
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const [directory] = process.argv.slice(2);
const execute = promisify(execFile);
const calls = [];
const timers = [];
const replies = [];
const sdk = {
  process: { run: async (argv, options = {}) => {
    calls.push(argv);
    const execution = execute(argv[0], argv.slice(1), {
      env: process.env, timeout: options.timeoutMs, maxBuffer: 8192,
    });
    execution.child.stdin.end(options.stdin ?? "");
    const output = await execution;
    assert.equal(output.stderr, "");
    replies.push(JSON.parse(output.stdout));
    return { exitCode: 0, stdout: output.stdout, stderr: output.stderr };
  } },
  clock: { every: (delay, callback) => {
    assert.equal(delay, 10000);
    const timer = { callback, cancelled: 0, cancel() { this.cancelled++; } };
    timers.push(timer);
    return timer;
  } },
};

/** Bounded owned test gates never create telemetry authority or alter selectors. */
async function gate(name) {
  const deadline = performance.now() + 10000;
  while (!existsSync(join(directory, name))) {
    assert(performance.now() < deadline, `owned ${name} gate timed out`);
    await new Promise(resolve => setTimeout(resolve, 5));
  }
}

const hooks = new Map();
const { register } = await import(pathToFileURL(join(directory,
  "home/.claude/skills/mezzanine-telemetry/hooks/register.mjs")));
register((name, handler) => { assert(!hooks.has(name)); hooks.set(name, handler); });
const result = Object.freeze({ vendor: "unchanged" });
const start = { hook_event_name: "SessionStart", session_id: "session-a", source: "startup" };
assert.equal(await hooks.get("classic.SessionStart")(sdk, start, async () => result), result);
assert.equal(timers.length, 1);
assert.equal(calls.length, 2); // enrollment then separately ordered lifecycle
const original = Object.freeze(replies[0]);
assert.equal(original.registered, true);
assert.deepEqual(original.controls, []);
assert.equal(original.observer_epoch, 1);
assert.equal(original.observer_instance, "mez-curated-client-1");
assert(!("launch_token" in original));
writeFileSync(join(directory, "callback-returned"), "returned");
await timers[0].callback();
assert.deepEqual(replies.at(-1), { observed: true, sequence: 1, changed: true });
assert.equal(timers[0].cancelled, 0);
writeFileSync(join(directory, "positive-proof"), JSON.stringify(original));
await gate("retired-while-alive");
await timers[0].callback();
assert.deepEqual(replies.at(-1), { observed: false, terminal: true });
assert.equal(timers[0].cancelled, 1);
const terminal = JSON.parse(calls.at(-1)[2]);
assert.deepEqual(terminal, { operation: "curated-heartbeat", external_session_id: original.external_session_id,
  generation: original.generation, observer_witness: original.observer_witness, sequence: 2 });
const count = calls.length;
await timers[0].callback();
await timers[0].callback();
assert.equal(await hooks.get("classic.SessionStart")(sdk, start, async () => result), result);
assert.equal(await hooks.get("classic.UserPromptSubmit")(sdk,
  { hook_event_name: "UserPromptSubmit", session_id: "session-a" }, async () => result), result);
assert.equal(calls.length, count);
assert.equal(timers.length, 1);
assert.equal(calls.filter(argv => argv[1] === "harness-source" && !JSON.parse(argv[2]).operation).length, 1);
writeFileSync(join(directory, "local-closed"), "closed");
await gate("exit-producer");
console.log('{"qualified":true}');
