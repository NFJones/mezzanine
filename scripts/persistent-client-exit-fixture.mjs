/** Test-owned stalled peer-helper fault injection; no vendor/provider activity.
 * Captures only boolean resource ownership and cleans its own child in finally. */
import childProcess from "node:child_process";
import { syncBuiltinESMExports } from "node:module";
import { createPersistentTelemetryClient } from "../crates/mezzanine/src/integrations/bootstrap/persistent_client.mjs";

const nativeSpawn = childProcess.spawn;
let owned;
childProcess.spawn = (...args) => {
  owned = nativeSpawn(...args);
  if (process.argv[4] === "detach") setTimeout(() => client.detach(), 50);
  return owned;
};
syncBuiltinESMExports();
const socket = process.argv[2];
const helper = process.argv[3];
const client = createPersistentTelemetryClient({ harness: "pi", version: "fixture",
  session: "session-a", instance: "instance-a", displayName: "fixture", peerHelper: helper,
  env: { MEZ: `${socket}\x1fsession=fixture\x1fwindow=@1\x1fpane=%1\x1fprotocol=mez-control/1`, MEZ_PANE: "%1" } });
// The fake vendor's own lifetime, independent of all telemetry resources.
const lifetime = setTimeout(() => {}, 2500);
try {
  const result = await client.start();
  process.stdout.write(JSON.stringify({ result, phase: client.status().phase,
    helperSpawned: !!owned, childReferenced: owned?._handle?.hasRef?.() ?? false,
    stdoutReferenced: owned?.stdout?._handle?.hasRef?.() ?? false }) + "\n");
} finally {
  client.detach();
  if (owned && owned.exitCode === null && owned.signalCode === null) owned.kill("SIGKILL");
  owned?.stdout?.destroy();
  owned?.unref();
  clearTimeout(lifetime);
  childProcess.spawn = nativeSpawn;
  syncBuiltinESMExports();
}
