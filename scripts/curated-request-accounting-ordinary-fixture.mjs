/** SDK-shaped ordinary creator -> real fixed helper -> native Unix actor.
 * Only standard discovery is inherited; no fd3/token/PID or vendor request is
 * injected. Synthetic incurred counters follow actual original readiness. */
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const [directory, helper] = process.argv.slice(2);
const { createCuratedLifetime } = await import(pathToFileURL(`${directory}/curated_lifetime.mjs`));
const { createCuratedRequestAccountingClient } = await import(pathToFileURL(`${directory}/curated_request_accounting.mjs`));
const exec = promisify(execFile);
let helpers = 0;
const sdk = { process: { run: async (argv, options) => {
  helpers++;
  const result = await exec(argv[0], argv.slice(1), { timeout: options.timeoutMs, maxBuffer: 8192 });
  assert.equal(result.stderr, '');
  return { exitCode: 0, stdout: result.stdout };
} } };
const registered = await sdk.process.run([helper, 'harness-source', JSON.stringify({
  external_session_id: 'session-a', observer_instance: 'module-a', session_boundary: 'startup',
})], { timeoutMs: 3000 });
const source = JSON.parse(registered.stdout);
assert.equal(source.registered, true);
assert(!Object.hasOwn(source, 'launch_token'));
const lifetime = createCuratedLifetime();
const local = lifetime.begin('session-a');
assert(lifetime.publish(local, { external_session_id: source.external_session_id,
  generation: source.generation, observer_witness: source.observer_witness,
  run_id: source.run_id, observer_epoch: source.observer_epoch, agent_id: source.agent_id }));
const body = readFileSync(`${directory}/runner.mjs`, 'utf8');
const create = new Function('$', 'observerLifetime', 'createCuratedRequestAccountingClient',
  `${body}\nreturn requestAccounting;`);
const client = create(sdk, lifetime, createCuratedRequestAccountingClient);
const ticket = await client.beginRequest('a'.repeat(64));
assert(ticket);
const report = { event_id: 'b'.repeat(64), observed_at: ticket.admitted_at,
  provider: 'fixture', model: 'model', counters: { input_tokens: 10, output_tokens: 3 } };
assert.equal((await client.endRequest(ticket, report)).durable, true);
assert.equal(helpers, 4);
lifetime.stop();
assert.equal((await client.endRequest(ticket, report)).durable, true);
assert.equal(await client.beginRequest('c'.repeat(64)), undefined);
assert.equal(helpers, 4);
process.stdout.write('{"qualified":true}\n');
