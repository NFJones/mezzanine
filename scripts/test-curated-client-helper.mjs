/** Production fixed-helper/client seam on an isolated same-user Unix socket.
 * No vendor, native admission bypass, user settings or provider work occurs.
 * A fixture daemon supplies public receipts solely to qualify reply handling;
 * separate native SDK fixtures qualify actual creator/root/epoch authority. */
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { readFileSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import { createCuratedLifetime } from "../crates/mezzanine/src/integrations/bootstrap/curated_lifetime.mjs";

const helper = process.env.MEZ_TEST_HELPER_BINARY;
const execute = promisify(execFile);
const template = readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/curated_client.mjs", import.meta.url), "utf8");
const body = template.replace(/__MEZ_[A-Z]+__/g, marker => ({
  __MEZ_OWNED__: "true", __MEZ_LIFETIME__: "observerLifetime", __MEZ_INTERVAL__: "10000",
  __MEZ_HELPER__: JSON.stringify(helper), __MEZ_INSTANCE__: '"mez-curated-client-1"',
  __MEZ_REPLACING__: "false", __MEZ_PREDECESSOR__: "undefined",
})[marker]);
const handler = new (Object.getPrototypeOf(async function () {}).constructor)("$", "e", "next", "observerLifetime", `const result = await next(e); ${body}\nreturn result;`);

/** After genuine built-helper admission transport, an absent socket or malformed
 * daemon receipt is retryable telemetry loss. Later positive fresh/replay proof
 * retains the original owner/selectors with no re-admission. Only the strictly
 * typed forbidden reply closes that owner, cancelling before queued callbacks. */
test("built curated helper distinguishes unavailable proof from terminal rejection", { skip: !helper }, async () => {
  assert(helper.startsWith("/"));
  const directory = mkdtempSync(join(tmpdir(), "mez-client-seam-"));
  const socket = join(directory, "control.sock");
  const requests = [];
  const outputs = [];
  let mode = "valid";
  const server = createServer(stream => {
    let input = Buffer.alloc(0);
    stream.on("error", () => {});
    stream.on("data", bytes => {
      input = Buffer.concat([input, bytes]);
      assert(input.length <= 8192);
      const header = input.indexOf("\r\n\r\n");
      if (header < 0) return;
      const size = Number(input.subarray(0, header).toString().match(/(?:^|\r\n)Content-Length: (\d+)(?:\r\n|$)/i)?.[1]);
      assert(Number.isSafeInteger(size) && size > 0);
      if (input.length < header + 4 + size) return;
      assert.equal(input.length, header + 4 + size);
      const request = JSON.parse(input.subarray(header + 4).toString());
      assert.equal(request.id, "harness-source");
      requests.push(request);
      let response;
      if (request.method === "agent/external/curated-enroll") {
        response = { jsonrpc: "2.0", id: request.id, result: {
          protocol: "external-agent/1", registered: true, controls: [], agent_id: "external-a",
          generation: 7, observer_witness: "a".repeat(64), run_id: 1, observer_epoch: 1,
          observer_instance: "mez-curated-client-1", external_session_id: "session-a",
          usage: "unavailable-source-continuity", observer_transport: "unavailable-curated-freshness",
          expires_at_unix_seconds: 123, lease_seconds: 60,
        } };
      } else {
        assert.equal(request.method, "agent/external/curated-heartbeat");
        response = mode === "forbidden" || mode === "limited"
          ? { jsonrpc: "2.0", id: request.id, error: { code: mode === "forbidden" ? -32002 : -32011,
            message: "PRIVATE daemon diagnostic", data: { mezzanine_code: mode === "forbidden" ? "forbidden" : "rate_limited" } } }
          : { jsonrpc: "2.0", id: request.id, result: mode === "malformed"
            ? { observed: true, sequence: request.params.sequence, changed: false, private: "PRIVATE" }
            : { observed: true, sequence: request.params.sequence, changed: false } };
      }
      const payload = JSON.stringify(response);
      stream.end(`Content-Length: ${Buffer.byteLength(payload)}\r\nContent-Type: application/vnd.mezzanine.control+json; version=1\r\n\r\n${payload}`);
    });
  });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(socket, resolve); });
  try {
    const owner = createCuratedLifetime();
    const calls = [];
    let timer;
    let cancelled = 0;
    const sdk = { process: { run: async (argv, options) => {
      calls.push(JSON.parse(argv[2]));
      const result = await execute(argv[0], argv.slice(1), { timeout: options.timeoutMs,
        env: { MEZ: mode === "absent" ? join(directory, "absent.sock") : socket, MEZ_PANE: "%1" } });
      assert.equal(result.stderr, "");
      assert(!result.stdout.includes("PRIVATE"));
      outputs.push(JSON.parse(result.stdout));
      return { exitCode: 0, stdout: result.stdout };
    } }, clock: { every: (_delay, callback) => { timer = { callback, cancel() { cancelled++; } }; return timer; } } };
    const result = Object.freeze({ vendor: "unchanged" });
    const event = { session_id: "session-a", source: "startup" };
    assert.equal(await handler(sdk, event, async () => result, owner), result);
    const original = owner.receipt;
    assert(original);
    for (const failure of ["absent", "malformed", "limited"]) {
      mode = failure;
      await timer.callback();
      assert.deepEqual(outputs.at(-1), { observed: false });
      assert.equal(owner.receipt, original);
      assert.equal(cancelled, 0);
      mode = "valid";
      await timer.callback();
      assert.equal(outputs.at(-1).observed, true);
      assert.equal(owner.receipt, original);
    }
    assert.equal(calls.filter(call => !call.operation).length, 1);
    for (const call of calls.slice(1)) {
      assert.equal(call.generation, 7);
      assert.equal(call.observer_witness, "a".repeat(64));
    }
    mode = "forbidden";
    await timer.callback();
    assert.deepEqual(outputs.at(-1), { observed: false, terminal: true });
    assert.equal(owner.receipt, undefined);
    assert.equal(cancelled, 1);
    const count = calls.length;
    await timer.callback();
    assert.equal(await handler(sdk, event, async () => result, owner), result);
    assert.equal(calls.length, count);
    assert.equal(requests.filter(request => request.method === "agent/external/curated-enroll").length, 1);
  } finally {
    await new Promise(resolve => server.close(resolve));
    rmSync(directory, { recursive: true, force: true });
  }
});
