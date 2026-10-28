/** Public Claude turn.step contract fixtures from locally generated declarations.
 * No vendor/provider/transcript or credentials are used; only inert identity
 * and finalized SDK counters may enter the accounting projection. */
import assert from 'node:assert/strict';
import test from 'node:test';
import { captureClaudeStep, identifyClaudeStep, projectClaudeStepOutcome } from '../crates/mezzanine/src/integrations/bootstrap/claude_request.mjs';

const input = () => ({ turnId: 'turn-a', index: 0, model: 'selected-model', messageCount: 2 });
const result = () => ({ turnId: 'turn-a', index: 0, answer: 'PRIVATE', toolUses: [], stopReason: 'end_turn',
  usage: { model: 'response-model', input_tokens: 10, output_tokens: 3, cache_read_input_tokens: 2, cache_creation_input_tokens: 4 } });

/** Step identity excludes counters, response model, time and callback content.
 * Final counters use SDK uncached+cache input semantics; provider and reasoning
 * are unavailable because this API does not name them. */
test('Claude step identity and final usage are stable content-free observations', async () => {
  const step = await identifyClaudeStep(captureClaudeStep('session-a', input()));
  assert(Object.isFrozen(step));
  assert.match(step.request_id, /^[a-f0-9]{64}$/);
  assert.match(step.event_id, /^[a-f0-9]{64}$/);
  assert.notEqual(step.request_id, step.event_id);
  const changed = input(); changed.model = 'different-selected'; changed.messageCount = 99;
  const replay = await identifyClaudeStep(captureClaudeStep('session-a', changed));
  assert.equal(replay.request_id, step.request_id);
  assert.notEqual((await identifyClaudeStep(captureClaudeStep('session-b', input()))).request_id, step.request_id);
  const outcome = projectClaudeStepOutcome(step, result(), 101);
  assert.equal(outcome.provider, 'unavailable');
  assert.equal(outcome.model, 'response-model');
  assert.deepEqual(outcome.counters, { input_tokens: 16, output_tokens: 3, cached_input_tokens: 2, cache_write_input_tokens: 4 });
  assert(!Object.hasOwn(outcome.counters, 'reasoning_tokens'));
  assert(!JSON.stringify(outcome).includes('PRIVATE'));
});

/** Child/unknown identities, mismatched final result, absent/unsafe SDK counters
 * and overflow cannot fabricate zero, a newest marker, or another observation. */
test('Claude request projector rejects unknown identities and unsafe final usage', async () => {
  for (const event of [null, {}, { ...input(), agentId: 'child' }, { ...input(), index: -1 },
    { ...input(), index: 9007199254740992 }, { ...input(), turnId: 'PRIVATE\n' }]) {
    assert.equal(captureClaudeStep('session-a', event), undefined);
  }
  const step = await identifyClaudeStep(captureClaudeStep('session-a', input()));
  for (const outcome of [{ ...result(), turnId: 'foreign' }, { ...result(), index: 1 }, { ...result(), usage: null },
    { ...result(), usage: { ...result().usage, output_tokens: -1 } },
    { ...result(), usage: { ...result().usage, input_tokens: Number.MAX_SAFE_INTEGER } },
    { ...result(), usage: { ...result().usage, cache_read_input_tokens: undefined } }]) {
    assert.equal(projectClaudeStepOutcome(step, outcome, 101), undefined);
  }
  assert.equal(projectClaudeStepOutcome(step, result(), 0), undefined);
});

/** SDK callback getters are captured once, and unrelated content is never read.
 * A malformed getter is unavailable evidence, not a vendor middleware failure. */
test('Claude projector captures selectors once and never reads answer or chunks', async () => {
  let reads = 0;
  const event = input();
  Object.defineProperty(event, 'turnId', { get() { reads++; return 'turn-a'; } });
  Object.defineProperty(event, 'messages', { get() { assert.fail('transcript read'); } });
  const step = await identifyClaudeStep(captureClaudeStep('session-a', event));
  assert.equal(reads, 1);
  const outcome = result();
  Object.defineProperty(outcome, 'answer', { get() { assert.fail('answer read'); } });
  Object.defineProperty(outcome, 'toolUses', { get() { assert.fail('tool content read'); } });
  assert(projectClaudeStepOutcome(step, outcome, 101));
  Object.defineProperty(outcome.usage, 'model', { get() { throw new Error('PRIVATE'); } });
  assert.equal(projectClaudeStepOutcome(step, outcome, 101), undefined);
});

/** Response-model spelling is data rather than a source/request selector.
 * Valid cloud punctuation and exact bounded UTF8 survive without modifying
 * immutable expense IDs; unknown/control/oversized labels never become expense. */
test('Claude response model labels preserve bounded inert metadata', async () => {
  const step = await identifyClaudeStep(captureClaudeStep('session-a', input()));
  assert.equal(step.request_id, '7042362f4a84ac30981915be04df27bcfdcb6f1feea2f3b169dbc9a307c564e6');
  assert.equal(step.event_id, 'edc01fc33ee38fdc8f78812613eb9eaacef8cf295210e08bebfda39a0bbe1e57');
  for (const model of ['claude-haiku-4-5@20251001', 'publisher/model@revision', 'é'.repeat(64)]) {
    const outcome = result(); outcome.usage.model = model;
    assert.equal(projectClaudeStepOutcome(step, outcome, 101)?.model, model);
  }
  for (const model of ['', '  ', 'model\n', 'model\u0085', 'model\u202e', 'é'.repeat(65)]) {
    const outcome = result(); outcome.usage.model = model;
    assert.equal(projectClaudeStepOutcome(step, outcome, 101), undefined);
  }
  assert.equal(captureClaudeStep('session@other', input()), undefined);
  assert.equal(captureClaudeStep('session-a', { ...input(), turnId: 'turn@other' }), undefined);
});
