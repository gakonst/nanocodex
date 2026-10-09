import assert from "node:assert/strict";
import { generateKeyPairSync, sign as edSign, createHash, randomUUID } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import WebSocket from "ws";
import { EXEC_COMMAND_PARAMETERS, EXECUTION_OUTPUT_SCHEMA } from "../../nanocodex-tools/tools/execution-contract.mjs";
import { startHandDeviceServer } from "./support/hand-device-server.mjs";

// Run from js/managed: node --test test/hand-device-vm-host-journey.test.mjs
// VM factory hosts on the public /v1/account/vm-host WebSocket, authenticated by
// Hand device credentials, over real loopback HTTP against workerd SQLite
// Durable Objects. The fixture front door presents an https public origin.
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const PUBLIC_ORIGIN = "https://managed.vm-host.test";
const DOMAIN = "nanocodex-hand-device:v1";
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

function keypair() {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  return { publicKey: publicKey.export({ format: "jwk" }).x, privateKey };
}
const signWith = (key, fields) => edSign(null, Buffer.from([DOMAIN, ...fields.map(String)].join("\n")), key.privateKey).toString("base64url");
function redact(value) {
  return JSON.parse(JSON.stringify(value ?? null, (_key, entry) => typeof entry !== "string" ? entry
    : entry.replace(/ncxh[dg]1\.[A-Za-z0-9._-]+/g, m => m.slice(0, 7) + "[redacted]")
      .replace(/ncx_live_[A-Za-z0-9_-]+/g, "ncx_live_[redacted]")
      .replace(/^[A-Za-z0-9_-]{86}$/, "[signature]").replace(/^[A-Za-z0-9_-]{54}$/, "[challenge]")));
}

