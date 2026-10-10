/** Ordinary SDK-shaped host of the compiled Claude literal module. Synthetic
 * final counters cross real native helper/actor/ledger ownership; no provider
 * is contacted, no fd3 or native PID/capability is injected, and no transcript
 * is read. This is not a live vendor request or installed bootstrap claim. */
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { pathToFileURL } from 'node:url';

const [directory] = process.argv.slice(2);
const { register } = await import(pathToFileURL(`${directory}/register.mjs`));
const exec = promisify(execFile), hooks = new Map();
register((name, handler) => hooks.set(name, handler));
let helpers = 0, cancelled = false;
const sdk = { process: { run: async (argv, options) => {
  helpers++;
  const result = await exec(argv[0], argv.slice(1), { timeout: options.timeoutMs, maxBuffer: 8192 });
  assert.equal(result.stderr, '');
  return { exitCode: 0, stdout: result.stdout };
} }, clock: { every: () => ({ cancel() { cancelled = true; } }) } };
const unchanged = Object.freeze({ unchanged: true });
assert.equal(await hooks.get('classic.SessionStart')(sdk, {
  session_id: 'session-a', hook_event_name: 'SessionStart', source: 'startup',
}, async () => unchanged), unchanged);
assert.equal(helpers, 1);
const chunk = Object.freeze({ kind: 'text', text: 'PRIVATE' });
const final = Object.freeze({ turnId: 'turn-a', index: 0, answer: 'PRIVATE', toolUses: [], stopReason: 'end_turn',
  usage: { model: 'response-model', input_tokens: 10, output_tokens: 3, cache_read_input_tokens: 2, cache_creation_input_tokens: 4 } });
for (let iteration = 0; iteration < 2; iteration++) {
  const event = Object.freeze({ turnId: 'turn-a', index: 0, model: 'selected', messageCount: 2 });
  const stream = hooks.get('turn.step')(sdk, event, async function* (passed) {
    assert.equal(passed, event); yield chunk; return final;
  });
  assert.equal((await stream.next()).value, chunk);
  const completed = await stream.next();
  assert.equal(completed.done, true); assert.equal(completed.value, final);
}
assert.equal(helpers, 7);
const missing = Object.freeze({ ...final, index: 1, usage: null });
const stream = hooks.get('turn.step')(sdk, { turnId: 'turn-a', index: 1 }, async function* () { return missing; });
assert.equal((await stream.next()).value, missing);
assert.equal(helpers, 8);
await hooks.get('classic.SessionEnd')(sdk, { session_id: 'session-a', hook_event_name: 'SessionEnd', reason: 'clear' }, async () => unchanged);
assert.equal(cancelled, true);
process.stdout.write('{"qualified":true}\n');
