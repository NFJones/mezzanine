/** Offline lifecycle transport contracts: fixed helper stdin only, no vendor,
 * credentials, general RPC, user content, accounting or lease authority. */
import assert from 'node:assert/strict';
import test from 'node:test';
import { createClaudeLifecycleSender } from '../crates/mezzanine/src/integrations/bootstrap/claude_observer.mjs';

const observer = () => ({ external_session_id: 'session-a', generation: 1, observer_witness: 'a'.repeat(64), run_id: 1, observer_epoch: 1, agent_id: 'external-a' });
const fact = type => Object.freeze({ session: 'session-a', event: Object.freeze({ type, active: false }) });

/** Normalized facts map to exact observational states, never retirement or
 * approval/continuation. Neutral helper success is explicitly not delivery ACK. */
test('Claude lifecycle uses frozen original selectors and a separate presentation sequence', async () => {
  const source = observer(), calls = [];
  const sender = createClaudeLifecycleSender({ observer: source, isCurrent: () => true,
    send: async envelope => { calls.push(envelope); return { exitCode: 0, stdout: '{}\n' }; } });
  source.generation = 99;
  for (const type of ['session_start', 'prompt_submit', 'permission_wait', 'stop', 'stop_failure', 'session_end']) {
    const result = await sender.observe(fact(type));
    assert.equal(result.submitted, true);
    assert.equal(result.delivered, false);
  }
  assert.deepEqual(calls.map(call => call.data.state), ['ready', 'running', 'approval-wait', 'ready', 'failed', 'complete']);
  assert.deepEqual(calls.map(call => call.data.sequence), [1, 2, 3, 4, 5, 6]);
  for (const call of calls) {
    assert(Object.isFrozen(call)); assert(Object.isFrozen(call.data));
    assert.equal(call.operation, 'helper-observe'); assert.equal(call.harness, 'claude');
    assert.equal(call.generation, 1); assert.equal(call.data.title, 'Claude');
    assert(!Object.hasOwn(call, 'launch_token'));
  }
});

/** A stopped or lost original observer cannot emit queued events or revive
 * through a mutable currentness predicate; pressure never grows beyond16. */
test('Claude lifecycle bounds pending work and permanently fences owner loss', async () => {
  let live = true, release, entered;
  const started = new Promise(resolve => { entered = resolve; });
  const held = new Promise(resolve => { release = resolve; });
  const calls = [];
  const sender = createClaudeLifecycleSender({ observer: observer(), isCurrent: () => live,
    send: async envelope => { calls.push(envelope); entered(); return held; } });
  const first = sender.observe(fact('prompt_submit')); await started;
  const queued = Array.from({ length: 15 }, () => sender.observe(fact('stop')));
  assert.equal((await sender.observe(fact('stop'))).submitted, false);
  live = false; release({ exitCode: 0, stdout: '{}\n' });
  assert.equal((await first).submitted, false);
  live = true;
  assert((await Promise.all(queued)).every(result => result.submitted === false));
  assert.equal((await sender.observe(fact('stop'))).submitted, false);
  assert.equal(calls.length, 1);
});

/** Raw callback data is never retained or forwarded. Unknown/wrong-session
 * facts, malformed selectors and helper errors stay inert and content-free. */
test('Claude lifecycle rejects raw or foreign facts and neutralizes helper failures', async () => {
  const calls = [];
  const sender = createClaudeLifecycleSender({ observer: observer(), isCurrent: () => true,
    send: async envelope => { calls.push(envelope); throw new Error('PRIVATE'); } });
  for (const input of [undefined, {}, { session: 'foreign', event: { type: 'stop' } }, { session: 'session-a', event: { type: 'unknown' } }]) {
    assert.equal((await sender.observe(input)).submitted, false);
  }
  assert.equal(calls.length, 0);
  assert.equal((await sender.observe(fact('stop'))).submitted, false);
  assert(!JSON.stringify(calls).includes('PRIVATE'));
  const invalid = createClaudeLifecycleSender({ observer: { ...observer(), launch_token: 'PRIVATE' }, isCurrent: () => true,
    send: async () => assert.fail('private observer dispatched') });
  assert.equal((await invalid.observe(fact('stop'))).submitted, false);
});

/** Acceptance order is allocated before downstream completion; stale earlier
 * statuses cannot undo a later one. Copied/abandoned tickets are inert and free
 * bounded capacity without resurrecting previously published observations. */
test('Claude lifecycle orders captured callbacks and abandons exact tickets', async () => {
  const calls = [];
  const sender = createClaudeLifecycleSender({ observer: observer(), isCurrent: () => true,
    send: async value => { calls.push(value); return { exitCode: 0, stdout: '{}\n' }; } });
  const old = sender.capture(fact('stop')), newer = sender.capture(fact('prompt_submit'));
  assert.equal((await sender.publish({ ...newer })).submitted, false);
  assert.equal((await sender.publish(newer)).submitted, true);
  assert.equal((await sender.publish(old)).submitted, false);
  const abandoned = sender.capture(fact('stop')); sender.abandon(abandoned);
  assert.equal((await sender.publish(abandoned)).submitted, false);
  assert.deepEqual(calls.map(call => call.data.state), ['running']);
});
