/** Owned local OpenCode event plugin. Factory loading performs no I/O. Only
 * root-session metadata opens the inherited observation-only fd3; credentials,
 * daemon routes and vendor decisions never enter this plugin. Shared/remote
 * servers without a launcher-owned descriptor remain inert. */
import { Socket } from "node:net";
import { fstatSync } from "node:fs";
import { createOpenCodeObserver } from "./opencode_observer.mjs";

const ownerKey = Symbol.for("mezzanine.opencode.observer.v1");
const id = value => typeof value === "string" && /^[A-Za-z0-9_.:-]{1,128}$/.test(value);

/** Injectable inert event factory for content-free local-schema fixtures. */
export function createOpenCodeStreamPlugin(binding, open) {
  if (binding?.descriptor !== "3" || (binding.session !== undefined && !id(binding.session))
      || typeof open !== "function") return { async event() {} };
  let session;
  let stream;
  let producer;
  let available = true;
  const fail = () => { available = false; try { stream?.end(); } catch {} };
  const send = item => {
    if (!available) return false;
    try {
      const frame = JSON.stringify(item) + "\n";
      const length = Buffer.byteLength(frame);
      if (length > 8193 || !Number.isSafeInteger(stream.writableLength)
          || stream.writableLength < 0 || stream.writableLength + length > 32768) { fail(); return false; }
      if (!stream.write(frame)) { fail(); return false; }
      return true;
    } catch { fail(); return false; }
  };
  return { async event(input) {
    try {
      if (!available) return;
      const event = input?.event;
      if (!producer) {
        if (!["session.created", "session.updated"].includes(event?.type)) return;
        const info = event.properties?.info;
        if (!id(info?.id) || info.parentID != null || (binding.session && binding.session !== info.id)
            || (!binding.session && event.type !== "session.created")) return;
        session = info.id;
        stream = open();
        stream.on("error", fail);
        stream.on("close", () => { available = false; });
        producer = createOpenCodeObserver(session, item => {
          const accepted = send(item);
          if (item.kind === "unavailable" || item.state === "retire") fail();
          return accepted;
        });
        if (!send({ session, kind: "start" })) return;
      }
      await producer.event(input);
    } catch { fail(); }
  } };
}

/** OpenCode scans named plugin exports. Keep helper modules in a private
 * sibling directory and export only this plugin from the installed entry. */
export const MezzanineOpenCode = async () => createOpenCodeStreamPlugin({
  descriptor: process.env.MEZ_OPENCODE_OBSERVER_FD,
  session: process.env.MEZ_OPENCODE_SESSION,
}, () => {
  if (globalThis[ownerKey] || !fstatSync(3).isSocket()) throw new Error("OpenCode observer unavailable");
  globalThis[ownerKey] = true;
  return new Socket({ fd:3, readable:false, writable:true });
});
