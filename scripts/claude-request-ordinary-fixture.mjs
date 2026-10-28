/** Ordinary SDK-shaped host of the compiled Claude literal module. Synthetic
 * final counters cross real native helper/actor/ledger ownership; no provider
 * is contacted, no fd3 or native PID/capability is injected, and no transcript
 * is read. This is not a live vendor request or installed bootstrap claim. */
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { pathToFileURL } from 'node:url';

const [directory] = process.argv.slice(2);
const childScopes = process.argv[1].endsWith('claude-child-ordinary-fixture.mjs');
const pairedClear = process.argv[1].endsWith('claude-clear-ordinary-fixture.mjs');
const responseModel = process.argv[1].endsWith('claude-model-ordinary-fixture.mjs')
  ? 'claude-haiku-4-5@20251001' : 'response-model';
const { register } = await import(pathToFileURL(`${directory}/register.mjs`));
const exec = promisify(execFile), hooks = new Map();
register((name, handler) => hooks.set(name, handler));
let helpers = 0, cancelled = false, fixedHelper;
const coverageGaps = [];
const sources = [], timers = [];
const sdk = { process: { run: async (argv, options) => {
  helpers++;
  const execution = exec(argv[0], argv.slice(1), { timeout: options.timeoutMs, maxBuffer: 8192 });
  execution.child.stdin.end(options.stdin ?? '');
  const result = await execution;
  assert.equal(result.stderr, '');
  if (argv[1] === 'harness-source' && JSON.parse(argv[2]).operation === 'curated-usage-gap') {
    const capsule = JSON.parse(argv[2]);
    assert.deepEqual(JSON.parse(result.stdout), { observed: true, reason: capsule.reason });
    coverageGaps.push(capsule);
  }
  if (argv[1] === 'harness-source' && JSON.parse(argv[2]).operation === undefined) {
    const source = JSON.parse(result.stdout);
    assert.equal(source.registered, true);
    assert(!Object.hasOwn(source, 'launch_token'));
    fixedHelper = argv[0];
    sources.push(source);
  }
  return { exitCode: 0, stdout: result.stdout };
} }, clock: { every: (_period, callback) => { const timer = { callback, cancelled: false, cancel() { cancelled = this.cancelled = true; } }; timers.push(timer); return timer; } } };
const unchanged = Object.freeze({ unchanged: true });
assert.equal(await hooks.get('classic.SessionStart')(sdk, {
  session_id: 'session-a', hook_event_name: 'SessionStart', source: 'startup',
}, async () => unchanged), unchanged);
assert.equal(helpers, 2);
const chunk = Object.freeze({ kind: 'text', text: 'PRIVATE' });
const final = Object.freeze({ turnId: 'turn-a', index: 0, answer: 'PRIVATE', toolUses: [], stopReason: 'end_turn',
  usage: { model: responseModel, input_tokens: 10, output_tokens: 3, cache_read_input_tokens: 2, cache_creation_input_tokens: 4 } });
