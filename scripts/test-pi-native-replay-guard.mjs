/** Regression for activation-input diagnostic privacy in the native replay
 * fixture. Execute the real Node entry point with one synthetic input at a
 * time, never inherit or inspect the user's environment. The isolated HOME has
 * only replay-mode and no installed factory: rejection must precede discovery
 * and resource creation. Captured output stays private even when assertions
 * fail; only inert booleans, public exit codes and fixed reasons are asserted.
 */
import assert from "node:assert/strict";
import { mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const fixture = fileURLToPath(new URL("./pi-native-replay-fixture.mjs", import.meta.url));
const keys = ["PI_CODING_AGENT_DIR", "MEZ_HARNESS_SESSION", "MEZ_HARNESS_OBSERVER_FD", "MEZ_HARNESS_HELPER", "MEZ_EXTERNAL_LAUNCH_TOKEN"];
const reason = "fixture received an activation input";
const home = mkdtempSync("/tmp/mez-pi-replay-guard-");
const results = [];
try {
  writeFileSync(join(home, "replay-mode"), "connected");
  for (const key of keys) {
    // This marker is authored synthetic test data, never a real credential.
    // No spread of process.env: the child receives only fixed metadata, HOME
    // and this single deliberately forbidden activation input.
    const marker = `SYNTHETIC_REPLAY_GUARD_${key}_PRIVATE`;
    const child = spawnSync(process.execPath, [fixture], {
      env: { HOME: home, LANG: "C", LC_ALL: "C", TZ: "UTC", [key]: marker },
      encoding: "utf8", timeout: 5000, maxBuffer: 64 * 1024,
    });
    results.push({
      bounded: child.error === undefined && child.signal === null,
      status: child.status,
      fixedReason: typeof child.stderr === "string" && child.stderr.includes(reason),
      markerAbsent: typeof child.stdout === "string" && typeof child.stderr === "string"
        && !child.stdout.includes(marker) && !child.stderr.includes(marker),
      stdoutEmpty: child.stdout === "",
      noResources: readdirSync(home).length === 1 && readdirSync(home)[0] === "replay-mode",
    });
  }
} finally {
  rmSync(home, { recursive: true, force: true });
}

// Aggregate every case before asserting so the old guard's genuine Node leak
// is observed for every key, without printing any captured subprocess output.
console.log(`activation guard cases: ${results.length}; bounded failures: ${results.every(result => result.bounded && result.status === 1)}; fixed diagnostic: ${results.every(result => result.fixedReason)}; marker-free cases: ${results.filter(result => result.markerAbsent).length}/${results.length}`);
assert.equal(results.length, 5, "all forbidden activation inputs were exercised");
for (const result of results) {
  assert.equal(result.bounded, true, "activation rejection must terminate within the bound");
  assert.equal(result.status, 1, "activation rejection must exit with public failure code");
  assert.equal(result.fixedReason, true, reason);
  assert.equal(result.markerAbsent, true, "activation diagnostic must omit synthetic private input");
  assert.equal(result.stdoutEmpty, true, "activation rejection must not produce stdout");
  assert.equal(result.noResources, true, "activation rejection must precede factory resources");
}
console.log("native replay activation-input guard: PASS (5 cases)");
