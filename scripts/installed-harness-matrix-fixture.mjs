/** Offline ordinary fake vendor for the installed supported-subset matrix.
 * Discovers the production-installed metadata from HOME, never accepts module
 * paths, helper commands, authority, counters or callbacks from the parent.
 * Claude uses an SDK-shaped loader shim, NOT an actual vendor loader. Classic
 * vendors execute their installed command/exec entries. Only synthetic public
 * callbacks are emitted; no vendor, transcript, provider or user config is read.
 * A separate coordination socket carries phase bytes only, not control claims.
 */
import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [harness] = process.argv.slice(2);
assert(['claude', 'codex', 'copilot', 'cursor'].includes(harness));
assert.deepEqual(Object.keys(process.env).sort(), ['HOME', 'MEZ', 'MEZ_PANE', 'PATH']);
const root = path.join(process.env.HOME, `.${harness}`);
const json = async file => JSON.parse(await readFile(file, 'utf8'));
const coordination = net.createConnection(process.env.MEZ.split('\x1f')[0]);
await once(coordination, 'connect');
coordination.setTimeout(30000, () => { throw new Error('matrix phase timeout'); });

/** Allows inspection while retaining the real creator and all private state. */
async function phase(value) {
  const acknowledged = once(coordination, 'data');
  coordination.write(Buffer.from([value]));
  const [data] = await acknowledged;
  assert.deepEqual(data, Buffer.from([1]));
}

/** Runs only installed argv/commands, preserving neutral observational output. */
async function execute(argv, stdin = '') {
  const child = spawn(argv[0], argv.slice(1), { stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', data => { stdout += data; assert(stdout.length <= 8192); });
  child.stderr.on('data', data => { stderr += data; assert(stderr.length <= 8192); });
  // An authored observational callback is allowed not to consume its stdin.
  child.stdin.on('error', error => { if (error.code !== 'EPIPE') throw error; });
  child.stdin.end(stdin);
  const [code] = await once(child, 'close');
  assert.equal(code, 0);
  assert.equal(stderr, '');
  return stdout;
}

let callback, step, heartbeat, assertClosed;
if (harness === 'claude') {
  // Follow the installed skill/plugin/hooks graph, not a supplied entry path.
  const hooks = new Map(), timers = [];
  let found = 0;
  for (const skill of await readdir(path.join(root, 'skills'))) {
    const directory = path.join(root, 'skills', skill);
    let manifest;
    try { manifest = await json(path.join(directory, '.claude-plugin/plugin.json')); }
    catch (error) { if (error.code === 'ENOENT') continue; throw error; }
    if (manifest.name !== 'mezzanine-telemetry') continue;
    for (const entry of (await json(path.join(directory, 'hooks/hooks.json'))).modules) {
      const { register } = await import(pathToFileURL(path.resolve(directory, 'hooks', entry)));
      register((name, handler) => hooks.set(name, handler));
      found++;
    }
  }
  assert.equal(found, 1);
  const sdk = {
    process: { run: async (argv, options) => ({
      exitCode: 0, stdout: await execute(argv, options.stdin ?? ''),
    }) },
    clock: { every: (_period, handler) => {
      const timer = { handler, cancelled: false, cancel() { this.cancelled = true; } };
      timers.push(timer); return timer;
    } },
  };
  const unchanged = Object.freeze({ untouched: true });
  callback = async (event, extra = {}) => {
    assert.equal(await hooks.get(`classic.${event}`)(sdk, {
      session_id: 'matrix-session', hook_event_name: event, ...extra,
    }, async () => unchanged), unchanged);
  };
  step = async usage => {
    const final = Object.freeze({ turnId: 'matrix-turn', index: usage ? 0 : 1,
      answer: 'PRIVATE_MATRIX_CONTENT', stopReason: 'end_turn', toolUses: [], usage });
    const chunk = Object.freeze({ kind: 'text', text: 'PRIVATE_MATRIX_CONTENT' });
    const stream = hooks.get('turn.step')(sdk, {
      turnId: final.turnId, index: final.index, model: 'selected-not-response', messageCount: 2,
    }, async function* () { yield chunk; return final; });
    assert.equal((await stream.next()).value, chunk);
    const done = await stream.next();
    assert.equal(done.done, true); assert.equal(done.value, final);
  };
  heartbeat = async () => {
    assert.equal(timers.length, 1); assert.equal(timers[0].cancelled, false);
    await timers[0].handler();
  };
  assertClosed = () => assert(timers.every(timer => timer.cancelled));
} else {
  const hooks = (await json(path.join(root,
    harness === 'copilot' ? 'hooks/mezzanine.json' : 'hooks.json'))).hooks;
  callback = async (event, extra = {}) => {
    for (const entry of hooks[event]) {
      const payload = { hook_event_name: event, session_id: 'matrix-session',
        conversation_id: 'matrix-session', sessionId: 'matrix-session',
        generation_id: 'matrix-turn', turn_id: 'matrix-turn', timestamp: 1,
        source: 'startup', status: 'completed', loop_count: 0, stop_hook_active: false,
        prompt: 'PRIVATE_MATRIX_CONTENT', transcript_path: 'PRIVATE_MATRIX_CONTENT',
        usage: { input_tokens: 999999 }, model: 'selected-not-response', ...extra };
      const argv = harness === 'copilot' ? [entry.exec, ...entry.args]
        : ['/bin/sh', '-c', harness === 'codex' ? entry.hooks[0].command : entry.command];
      assert.equal(await execute(argv, JSON.stringify(payload)), '{}\n');
    }
  };
}

await phase(0);
const start = harness === 'copilot' ? 'sessionStart' : harness === 'cursor' ? 'sessionStart' : 'SessionStart';
const prompt = harness === 'copilot' ? 'userPromptSubmitted' : harness === 'cursor' ? 'beforeSubmitPrompt' : 'UserPromptSubmit';
await callback(start, { source: 'startup' });
await callback(start, { source: 'startup' });
await callback(prompt, { timestamp: 2, prompt: 'PRIVATE_MATRIX_CONTENT' });
await phase(1);
if (harness === 'claude') {
  const usage = Object.freeze({ model: 'response-model', input_tokens: 10, output_tokens: 3,
    cache_read_input_tokens: 2, cache_creation_input_tokens: 4 });
  await step(usage); await step(usage); await step(null);
  await heartbeat();
  await callback('Notification', { notification_type: 'permission_prompt', message: 'PRIVATE_MATRIX_CONTENT' });
} else if (harness === 'codex') {
  await callback('Stop', { 'event.name': 'codex.sse_event', 'event.kind': 'response.completed',
    input_token_count: '121', output_token_count: '13', cached_token_count: 32,
    total_token_usage: { input_tokens: 5000 }, last_token_usage: { input_tokens: 1000 } });
} else if (harness === 'copilot') {
  await callback('notification', { timestamp: 3, notification_type: 'permission_prompt' });
  await callback('agentStop', { timestamp: 4, stopReason: 'end_turn' });
} else {
  await callback('preCompact', { context_tokens: 999999 });
  await callback('stop', { status: 'completed' });
}
await phase(2);
await callback(harness === 'copilot' || harness === 'cursor' ? 'sessionEnd' : 'SessionEnd',
  { timestamp: 5, reason: harness === 'claude' ? 'logout' : harness === 'cursor' ? 'user_close' : 'user_exit' });
assertClosed?.();
await phase(3);
coordination.end();
