/** Installed ordinary SDK-shaped Claude producer with two lost durable replies.
 * Real helpers commit synthetic usage; only public ACK stdout is withheld from
 * the module. A later genuine main request recovers the retained original report
 * without rerunning its delegate. No provider, transcript or private handle. */
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { pathToFileURL } from 'node:url';

const [directory] = process.argv.slice(2);
const { register } = await import(pathToFileURL(`${directory}/register.mjs`));
const exec = promisify(execFile), hooks = new Map(), endings = [], timers = [];
register((name, handler) => hooks.set(name, handler));
let helpers = 0, lost = 0, delegates = 0;
const sdk = { process: { run: async (argv, options) => {
  helpers++;
  const execution = exec(argv[0], argv.slice(1), { timeout: options.timeoutMs, maxBuffer: 8192 });
  execution.child.stdin.end(options.stdin ?? '');
  const result = await execution;
  assert.equal(result.stderr, '');
  if (argv[1] === 'harness-source') {
    const capsule = JSON.parse(argv[2]), receipt = JSON.parse(result.stdout);
    if (capsule.operation === undefined) {
      assert.equal(receipt.registered, true);
      assert(!Object.hasOwn(receipt, 'launch_token'));
    }
    if (capsule.operation === 'curated-usage-end') {
      assert.equal(receipt.durable, true, 'expense really committed before the reply was lost');
      endings.push(capsule);
      if (lost < 2) {
        assert.equal(receipt.applied, lost === 0);
        lost++;
        return { exitCode: 0, stdout: '{}\n' };
      }
      assert.equal(receipt.applied, endings.length !== 3, 'retained replay is inert; new request charges once');
    }
  }
  return { exitCode: 0, stdout: result.stdout };
} }, clock: { every: (_period, callback) => {
  const timer = { callback, cancelled: false, cancel() { this.cancelled = true; } };
  timers.push(timer); return timer;
} } };
const unchanged = Object.freeze({ unchanged: true });
assert.equal(await hooks.get('classic.SessionStart')(sdk, {
  session_id: 'session-a', hook_event_name: 'SessionStart', source: 'startup',
}, async () => unchanged), unchanged);
assert.equal(helpers, 2);
const chunk = Object.freeze({ kind: 'text', text: 'PRIVATE' });
for (const turnId of ['turn-outage', 'turn-recovery']) {
  const event = Object.freeze({ turnId, index: 0, model: 'selected', messageCount: 2 });
  const final = Object.freeze({ turnId, index: 0, answer: 'PRIVATE', toolUses: [], stopReason: 'end_turn',
    usage: { model: 'response-model', input_tokens: 10, output_tokens: 3, cache_read_input_tokens: 2, cache_creation_input_tokens: 4 } });
  const stream = hooks.get('turn.step')(sdk, event, async function* (passed) {
    assert.equal(passed, event); delegates++;
    if (turnId === 'turn-recovery') {
      assert.equal(endings.length, 3, 'later main step retries its original pending predecessor before delegation');
      assert.deepEqual(endings[2], endings[0]);
    }
    yield chunk; return final;
  });
  assert.equal((await stream.next()).value, chunk);
  const completed = await stream.next();
  assert.equal(completed.done, true); assert.equal(completed.value, final);
}
assert.equal(delegates, 2, 'delivery retry never reruns provider work');
assert.equal(endings.length, 4);
assert.deepEqual(endings[1], endings[0]);
assert.notEqual(endings[3].event_id, endings[0].event_id);
assert.equal(endings[3].observer_witness, endings[0].observer_witness);
assert(!JSON.stringify(endings).includes('PRIVATE'));
assert.equal(helpers, 12);
for (const [name, extra] of [
  ['UserPromptSubmit', { prompt: 'PRIVATE' }],
  ['Notification', { notification_type: 'permission_prompt', message: 'PRIVATE' }],
  ['Stop', { stop_hook_active: true }],
  ['StopFailure', { error: 'PRIVATE' }],
]) {
  assert.equal(await hooks.get(`classic.${name}`)(sdk,
    { session_id: 'session-a', hook_event_name: name, ...extra }, async () => unchanged), unchanged);
}
assert.equal(await hooks.get('classic.SessionEnd')(sdk,
  { session_id: 'session-a', hook_event_name: 'SessionEnd', reason: 'logout' }, async () => unchanged), unchanged);
assert.equal(helpers, 17);
assert.equal(timers.length, 1); assert.equal(timers[0].cancelled, true);
process.stdout.write('{"qualified":true}\n');
