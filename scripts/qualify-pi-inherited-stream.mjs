/**
 * Explicit offline Pi 1.0.2 qualification using an inherited Unix descriptor.
 * Only the reviewed inline or explicitly supplied candidate extension is loaded. No user configuration, session
 * history, provider client or tools are created. Descriptor 3 carries lifecycle
 * observations only; daemon credentials remain in the parent Rust fixture.
 */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { Socket } from "node:net";
import { once } from "node:events";
import { createPiStreamExtension } from "../crates/mezzanine/src/integrations/bootstrap/pi_extension.mjs";

assert([3, 4].includes(process.argv.length), "explicit trusted Pi package root required");
const root = resolve(process.argv[2]);
const pkg = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
assert.equal(pkg.name, "@earendil-works/pi-coding-agent");
assert.equal(pkg.version, "1.0.2");
const [major, minor] = process.versions.node.split(".").map(Number);
assert(major > 22 || (major === 22 && minor >= 19));
const load = (file) => import(pathToFileURL(join(root, "dist/core", file)).href);
const { loadExtensionFromFactory, createExtensionRuntime, discoverAndLoadExtensions } = await load("extensions/loader.js");
const { ExtensionRunner } = await load("extensions/runner.js");
const { createEventBus } = await load("event-bus.js");
let opened = 0;
let closed;
const runtime = createExtensionRuntime();
let extension;
let duplicate;
if (process.argv[3]) {
  const artifactRoot = resolve(process.argv[3]);
  const loaded = await discoverAndLoadExtensions([], process.cwd(), artifactRoot, createEventBus());
  assert.equal(loaded.errors.length, 0);
  assert.equal(loaded.extensions.length, 1);
  extension = loaded.extensions[0];
  const second = await discoverAndLoadExtensions([], process.cwd(), artifactRoot, createEventBus());
  assert.equal(second.errors.length, 0);
  assert.equal(second.extensions.length, 1);
  duplicate = second.extensions[0];
} else {
extension = await loadExtensionFromFactory(createPiStreamExtension("bound", () => {
  opened++;
  const stream = new Socket({ fd: 3, readable: false, writable: true });
  closed = once(stream, "close");
  return { stream, close() { stream.end(); } };
}), process.cwd(), createEventBus(), runtime);
}
assert.equal(opened, 0);
for (const key of ["tools", "commands", "flags", "shortcuts"]) assert.equal(extension[key].size, 0);
const runner = new ExtensionRunner(duplicate ? [extension, duplicate] : [extension], runtime, process.cwd(), { getSessionId: () => "bound" }, {});
const errors = [];
runner.onError((e) => errors.push(e));
for (const event of [
  { type: "session_start", reason: "startup", previousSessionFile: "PRIVATE" },
  { type: "agent_start", prompt: "PRIVATE" },
  { type: "ui_prompt_start", reason: "ui_prompt", kind: "input", title: "PRIVATE" },
  { type: "ui_prompt_end", reason: "ui_prompt", kind: "input", answer: "PRIVATE" },
]) assert.equal(await runner.emit(event), undefined);
const context = { contextEntries: [], contextMessages: [], llmMessages: [], pendingMessages: [], canContinue: true };
const boundary = await runner.emitBoundary({ type: "agent_before_settle", outcome: "completed" }, async () => context);
assert.equal(boundary.continue, false);
assert.deepEqual(boundary.entries, []);
assert.equal(await runner.emit({ type: "agent_settled" }), undefined);
assert.equal(await runner.emit({ type: "session_shutdown", reason: "quit", targetSessionFile: "PRIVATE" }), undefined);
if (!process.argv[3]) {
  await closed;
  assert.equal(opened, 1);
}
assert.equal(errors.length, 0);
