/**
 * Opt-in offline Pi 1.0.2 loader/runner conformance fixture.
 * The package root is explicit and trusted by the caller. Only our inline
 * observer is loaded; no configuration discovery, session files, tools, API
 * clients or provider requests are created. Output is bounded synthetic facts
 * consumed by the Rust projector test, not live vendor certification.
 */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { EventEmitter } from "node:events";
import { createPiStreamExtension } from "../crates/mezzanine/src/integrations/bootstrap/pi_extension.mjs";

assert.equal(process.argv.length, 3, "explicit trusted Pi package root required");
const root = resolve(process.argv[2]);
const pkg = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
assert.equal(pkg.name, "@earendil-works/pi-coding-agent");
assert.equal(pkg.version, "1.0.2");
const [major, minor] = process.versions.node.split(".").map(Number);
assert(major > 22 || (major === 22 && minor >= 19), "Pi requires Node >=22.19.0");
const load = (file) => import(pathToFileURL(join(root, "dist/core", file)).href);
const { loadExtensionFromFactory, createExtensionRuntime } = await load("extensions/loader.js");
const { ExtensionRunner } = await load("extensions/runner.js");
const { createEventBus } = await load("event-bus.js");
const observations = [];
const runtime = createExtensionRuntime();
let opened = 0;
let closed = 0;
const stream = new EventEmitter();
stream.writableLength = 0;
stream.write = (frame) => {
  observations.push({ session: "bound", event: JSON.parse(frame) });
  return true;
};
const extension = await loadExtensionFromFactory(
  createPiStreamExtension("bound", () => {
    opened++;
    return { stream, close() { closed++; stream.emit("close"); } };
  }),
  process.cwd(), createEventBus(), runtime,
);
assert.equal(observations.length, 0);
assert.equal(opened, 0, "factory loading must not open session resources");
for (const key of ["tools", "commands", "flags", "shortcuts"]) assert.equal(extension[key].size, 0);
let session = "bound";
const runner = new ExtensionRunner([extension], runtime, process.cwd(), { getSessionId: () => session }, {});
const errors = [];
runner.onError((e) => errors.push(e));
for (const event of [
  { type: "session_start", reason: "startup", previousSessionFile: "PRIVATE" },
  { type: "agent_start", prompt: "PRIVATE" },
  { type: "agent_end", messages: ["PRIVATE"] },
  { type: "ui_prompt_start", reason: "ui_prompt", kind: "input", title: "PRIVATE" },
  { type: "ui_prompt_end", reason: "ui_prompt", kind: "input", answer: "PRIVATE" },
]) assert.equal(await runner.emit(event), undefined);
const context = { contextEntries: [], contextMessages: [], llmMessages: [], pendingMessages: [], canContinue: true };
const boundary = await runner.emitBoundary({ type: "agent_before_settle", outcome: "completed" }, async () => context);
assert.equal(boundary.continue, false);
assert.deepEqual(boundary.entries, []);
assert.equal(await runner.emit({ type: "agent_settled" }), undefined);
session = "other";
await runner.emit({ type: "agent_start", session_id: "bound" });
assert.equal(observations.length, 6);
session = "bound";
await runner.emit({ type: "session_shutdown", reason: "reload", targetSessionFile: "PRIVATE" });
runner.invalidate();
await runner.emit({ type: "agent_start" });
assert.equal(observations.length, 7);
assert.equal(opened, 1);
assert.equal(closed, 1);
assert.equal(stream.listenerCount("error"), 0);
assert.equal(errors.length, 0);
assert.equal(JSON.stringify(observations).includes("PRIVATE"), false);
process.stdout.write(JSON.stringify(observations) + "\n");
