/** Pure source-shaped child association tests. Observer objects are inert epoch
 * brands; this fixture grants no native source, SDK capability or expense ACK. */
import assert from 'node:assert/strict';
import test from 'node:test';
import { createClaudeChildScopes } from '../crates/mezzanine/src/integrations/bootstrap/claude_child_scopes.mjs';

const start = (agent_id = 'child-a', session_id = 'session-a') => ({ hook_event_name: 'SubagentStart', session_id, agent_id });

/** Only the three public identity fields are captured, exactly once. Copied
 * handles and equal-looking observer objects cannot publish another epoch. */
test('child scopes capture inert metadata and require exact branded attempts', () => {
  const scopes = createClaudeChildScopes(), observer = Object.freeze({});
  let reads = 0;
  const event = start();
  Object.defineProperty(event, 'agent_id', { get() { reads++; return 'child-a'; } });
  for (const key of ['transcript_path', 'cwd', 'agent_type', 'prompt', 'agent_transcript_path']) {
    Object.defineProperty(event, key, { get() { throw new Error('PRIVATE read'); } });
  }
  const ticket = scopes.prepare('session-a', event, observer);
  assert.equal(reads, 1);
  assert.equal(scopes.current('session-a', 'child-a', observer), false);
  scopes.finish({ ...ticket });
  assert.equal(scopes.current('session-a', 'child-a', observer), false);
  scopes.finish(ticket);
  assert.equal(scopes.current('session-a', 'child-a', observer), true);
  assert.equal(scopes.current('session-a', 'child-a', {}), false);
  assert.equal(scopes.current(undefined, 'unknown', observer), false);
});

/** Downstream failure cancels only the exact pending attempt. Newer completion
 * wins; old completion/cancellation cannot overwrite its observer association. */
test('child scope completion and cancellation are latest-attempt fenced', () => {
  const scopes = createClaudeChildScopes(), first = {}, second = {};
  const old = scopes.prepare('session-a', start(), first);
  const newer = scopes.prepare('session-a', start(), second);
  scopes.finish(newer); scopes.finish(old); scopes.cancel(old);
  assert.equal(scopes.current('session-a', 'child-a', second), true);
  assert.equal(scopes.current('session-a', 'child-a', first), false);
  const cancelled = scopes.prepare('session-a', start(), first);
  scopes.cancel(cancelled); scopes.finish(cancelled);
  assert.equal(scopes.current('session-a', 'child-a', second), true);
});

/** A child ID keeps its first session even if its initial start failed. This
 * tombstone prevents a later parent from adopting an ambiguous old child, while
 * genuine same-session starts may bind a freshly qualified parent epoch. */
test('child scopes reject cross-session reuse and require explicit epoch refresh', () => {
  const scopes = createClaudeChildScopes(), old = {}, newer = {};
  const ticket = scopes.prepare('session-a', start(), old); scopes.cancel(ticket);
  const foreign = scopes.prepare('session-b', start('child-a', 'session-b'), newer);
  assert.equal(scopes.finish(foreign), 'request-readiness');
  assert.equal(scopes.current('session-b', 'child-a', newer), false);
  scopes.finish(scopes.prepare('session-a', start(), old));
  assert.equal(scopes.current('session-a', 'child-a', newer), false);
  scopes.finish(scopes.prepare('session-a', start(), newer));
  assert.equal(scopes.current('session-a', 'child-a', newer), true);
  assert.equal(scopes.current('session-a', 'child-a', old), false);
});

/** Global metadata is finite across active, stopped and failed child starts.
 * Exhaustion never evicts an old identity to make an unrelated parent eligible. */
test('child scope capacity retains original identities without eviction', () => {
  const scopes = createClaudeChildScopes(), observer = {};
  for (let n = 0; n < 128; n++) scopes.finish(scopes.prepare('session-a', start(`child-${n}`), observer));
  assert.equal(scopes.finish(scopes.prepare('session-a', start('overflow'), observer)), 'capacity');
  assert.equal(scopes.current('session-a', 'child-0', observer), true);
  assert.equal(scopes.finish(scopes.prepare('session-b', start('child-0', 'session-b'), observer)), 'request-readiness');
  assert.equal(scopes.current('session-b', 'child-0', observer), false);
});

/** Invalid/foreign/unadmitted facts cannot reserve identities or acquire an
 * epoch. Unknown fields are ignored rather than read or serialized. */
test('child scopes reject malformed metadata and missing parent epochs', () => {
  const scopes = createClaudeChildScopes(), observer = {};
  for (const event of [null, [], {}, start(''), start('../PRIVATE'), start('child-a', 'foreign'), { ...start(), hook_event_name: 'SubagentStop' }]) {
    assert.equal(scopes.prepare('session-a', event, observer), undefined);
  }
  assert.equal(scopes.prepare('session-a', start(), undefined), undefined);
  scopes.finish(scopes.prepare('session-a', start(), observer));
  assert.equal(scopes.current('session-a', 'child-a', observer), true);
});
