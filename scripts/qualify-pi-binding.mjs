/** Explicit offline released-loader exercise of the v2 installed artifact.
 * Only supplied compiled artifacts and fake session contexts are used. No user
 * extensions, session content, tools, credentials or provider are instantiated.
 */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [rootArg, artifactArg] = process.argv.slice(2);
assert(rootArg && artifactArg, "explicit trusted package and artifact roots required");
const root = resolve(rootArg);
const pkg = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
assert.equal(pkg.name, "@earendil-works/pi-coding-agent");
const load = (file) => import(pathToFileURL(join(root, "dist/core", file)).href);
const { discoverAndLoadExtensions, createExtensionRuntime } = await load("extensions/loader.js");
const { ExtensionRunner } = await load("extensions/runner.js");
const { createEventBus } = await load("event-bus.js");
let session = "initial";
async function runner() {
  const loaded = await discoverAndLoadExtensions([], process.cwd(), resolve(artifactArg), createEventBus());
  assert.equal(loaded.errors.length, 0);
  assert.equal(loaded.extensions.length, 1);
  const runtime = createExtensionRuntime();
  const runner = new ExtensionRunner(loaded.extensions, runtime, process.cwd(), { getSessionId: () => session }, {});
  runner.onError(() => assert.fail("observer callback failed"));
  for (const key of ["tools", "commands", "flags", "shortcuts"]) assert.equal(loaded.extensions[0][key].size, 0);
  return runner;
}
let current = await runner();
assert.equal(await current.emit({ type: "session_start", reason: "startup", previousSessionFile: "PRIVATE" }), undefined);
assert.equal(await current.emit({ type: "agent_start", prompt: "PRIVATE" }), undefined);
for (const [reason, next] of [["new", "second"], ["resume", "initial"], ["fork", "branch"]]) {
  assert.equal(await current.emit({ type: "session_shutdown", reason, targetSessionFile: "PRIVATE" }), undefined);
  session = next;
  assert.equal(await current.emit({ type: "session_start", reason }), undefined);
  assert.equal(await current.emit({ type: "agent_start", prompt: "PRIVATE" }), undefined);
}
assert.equal(await current.emit({ type: "session_shutdown", reason: "reload" }), undefined);
current = await runner();
assert.equal(await current.emit({ type: "session_start", reason: "reload" }), undefined);
assert.equal(await current.emit({ type: "agent_start" }), undefined);
assert.equal(await current.emit({ type: "session_shutdown", reason: "quit" }), undefined);
