/** Ordinary Node producer -> immutable public snapshot -> fixed child helper.
 * Only standard MEZ discovery and public observation metadata reach the child.
 * This fake vendor remains alive briefly after helper EOF for actor assertions;
 * no provider, user config, injected capability, observer fd3 or wrapper is used. */
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { pathToFileURL } from "node:url";

const lifetime = setTimeout(() => { process.exitCode = 1; }, 20000);
const { createPersistentTelemetryClient } = await import(pathToFileURL(process.argv[2]).href);
const { peerHelper } = await import(pathToFileURL(process.argv[3]).href);
const client = createPersistentTelemetryClient({ harness: process.argv[4] ?? "pi", session: "session-a",
  version: "fixture", displayName: "ordinary helper fixture", instance: "helper-instance-a", peerHelper });
let completed = false;
try {
  assert.equal(await client.start(), true);
  const snapshot = await client.captureHelperObservation("running");
  assert.equal(snapshot.operation, "helper-observe");
  assert.equal(snapshot.data.sequence, 1);
  assert.equal(snapshot.launch_token, undefined);
  assert.equal(snapshot.pane_id, undefined);
  assert.equal(snapshot.pid, undefined);
  const result = await new Promise((resolve, reject) => {
    const child = spawn(peerHelper, ["harness-event"], { shell: false,
      env: { MEZ: process.env.MEZ, MEZ_PANE: process.env.MEZ_PANE },
      stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    const limit = setTimeout(() => { child.kill("SIGKILL"); reject(new Error("helper deadline")); }, 3000);
    child.on("error", error => { clearTimeout(limit); reject(error); });
    child.stdout.on("data", bytes => {
      stdout += bytes.toString();
      if (Buffer.byteLength(stdout) > 256) { child.kill("SIGKILL"); reject(new Error("helper output bound")); }
    });
    child.stderr.on("data", bytes => {
      stderr += bytes.toString();
      if (Buffer.byteLength(stderr) > 256) { child.kill("SIGKILL"); reject(new Error("helper diagnostic bound")); }
    });
    child.on("close", code => { clearTimeout(limit); resolve({ code, stdout, stderr }); });
    child.stdin.on("error", () => {});
    child.stdin.end(JSON.stringify(snapshot));
  });
  assert.deepEqual(result, { code: 0, stdout: "{}\n", stderr: "" });
  // Closure of the producer observer allows the server loop to finish while
  // producer lifetime remains independently live. Rust checks actual applied
  // helper-only presentation, not the neutral output as a delivery receipt.
  client.detach();
  completed = true;
} finally {
  client.detach();
  if (!completed) clearTimeout(lifetime);
}