test("VM factory on device credentials: machine-bound registration, downgrade fence, revoke and rotate close", { timeout: 180_000 }, async () => {
  const output = join(repo, "output/hand-device-keys/vm-host", new Date().toISOString().replace(/[:.]/g, "-") + "-" + process.pid);
  await mkdir(output, { recursive: true });
  const http = [], sockets = [], checks = [];
  let server;
  const check = (name, condition, detail) => { checks.push({ name, ok: !!condition, ...(detail === undefined ? {} : { detail: redact(detail) }) }); assert.ok(condition, name + " " + JSON.stringify(redact(detail))); };
  try {
    server = await startHandDeviceServer({ output, ttlSeconds: 60, publicOrigin: PUBLIC_ORIGIN });
    const { base, origin, owner, apiKey } = server;
    async function call(label, method, path, { auth, body } = {}) {
      const response = await fetch(base + path, { method, headers: { ...(auth ? { authorization: "Bearer " + auth } : {}),
        ...(body === undefined ? {} : { "content-type": "application/json" }) },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(15_000) });
      const text = await response.text();
      let value; try { value = text ? JSON.parse(text) : null; } catch { value = text; }
      http.push(redact({ label, method, path, auth: auth ? auth.slice(0, 7) : null, request: body ?? null, status: response.status, response: value }));
      return { status: response.status, value };
    }
    const enroll = async (key, machine) => {
      const challenge = (await call("enroll challenge " + machine, "POST", "/v1/account/hand-devices/challenges", { auth: apiKey, body: {} })).value.challenge;
      return call("enroll " + machine, "POST", "/v1/account/hand-devices", { auth: apiKey, body: { machine_id: machine, name: "Synthetic " + machine,
        algorithm: "ed25519", public_key: key.publicKey, challenge, signature: signWith(key, ["enroll", origin, owner, challenge, machine, key.publicKey]) } });
    };
    const challengeFor = (label, device, purpose) => call(label, "POST", "/v1/hand-devices/" + owner + "/" + device + "/challenges", { body: { purpose } });
    const credential = async (label, key, device) => {
      const { challenge, key_version } = (await challengeFor(label + " challenge", device, "credential")).value;
      return (await call(label, "POST", "/v1/hand-devices/" + owner + "/" + device + "/credentials",
        { body: { challenge, signature: signWith(key, ["credential", origin, owner, device, key_version, challenge]) } })).value.credential;
    };
    const open = (label, path, headers) => new Promise((resolve, reject) => {
      const socket = new WebSocket(base.replace(/^http/, "ws") + path, { headers });
      const record = { label, path, auth: headers.authorization?.slice(7, 14) ?? null, opened: false, frames: [] };
      sockets.push(record);
      socket.once("unexpected-response", (_request, response) => {
        let text = ""; response.on("data", chunk => { text += chunk; });
        response.on("end", () => { record.status = response.statusCode; try { record.response = JSON.parse(text); } catch { record.response = text; }
          resolve({ status: response.statusCode, value: record.response, record }); });
      });
      socket.once("open", () => { record.opened = true; resolve({ socket, record }); });
      socket.on("close", (code, reason) => { record.close = { code, reason: String(reason) }; });
      socket.once("error", error => { if (!record.opened && record.status === undefined) reject(error); });
    });
    const closed = (socket, ms = 10_000) => socket.readyState === WebSocket.CLOSED ? Promise.resolve(true)
      : new Promise(resolve => { const timer = setTimeout(() => resolve(false), ms); socket.once("close", () => { clearTimeout(timer); resolve(true); }); });
    /** Publish a Hand catalog whose machine advertises VM factories. */
    async function toolHost(label, auth, machine, factories, headers = true) {
      const opened = await open(label, "/v1/account/tool-host", { authorization: "Bearer " + auth,
        ...(headers ? { "x-nanocodex-hand-machine-id": machine, "x-nanocodex-hand-runtime-id": machine + "-runtime" } : {}) });
      if (!opened.socket) return opened;
      const first = new Promise(resolve => {
        opened.socket.once("message", data => resolve({ frame: JSON.parse(String(data)) }));
        opened.socket.once("close", (code, reason) => resolve({ close: { code, reason: String(reason) } }));
      });
      opened.socket.send(JSON.stringify({ type: "catalog", attachment_id: machine, ...(headers ? { runtime_id: machine + "-runtime" } : {}), turn_lifecycle: true,
        capabilities: ["turn_metadata"], machines: [{ id: machine, name: machine, workspace: "/synthetic/workspace",
          capabilities: ["native", ...factories.map(name => "vm_factory:" + name)] }],
        tools: [{ provider: "native", remote_name: "exec_command", parallel_safe: true, timeout_ms: 15_000,
          definition: { type: "function", name: "exec_command", description: "Synthetic shell", strict: false,
            parameters: EXEC_COMMAND_PARAMETERS, output_schema: EXECUTION_OUTPUT_SCHEMA } }] }));
      return { ...opened, first: await first };
    }
    /** Connect a VM factory host and attach one factory; resolves with the first server frame and the socket. */
    async function factory(label, auth, factoryName, hostId, extraHeaders = {}) {
      const opened = await open(label, "/v1/account/vm-host", { authorization: "Bearer " + auth, ...extraHeaders });
      if (!opened.socket) return opened;
      const { socket, record } = opened;
      const first = new Promise(resolve => {
        const timer = setTimeout(() => resolve({ timeout: true }), 10_000);
        socket.once("message", data => { clearTimeout(timer); resolve({ frame: JSON.parse(String(data)) }); });
        socket.once("close", (code, reason) => { clearTimeout(timer); resolve({ close: { code, reason: String(reason) } }); });
      });
      socket.on("message", data => record.frames.push(JSON.parse(String(data)).type));
      socket.send(JSON.stringify({ type: "attach", protocol_version: 1, host_id: hostId, factory_name: factoryName, max_vms: 1, vm: { cpus: 1, memory_mib: 512 } }));
      const result = { socket, record, first: await first };
      if (result.first.frame?.type === "error") await closed(socket, 3_000);
      return result;
    }

    // Two device-enrolled Hands advertising VM factories, plus one never-enrolled legacy Hand.
    const keyA = keypair(), keyB = keypair(), machineA = "vm-device-a", machineB = "vm-device-b";
    const deviceA = (await enroll(keyA, machineA)).value.id, deviceB = (await enroll(keyB, machineB)).value.id;
    check("both Hands enrolled", !!deviceA && !!deviceB && deviceA !== deviceB, { deviceA, deviceB });
    const credA = await credential("device A credential", keyA, deviceA), credB = await credential("device B credential", keyB, deviceB);
    const hostA = await toolHost("device A publishes vm_factory:garage-a", credA, machineA, ["garage-a"]);
    // Machine B also (claims to) advertise garage-a: the first device-bound registration wins permanently.
    const hostB = await toolHost("device B publishes vm_factory:garage-b and garage-a", credB, machineB, ["garage-b", "garage-a"]);
    check("device Hands published", hostA.first?.frame?.type === "ready" && hostB.first?.frame?.type === "ready", { a: hostA.first, b: hostB.first });
    const legacy = await toolHost("legacy account-key Hand publishes vm_factory:garage-legacy", apiKey, "legacy-vm-hand", ["garage-legacy"], false);
    check("legacy Hand published with the account key", legacy.first?.frame?.type === "ready", legacy.first);

    // 1. A device credential registers the factory its own machine advertises; no account API key.
    const hostIdA = randomUUID(), hostIdB = randomUUID();
    const factoryA = await factory("device A attaches garage-a", credA, "garage-a", hostIdA);
    check("device credential VM factory for its own machine receives a lease", factoryA.first?.frame?.type === "lease" && factoryA.first.frame.epoch === 1, factoryA.first);

    // 2. Another machine's provider is rejected.
    const crossName = await factory("device A attaches machine B's garage-b", credA, "garage-b", randomUUID());
    check("device cannot register a factory its machine does not advertise", crossName.first?.frame?.type === "error"
      && crossName.first.frame.code === "vm_factory_not_advertised" && crossName.record.close?.code === 1008, { first: crossName.first, close: crossName.record.close });
    const crossBound = await factory("device B attaches garage-a (bound to machine A)", credB, "garage-a", randomUUID());
    check("a factory bound to one machine is refused for another device", crossBound.first?.frame?.code === "hand_device_machine_mismatch", crossBound.first);
    const crossHost = await factory("device B reuses A's host_id", credB, "garage-a", hostIdA);
    check("another device cannot take over the bound host identity", crossHost.first?.frame?.code === "hand_device_machine_mismatch", crossHost.first);
    check("rejections never disturbed the bound factory's lease", factoryA.socket.readyState === WebSocket.OPEN, factoryA.record);

    // 3. The account API key cannot register a device-bound machine's factory, even asserting device headers.
    const downgrade = await factory("account key attaches garage-a", apiKey, "garage-a", hostIdA);
    check("account-key vm-host for a device-bound machine rejected", downgrade.first?.frame?.code === "hand_device_required", downgrade.first);
    const injected = await factory("account key with forged device headers", apiKey, "garage-a", randomUUID(), {
      "x-nanocodex-vm-host-device-id": deviceA, "x-nanocodex-vm-host-device-machine": machineA, "x-nanocodex-vm-host-device-key-version": "1" });
    check("forged device binding headers are stripped", injected.first?.frame?.code === "hand_device_required", injected.first);
    const legacyFactory = await factory("legacy Hand factory with the account key", apiKey, "garage-legacy", randomUUID());
    check("never-enrolled Hand factory keeps working with the account key", legacyFactory.first?.frame?.type === "lease", legacyFactory.first);

    // 4. Rotation closes the device's live factory socket; the new key version reconnects.
    const factoryB = await factory("device B attaches garage-b", credB, "garage-b", hostIdB);
    check("device B factory leased", factoryB.first?.frame?.type === "lease", factoryB.first);
    const keyB2 = keypair();
    const rc = (await challengeFor("rotate challenge", deviceB, "rotate")).value;
    const fields = ["rotate", origin, owner, deviceB, 1, rc.challenge, keyB2.publicKey];
    const rotated = await call("rotate device B", "POST", "/v1/hand-devices/" + owner + "/" + deviceB + "/rotate",
      { body: { challenge: rc.challenge, signature: signWith(keyB, fields), new_public_key: keyB2.publicKey, new_signature: signWith(keyB2, fields) } });
    check("rotation succeeds", rotated.status === 200 && rotated.value.key_version === 2, rotated.value);
    check("rotation closes the live vm-host socket for reconnect", await closed(factoryB.socket) && factoryB.record.close?.code === 1012
      && factoryB.record.close.reason === "hand_device_rotated", factoryB.record.close);
    const credB2 = await credential("device B v2 credential", keyB2, deviceB);
    const factoryB2 = await factory("device B v2 reattaches garage-b", credB2, "garage-b", hostIdB);
    check("rotated device re-registers its factory", factoryB2.first?.frame?.type === "lease" && factoryB2.first.frame.epoch === 2, factoryB2.first);

    // 5. Revocation closes the live vm-host socket and fences reconnects.
    const revoked = await call("revoke device A", "DELETE", "/v1/account/hand-devices/" + deviceA, { auth: apiKey });
    check("revoke succeeds and counts the factory socket", revoked.status === 200 && revoked.value.closed_connections >= 2, revoked.value);
    check("revoke reports no VM factory close failure", revoked.value.warnings === undefined, revoked.value);
    check("revoke closes the live vm-host socket", await closed(factoryA.socket) && factoryA.record.close?.code === 1008
      && factoryA.record.close.reason === "hand_device_revoked", factoryA.record.close);
    const afterRevoke = await factory("revoked device reconnects", credA, "garage-a", hostIdA);
    check("revoked device credential cannot reconnect", afterRevoke.status === 401, afterRevoke.value);
    const legacyAfter = await factory("account key after revoke", apiKey, "garage-a", hostIdA);
    check("revocation never reopens the factory to the account key", legacyAfter.first?.frame?.code === "hand_device_required", legacyAfter.first);

    const seen = predicate => server.observations.some(predicate);
    check("device factory registrations observed as device_key", seen(r => r?.type === "hand.connection" && r.surface === "vm_host" && r.auth_mode === "device_key" && r.machine_id === machineA));
    check("legacy factory observed as legacy", seen(r => r?.type === "hand.connection" && r.surface === "vm_host" && r.legacy === true && !r.outcome));
    check("factory rejections observed with fixed codes", seen(r => r?.type === "vm.pool.factory_rejected" && r.reason_code === "hand_device_required")
      && seen(r => r?.type === "vm.pool.factory_rejected" && r.reason_code === "hand_device_machine_mismatch"));
    check("device closes observed", seen(r => r?.type === "vm.pool.device_closed" && r.device_id === deviceA && r.closed === 1)
      && seen(r => r?.type === "vm.pool.device_closed" && r.device_id === deviceB && r.reason === "hand_device_rotated"));
    for (const socket of [hostB.socket, legacy.socket, legacyFactory.socket, factoryB2.socket]) socket?.close();
    await sleep(300);
  } finally {
    const observations = (server?.observations ?? []).filter(record => record?.type === "vm.pool.factory_rejected" || record?.type === "vm.pool.device_closed"
      || (record?.type === "hand.connection" && record.surface === "vm_host"));
    await writeFile(join(output, "http.json"), JSON.stringify(http, null, 2) + "\n");
    await writeFile(join(output, "sockets.json"), JSON.stringify(redact(sockets), null, 2) + "\n");
    await writeFile(join(output, "observations.json"), JSON.stringify(redact(observations), null, 2) + "\n");
    await writeFile(join(output, "checks.json"), JSON.stringify(checks, null, 2) + "\n");
    const leaked = JSON.stringify(server?.logs ?? []).match(/ncxh[dg]1\.[0-9a-f-]{36}\.[0-9a-f-]{36}\.[A-Za-z0-9_-]{43}/);
    await writeFile(join(output, "summary.json"), JSON.stringify({ command: "node --test test/hand-device-vm-host-journey.test.mjs (js/managed)",
      public_origin: PUBLIC_ORIGIN, checks: checks.length, failed: checks.filter(entry => !entry.ok).map(entry => entry.name),
      secrets_in_worker_logs: !!leaked }, null, 2) + "\n");
    await server?.stop();
    console.log("vm-host evidence: " + output);
    assert.ok(!leaked, "device credentials must never appear in worker logs");
  }
});
