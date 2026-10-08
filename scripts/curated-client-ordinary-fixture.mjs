/** Offline SDK-shaped ordinary producer for native curated-source qualification.
 * Loads the exact rendered module, returns from its middleware, then keeps idle
 * timers alive while a separate owned test helper gates process exit. No fd3,
 * credentials, provider work, native identity override or daemon bypass exists. */
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const [modulePath, testHelper, gate, callbackReport] = process.argv.slice(2);
const exec = promisify(execFile);
const timers = [];
const sdk = {
  process: { run: async (argv, options = {}) => {
    const output = await exec(argv[0], argv.slice(1), {
      env: { ...process.env, ...options.env }, timeout: options.timeoutMs, maxBuffer: 8192,
    });
    return { exitCode: 0, stdout: output.stdout, stderr: output.stderr };
  } },
  clock: { every: (delay, callback) => {
    const timer = setInterval(callback, delay);
    timers.push(timer);
    return { cancel: () => clearInterval(timer) };
  } },
};

/** The SDK host records the literal handler and preserves exact middleware
 * identity; no telemetry event or timer is emitted by the fixture itself. */
const { register } = await import(pathToFileURL(modulePath));
let handler;
register((name, callback) => {
  assert.equal(name, "classic.SessionStart");
  assert.equal(handler, undefined);
  handler = callback;
});
const original = Object.freeze({ unchanged: true });
try {
  const result = await handler(sdk, { session_id: "session-a", source: "startup" }, async () => original);
  assert.equal(result, original);
  writeFileSync(callbackReport, "returned");
  // This helper's native direct parent is the same normal Node producer as the
  // built source helpers. The socket is a test exit gate, not enrollment proof.
  await sdk.process.run([testHelper, "--exact",
    "runtime::control::external_enrollment::tests::claude_curated::external_claude_curated_command_helper_fixture",
    "--ignored", "--quiet"], { env: { MEZ_TEST_CURATED_SOCKET: gate }, timeoutMs: 10000 });
} finally {
  for (const timer of timers) clearInterval(timer);
}