for (let iteration = 0; iteration < 2; iteration++) {
  const event = Object.freeze({ turnId: 'turn-a', index: 0, model: 'selected', messageCount: 2 });
  const stream = hooks.get('turn.step')(sdk, event, async function* (passed) {
    assert.equal(passed, event); yield chunk; return final;
  });
  assert.equal((await stream.next()).value, chunk);
  const completed = await stream.next();
  assert.equal(completed.done, true); assert.equal(completed.value, final);
}
assert.equal(helpers, 8);
if (childScopes) {
  const unknown = { turnId: 'turn-a', index: 0, agentId: 'unobserved' };
  const inert = hooks.get('turn.step')(sdk, unknown, async function* (e) {
    assert.equal(e, unknown); return final;
  });
  assert.equal((await inert.next()).value, final);
  assert.equal(helpers, 8);
  for (const [agentId, count] of [['child-a', 2], ['child-b', 1]]) {
    assert.equal(await hooks.get('classic.SubagentStart')(sdk, {
      session_id: 'session-a', hook_event_name: 'SubagentStart', agent_id: agentId,
    }, async () => unchanged), unchanged);
    for (let index = 0; index < count; index++) {
      const event = Object.freeze({ turnId: 'turn-a', index: 0, agentId });
      const stream = hooks.get('turn.step')(sdk, event, async function* (e) {
        assert.equal(e, event); yield chunk; return final;
      });
      assert.equal((await stream.next()).value, chunk);
      const beforeStop = helpers;
      assert.equal(await hooks.get('classic.Stop')(sdk, {
        session_id: 'session-a', hook_event_name: 'Stop', agent_id: agentId,
        stop_hook_active: true,
      }, async () => unchanged), unchanged);
      assert.equal(helpers, beforeStop, 'child Stop cannot repaint or retire parent');
      assert.equal((await stream.next()).value, final);
    }
  }
  assert.equal(helpers, 17);
}
const missing = Object.freeze({ ...final, index: 1, usage: null });
const stream = hooks.get('turn.step')(sdk, { turnId: 'turn-a', index: 1 }, async function* () { return missing; });
assert.equal((await stream.next()).value, missing);
assert.equal(helpers, childScopes ? 19 : 10);
assert.equal(coverageGaps.length, 1);
assert.equal(coverageGaps[0].reason, "finalized-counters");
for (const [name, extra] of [
  ['UserPromptSubmit', { prompt: 'PRIVATE' }],
  ['Notification', { notification_type: 'permission_prompt', message: 'PRIVATE' }],
  ['Stop', { stop_hook_active: true }],
  ['StopFailure', { error: 'PRIVATE' }],
]) {
  assert.equal(await hooks.get(`classic.${name}`)(sdk,
    { session_id: 'session-a', hook_event_name: name, ...extra }, async () => unchanged), unchanged);
}
await hooks.get('classic.SessionEnd')(sdk, { session_id: 'session-a', hook_event_name: 'SessionEnd', reason: 'clear' }, async () => unchanged);
assert.equal(helpers, childScopes ? 24 : 15);
assert.equal(cancelled, true);
if (pairedClear) {
  assert.equal(await hooks.get('classic.SessionStart')(sdk, {
    session_id: 'session-a', hook_event_name: 'SessionStart', source: 'clear',
  }, async () => unchanged), unchanged);
  assert.equal(helpers, 17);
  assert.equal(sources.length, 2);
  assert.equal(sources[1].agent_id, sources[0].agent_id);
  assert.equal(sources[1].run_id, sources[0].run_id);
  assert.equal(sources[1].observer_epoch, sources[0].observer_epoch + 1);
  assert(sources[1].generation > sources[0].generation);
  assert.notEqual(sources[1].observer_witness, sources[0].observer_witness);
  assert.equal(timers[0].cancelled, true);
  assert.equal(timers[1].cancelled, false);
  await hooks.get('classic.SessionStart')(sdk, {
    session_id: 'session-a', hook_event_name: 'SessionStart', source: 'clear',
  }, async () => unchanged);
  assert.equal(helpers, 17, 'coalesced clear cannot re-admit');
  // Already committed request identity stays inert across the qualified epoch
  // change; one distinct synthetic request may incur new usage exactly once.
  for (const turnId of ['turn-a', 'turn-clear']) {
    const event = Object.freeze({ turnId, index: 0, model: 'selected', messageCount: 2 });
    const outcome = Object.freeze({ ...final, turnId });
    const stream = hooks.get('turn.step')(sdk, event, async function* (passed) {
      assert.equal(passed, event); yield chunk; return outcome;
    });
    assert.equal((await stream.next()).value, chunk);
    const completed = await stream.next();
    assert.equal(completed.done, true); assert.equal(completed.value, outcome);
  }
  assert.equal(helpers, 23);
  await timers[0].callback();
  assert.equal(helpers, 23, 'old queued timer must remain inert');
  const oldProof = await sdk.process.run([fixedHelper, 'harness-source', JSON.stringify({
    operation: 'curated-heartbeat', external_session_id: sources[0].external_session_id,
    generation: sources[0].generation, observer_witness: sources[0].observer_witness, sequence: 1,
  })], { timeoutMs: 3000 });
  assert.equal(JSON.parse(oldProof.stdout).observed, false);
  await timers[1].callback();
  assert.equal(helpers, 25, 'fresh timer uses the successor witness');
  await hooks.get('classic.UserPromptSubmit')(sdk, {
    session_id: 'session-a', hook_event_name: 'UserPromptSubmit', prompt: 'PRIVATE',
  }, async () => unchanged);
  await hooks.get('classic.SessionEnd')(sdk, {
    session_id: 'session-a', hook_event_name: 'SessionEnd', reason: 'logout',
  }, async () => unchanged);
  assert.equal(helpers, 27);
  assert.equal(timers[1].cancelled, true);
}
process.stdout.write('{"qualified":true}\n');
