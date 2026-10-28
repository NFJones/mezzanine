/** Metadata-only coverage custody tests. No vendor, provider, native authority,
 * file discovery or expense is fabricated by this pure transport fixture. */
import assert from 'node:assert/strict';
import test from 'node:test';
import { createClaudeUsageGapSender } from '../crates/mezzanine/src/integrations/bootstrap/claude_usage_gap.mjs';

const observer = () => ({ external_session_id: 'session-a', generation: 1, observer_witness: 'a'.repeat(64) });
const receipt = reason => JSON.stringify({ observed: true, reason });

/** Fixed public selectors are captured once, and only a closed reason crosses
 * transport. Diagnostic custody is separate from a durable expense receipt. */
test('Claude gap sender freezes selectors and rejects arbitrary content', async () => {
  const source = observer(), calls = [];
  const sender = createClaudeUsageGapSender({ observer: source, isCurrent: () => true,
    send: async capsule => { calls.push(capsule); return receipt(capsule.reason); } });
  source.generation = 99;
  assert.equal(await sender.observe('PRIVATE'), false);
  assert.equal(await sender.observe('finalized-counters'), true);
  assert.deepEqual(calls, [{ operation: 'curated-usage-gap', ...observer(), reason: 'finalized-counters' }]);
  assert(Object.isFrozen(calls[0]));
});

/** A thousand concurrent identical facts retain one original operation. The
 * four finite reasons serialize, and a fresh later fact can redisclose after
 * receiver reconstruction without permanent process-local last-reason veto. */
test('Claude gap pending custody is bounded and later facts redisclose', async () => {
  let release, entered;
  const held = new Promise(resolve => { release = resolve; });
  const started = new Promise(resolve => { entered = resolve; });
  const calls = [];
  const sender = createClaudeUsageGapSender({ observer: observer(), isCurrent: () => true,
    send: async capsule => { calls.push(capsule); entered(); if (calls.length === 1) await held; return receipt(capsule.reason); } });
  const first = sender.observe('finalized-counters'); await started;
  const copies = Array.from({ length: 1000 }, () => sender.observe('finalized-counters'));
  assert(copies.every(copy => copy === first));
  const other = ['request-readiness', 'durable-acknowledgment', 'capacity'].map(reason => sender.observe(reason));
  assert.equal(calls.length, 1);
  release();
  assert((await Promise.all([first, ...copies, ...other])).every(Boolean));
  assert.equal(calls.length, 4);
  assert.equal(await sender.observe('finalized-counters'), true);
  assert.equal(calls.length, 5);
});

/** Queued and late replies cannot migrate into a successor owner. Once source
 * currentness fails, flipping a mutable test flag back cannot revive it. */
test('Claude gap sender fences late custody and queued stale owners', async () => {
  let live = true, release, entered;
  const held = new Promise(resolve => { release = resolve; });
  const started = new Promise(resolve => { entered = resolve; });
  const calls = [];
  const sender = createClaudeUsageGapSender({ observer: observer(), isCurrent: () => live,
    send: async capsule => { calls.push(capsule); entered(); return held; } });
  const first = sender.observe('finalized-counters'); await started;
  const queued = sender.observe('capacity'); live = false;
  release(receipt('finalized-counters'));
  assert.equal(await first, false); assert.equal(await queued, false);
  live = true;
  assert.equal(await sender.observe('capacity'), false);
  assert.equal(calls.length, 1);
});

/** Neutral helper exits, private/duplicate/escaped-alias keys and a mismatched
 * reason do not establish custody. Failures must not throw into vendor work. */
test('Claude gap sender requires its exact typed content-free receipt', async () => {
  for (const raw of ['{}', 'null', 'bad', receipt('capacity'), '{"observed":false,"reason":"finalized-counters"}',
    '{"observed":false,"observed":true,"reason":"finalized-counters"}',
    '{"observed":true,"reason":"finalized-counters","private":"PRIVATE"}',
    '{"observed":true,"rea\\u0073on":"finalized-counters"}']) {
    const sender = createClaudeUsageGapSender({ observer: observer(), isCurrent: () => true, send: async () => raw });
    assert.equal(await sender.observe('finalized-counters'), false);
  }
  const sender = createClaudeUsageGapSender({ observer: observer(), isCurrent: () => true,
    send: async () => { throw new Error('PRIVATE'); } });
  assert.equal(await sender.observe('capacity'), false);
});
