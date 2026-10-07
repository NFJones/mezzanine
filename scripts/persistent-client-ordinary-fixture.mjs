/** Ordinary producer fixture: imports installed private client artifacts and
 * uses standard nonsecret pane discovery. No injected token, fd3 or primary. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

// A genuine interactive vendor owns its event loop independently of telemetry.
// Keep this offline fake vendor alive for a finite test lifetime; the client
// socket/timers remain unreferenced and cannot hold a real vendor open on exit.
const lifetime = setTimeout(() => { process.exitCode = 1; }, 20000);
const { createPersistentTelemetryClient } = await import(pathToFileURL(process.argv[2]).href);
const { peerHelper } = await import(pathToFileURL(process.argv[3]).href);
const old = createPersistentTelemetryClient({ harness: "pi", session: "session-a",
  version: "fixture", displayName: "ordinary Node fixture", instance: "node-instance-a", peerHelper });
let replacement;
try {
  assert.equal(await old.start(), true, "ordinary client enrollment unavailable");
  assert.equal((await old.presentation("running")).delivered, true);
  replacement = old.successor("node-instance-b");
  assert.equal(await replacement.start(), true, "observer replacement unavailable");
  assert.equal(replacement.status().run, old.status().run);
  assert.equal(replacement.status().epoch, 2);
  old.detach();
  assert.equal((await replacement.presentation("complete")).delivered, true);
  assert.equal((await replacement.end()).delivered, true);
} finally { old.detach(); replacement?.detach(); clearTimeout(lifetime); }
