import assert from "node:assert/strict";
import { generateKeyPairSync, sign as edSign, createHash } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import WebSocket from "ws";
import { EXEC_COMMAND_PARAMETERS, EXECUTION_OUTPUT_SCHEMA } from "../../nanocodex-tools/tools/execution-contract.mjs";
import { startHandDeviceServer } from "./support/hand-device-server.mjs";

// Run from js/managed: npm run test:hand-device-keys. The bundled Worker needs the
// generated QuickJS evaluator (prepared by the script) and the js/nanocodex
// pkg-web WASM build: CI downloads the nanocodex-wasm artifact produced by
// ./js/nanocodex-vite/scripts/build-js-package.sh --release; locally run that
// script (or its debug variant) from the repository root first.
// Public managed routes over real loopback HTTP + WebSocket against workerd
// SQLite Durable Objects. Only external identity is synthetic.
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const TTL_SECONDS = 5;
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const DOMAIN = "nanocodex-hand-device:v1";

function keypair() {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  return { publicKey: publicKey.export({ format: "jwk" }).x, privateKey,
    fingerprint: "SHA256:" + createHash("sha256").update(Buffer.from(publicKey.export({ format: "jwk" }).x, "base64url")).digest("base64").replace(/=+$/, "") };
}
const signWith = (key, fields) => edSign(null, Buffer.from([DOMAIN, ...fields.map(String)].join("\n")), key.privateKey).toString("base64url");

/** Redact bearer credentials, grants, API keys, signatures and challenges from evidence. */
function redact(value) {
  return JSON.parse(JSON.stringify(value ?? null, (_key, entry) => typeof entry !== "string" ? entry
    : entry.replace(/ncxh[dg]1\.[A-Za-z0-9._-]+/g, m => m.slice(0, 7) + "[redacted]")
      .replace(/ncx_live_[A-Za-z0-9_-]+/g, "ncx_live_[redacted]")
      .replace(/^[A-Za-z0-9_-]{86}$/, "[signature]").replace(/^[A-Za-z0-9_-]{54}$/, "[challenge]")));
}

test("Hand device keys: enrollment, credentials, fences, rotation, revocation and legacy Hands", { timeout: 180_000 }, async () => {
  const output = join(repo, "output/hand-device-keys/server-journey", `${new Date().toISOString().replace(/[:.]/g, "-")}-${process.pid}`);
  await mkdir(output, { recursive: true });
  const http = [], sockets = [], checks = [];
  let server;
  const check = (name, condition, detail) => { checks.push({ name, ok: !!condition, ...(detail === undefined ? {} : { detail: redact(detail) }) }); assert.ok(condition, name + " " + JSON.stringify(redact(detail))); };
  try {
    server = await startHandDeviceServer({ output, ttlSeconds: TTL_SECONDS });
    const { base, origin, owner, otherOwner, apiKey, otherApiKey } = server;
    async function call(label, method, path, { auth, body, headers = {} } = {}) {
      const response = await fetch(base + path, { method, headers: { ...(auth ? { authorization: "Bearer " + auth } : {}),
        ...(body === undefined ? {} : { "content-type": "application/json" }), ...headers },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(15_000) });
      const text = await response.text();
      let value; try { value = text ? JSON.parse(text) : null; } catch { value = text; }
      http.push(redact({ label, method, path, auth: auth ? auth.slice(0, 7) : null, request: body ?? null, status: response.status,
        marker: response.headers.get("x-nanocodex-hand-devices"), response: value }));
      return { status: response.status, value, marker: response.headers.get("x-nanocodex-hand-devices") };
    }
    const enroll = async (key, machine, { label = "enroll", api = apiKey, signOrigin = origin, challenge } = {}) => {
      const issued = challenge ?? (await call(label + ": challenge", "POST", "/v1/account/hand-devices/challenges", { auth: api, body: {} })).value.challenge;
      const signature = signWith(key, ["enroll", signOrigin, api === apiKey ? owner : otherOwner, issued, machine, key.publicKey]);
      const result = await call(label, "POST", "/v1/account/hand-devices", { auth: api,
        body: { machine_id: machine, name: "Synthetic " + machine, algorithm: "ed25519", public_key: key.publicKey, challenge: issued, signature } });
      return { ...result, challenge: issued, signature };
    };
    const deviceChallenge = (label, device, purpose, ownerId = owner) =>
      call(label, "POST", `/v1/hand-devices/${ownerId}/${device}/challenges`, { body: { purpose } });
    const credential = async (label, key, device, { ownerId = owner, signOrigin = origin } = {}) => {
      const issued = await deviceChallenge(label + ": challenge", device, "credential", ownerId);
      if (issued.status !== 201) return { ...issued, challenge: undefined };
      const { challenge, key_version } = issued.value;
      const signature = signWith(key, ["credential", signOrigin, ownerId, device, key_version, challenge]);
      const result = await call(label, "POST", `/v1/hand-devices/${ownerId}/${device}/credentials`, { body: { challenge, signature } });
      return { ...result, challenge, signature, key_version };
    };
    /** Open a WebSocket; resolves with the open socket or the HTTP rejection. */
    const open = (label, path, headers) => new Promise((resolve, reject) => {
      const socket = new WebSocket(base.replace(/^http/, "ws") + path, { headers });
      const record = { label, path, auth: headers.authorization?.slice(7, 14) ?? null, opened: false, frames: [] };
      sockets.push(record);
      socket.once("unexpected-response", (_request, response) => {
        let text = ""; response.on("data", chunk => { text += chunk; });
        response.on("end", () => { record.status = response.statusCode; try { record.response = JSON.parse(text); } catch { record.response = text; }
          resolve({ status: response.statusCode, value: record.response }); });
      });
      socket.once("open", () => { record.opened = true; resolve({ socket, record }); });
      socket.on("close", (code, reason) => { record.close = { code, reason: String(reason) }; });
      socket.once("error", error => { if (!record.opened && record.status === undefined) reject(error); });
    });
    const closed = socket => socket.readyState === WebSocket.CLOSED ? Promise.resolve()
      : new Promise(resolve => socket.once("close", resolve));
    const catalog = (machine, runtime) => ({ type: "catalog", attachment_id: machine, ...(runtime ? { runtime_id: runtime } : {}), turn_lifecycle: true,
      capabilities: ["turn_metadata"], machines: [{ id: machine, name: machine, workspace: "/synthetic/workspace", capabilities: ["native"] }],
      tools: [{ provider: "native", remote_name: "exec_command", parallel_safe: true, timeout_ms: 15_000,
        definition: { type: "function", name: "exec_command", description: "Synthetic device shell", strict: false,
          parameters: EXEC_COMMAND_PARAMETERS, output_schema: EXECUTION_OUTPUT_SCHEMA } }] });
    /** Publish a tool-host catalog and answer calls; resolves with the first broker frame or the close. */
    async function toolHost(label, auth, machine, { runtime = machine + "-runtime", identityHeaders = true, extraHeaders = {} } = {}) {
      const opened = await open(label, "/v1/account/tool-host", { authorization: "Bearer " + auth,
        ...(identityHeaders ? { "x-nanocodex-hand-machine-id": machine, "x-nanocodex-hand-runtime-id": runtime } : {}), ...extraHeaders });
      if (!opened.socket) return opened;
      const { socket, record } = opened;
      const first = new Promise(resolve => {
        socket.once("message", data => resolve({ frame: JSON.parse(String(data)) }));
        socket.once("close", (code, reason) => resolve({ close: { code, reason: String(reason) } }));
      });
      socket.on("message", data => {
        const frame = JSON.parse(String(data));
        record.frames.push(frame.type);
        if (frame.type === "call") socket.send(JSON.stringify({ type: "result", call_id: frame.call_id, outcome: { status: "completed",
          output: { output: "DEVICE_OK " + machine, success: true, structured_result: { output: "DEVICE_OK " + machine, exit_code: 0 }, metadata: null, process_trace: null } } }));
      });
      socket.send(JSON.stringify(catalog(machine, identityHeaders ? runtime : undefined)));
      return { socket, record, first: await first };
    }
    async function remoteHost(label, auth, machine) {
      const opened = await open(label, "/v1/account/hands/host", { authorization: "Bearer " + auth });
      if (!opened.socket) return opened;
      const { socket, record } = opened;
      const frames = [];
      const next = type => new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(label + " timed out waiting for " + type)), 10_000);
        const handler = data => { const frame = JSON.parse(String(data)); frames.push(frame.type);
          if (frame.type === type) { clearTimeout(timer); socket.off("message", handler); resolve(frame); } };
        socket.on("message", handler);
        socket.once("close", (code, reason) => { clearTimeout(timer); resolve({ type: "closed", code, reason: String(reason) }); });
      });
      const ready = await next("ready");
      socket.send(JSON.stringify({ type: "catalog", machine_id: machine, machine_name: "Synthetic " + machine,
        surfaces: [{ id: "display-1", name: "Display", kind: "desktop", width: 1280, height: 800, controllable: false }] }));
      const published = await next("published");
      record.frames = frames;
      return { socket, record, ready, published };
    }

    // 1. Enrollment with the account API key principal; idempotent and conflicting re-enrollment.
    const k1 = keypair(), machine = "device-hand-1";
    const enrolled = await enroll(k1, machine);
    check("enroll returns 201 active device", enrolled.status === 201 && enrolled.value.status === "active" && enrolled.value.key_version === 1
      && enrolled.value.fingerprint === k1.fingerprint && enrolled.marker === "v1", enrolled.value);
    const device = enrolled.value.id;
    check("device JSON never contains key material or digests", !JSON.stringify(enrolled.value).includes(k1.publicKey), Object.keys(enrolled.value));
    const again = await enroll(k1, machine, { label: "enroll again (same key)" });
    check("same key re-enroll is idempotent 200", again.status === 200 && again.value.id === device, again.value);
    const replayEnroll = await call("enroll replayed challenge+signature", "POST", "/v1/account/hand-devices", { auth: apiKey,
      body: { machine_id: machine, name: "Synthetic", algorithm: "ed25519", public_key: k1.publicKey, challenge: again.challenge, signature: again.signature } });
    check("replayed enrollment challenge rejected", replayEnroll.status === 401 && replayEnroll.value.error === "challenge_invalid", replayEnroll.value);
    const conflicting = await enroll(keypair(), machine, { label: "enroll different key same machine" });
    check("different key for an enrolled machine is 409", conflicting.status === 409 && conflicting.value.error === "hand_device_exists", conflicting.value);
    const wrongOrigin = await enroll(keypair(), "device-hand-origin", { label: "enroll signed for another origin", signOrigin: "https://attacker.example" });
    check("wrong-origin enrollment signature rejected", wrongOrigin.status === 401 && wrongOrigin.value.error === "signature_invalid", wrongOrigin.value);
    const reserved = await enroll(keypair(), "server:00000000-0000-4000-8000-000000000001", { label: "enroll reserved machine" });
    check("reserved platform machine ids cannot be enrolled", reserved.status === 400 && reserved.value.error === "hand_device_machine_reserved", reserved.value);
    const noAuthEnroll = await call("enroll without account principal", "POST", "/v1/account/hand-devices/challenges", { body: {} });
    check("enrollment requires an account principal", noAuthEnroll.status === 401, noAuthEnroll.value);

    // 2. Credentials by proof of possession; replay, cross-purpose and wrong-origin signatures.
    const cred1 = await credential("credential", k1, device);
    check("credential issued", cred1.status === 201 && /^ncxhd1\./.test(cred1.value.credential) && cred1.value.key_version === 1, cred1.value);
    const replay = await call("credential replayed challenge", "POST", `/v1/hand-devices/${owner}/${device}/credentials`,
      { body: { challenge: cred1.challenge, signature: cred1.signature } });
    check("replayed credential challenge/signature rejected", replay.status === 401 && replay.value.error === "challenge_invalid", replay.value);
    const rotateChallenge = (await deviceChallenge("rotate-purpose challenge", device, "rotate")).value;
    const crossPurpose = await call("credential with rotate-purpose challenge", "POST", `/v1/hand-devices/${owner}/${device}/credentials`,
      { body: { challenge: rotateChallenge.challenge, signature: signWith(k1, ["credential", origin, owner, device, 1, rotateChallenge.challenge]) } });
    check("challenge issued for another purpose rejected", crossPurpose.status === 401 && crossPurpose.value.error === "challenge_invalid", crossPurpose.value);
    const credChallenge = (await deviceChallenge("credential challenge (cross-purpose signature)", device, "credential")).value;
    const crossSig = await call("credential with ssh-host-keys signature", "POST", `/v1/hand-devices/${owner}/${device}/credentials`,
      { body: { challenge: credChallenge.challenge, signature: signWith(k1, ["ssh-host-keys", origin, owner, device, 1, credChallenge.challenge, ""]) } });
    check("signature for another purpose rejected", crossSig.status === 401 && crossSig.value.error === "signature_invalid", crossSig.value);
    const originCred = await credential("credential signed for another origin", k1, device, { signOrigin: "https://attacker.example" });
    check("wrong-origin credential signature rejected", originCred.status === 401 && originCred.value.error === "signature_invalid", originCred.value);
    const crossAccount = await deviceChallenge("device route under another owner", device, "credential", otherOwner);
    check("device of account A is unknown under owner B", crossAccount.status === 401 && crossAccount.value.error === "hand_reenroll_required" && crossAccount.marker === "v1", crossAccount.value);

    // 3. Device credentials are Hand-publisher-only.
    for (const [label, method, path] of [["list devices with device credential", "GET", "/v1/account/hand-devices"],
      ["agents with device credential", "GET", "/v1/agents"], ["inventory with device credential", "GET", "/v1/account/hands/inventory"],
      ["revoke with device credential", "DELETE", `/v1/account/hand-devices/${device}`]]) {
      const denied = await call(label, method, path, { auth: cred1.value.credential });
      check(label + " is 401", denied.status === 401, denied.value);
    }
    const forged = cred1.value.credential.replace(owner, otherOwner);
    const crossTool = await toolHost("tool-host with credential re-labelled to owner B", forged, machine);
    check("credential of account A rejected for account B", crossTool.status === 401, crossTool.value);

    // 4. Machine binding and a real tool round trip over the device-authenticated tool host.
    const mismatch = await toolHost("tool-host machine mismatch", cred1.value.credential, "some-other-machine");
    check("machine header mismatch is 403", mismatch.status === 403 && mismatch.value.error === "hand_device_machine_mismatch", mismatch.value);
    const missing = await toolHost("tool-host without machine header", cred1.value.credential, machine, { identityHeaders: false });
    check("missing machine header is 403", missing.status === 403 && missing.value.error === "hand_device_machine_mismatch", missing.value);
    const hostA = await toolHost("device tool-host", (await credential("fresh credential for tool-host", k1, device)).value.credential, machine);
    check("device tool-host catalog admitted", hostA.first?.frame?.type === "ready", hostA.first);
    const toolCall = await server.callHandTool({ machineId: machine, cmd: "printf DEVICE", callId: "device-call-1" });
    check("tool call round trip through device-authenticated Hand", toolCall.status === 200 && JSON.stringify(toolCall.body).includes("DEVICE_OK " + machine), toolCall.body);
    const remoteA = await remoteHost("device hands/host", (await credential("fresh credential for hands/host", k1, device)).value.credential, machine);
    check("device screen publisher published", remoteA.published?.type === "published", remoteA.published);
    const listed = await call("list devices", "GET", "/v1/account/hand-devices", { auth: apiKey });
    check("list shows the active device", listed.status === 200 && listed.value.data.some(entry => entry.id === device && entry.status === "active"
      && entry.last_authenticated_at > 0), listed.value);

    // 5. Legacy account-key Hands keep working for never-enrolled machines; enrolled machines are fenced.
    const legacy = await toolHost("legacy account-key tool-host (never enrolled)", apiKey, "legacy-hand", { identityHeaders: false });
    check("legacy never-enrolled Hand still publishes", legacy.first?.frame?.type === "ready", legacy.first);
    const legacyCall = await server.callHandTool({ machineId: "legacy-hand", cmd: "printf LEGACY", callId: "legacy-call-1" });
    check("legacy Hand tool call works", legacyCall.status === 200 && JSON.stringify(legacyCall.body).includes("DEVICE_OK legacy-hand"), legacyCall.body);
    const legacyList = await call("list shows legacy", "GET", "/v1/account/hand-devices", { auth: apiKey });
    check("legacy Hand listed", legacyList.value.legacy.some(entry => entry.machine_id === "legacy-hand" && entry.auth === "account_api_key"), legacyList.value.legacy);
    const fencedConnect = await toolHost("account key for enrolled machine (headers)", apiKey, machine);
    check("account key for an enrolled machine is 403 hand_device_required", fencedConnect.status === 403 && fencedConnect.value.error === "hand_device_required", fencedConnect.value);
    const fencedCatalog = await toolHost("account key header-less catalog for enrolled machine", apiKey, machine, { identityHeaders: false });
    check("header-less legacy catalog for an enrolled machine is fenced at admission", fencedCatalog.first?.close?.code === 1008
      && fencedCatalog.first.close.reason.includes("hand_device_required"), fencedCatalog.first);
    const fencedRemote = await remoteHost("account key screen publisher for enrolled machine", apiKey, machine);
    check("legacy screen publisher for an enrolled machine is rejected", fencedRemote.published?.type === "closed", fencedRemote.published);

    // 6. Expiry and renewal: an expired credential is rejected; a fresh proof yields a working one.
    // The screen publication lease is bounded by its credential; renewal needs a fresh, currently valid one.
    const shortLived = (await credential("credential to expire", k1, device)).value;
    const renewal = await call("hands/renew with fresh credential", "POST", "/v1/account/hands/renew", { auth: shortLived.credential,
      body: { connection_id: remoteA.ready.connection_id } });
    check("screen publisher renewed with a fresh credential", renewal.status === 200, renewal.value);
    await sleep(TTL_SECONDS * 1000 + 1200);
    const expired = await toolHost("expired credential tool-host", shortLived.credential, machine, { runtime: "device-hand-1-runtime-2" });
    check("expired credential rejected", expired.status === 401, expired.value);
    const staleRenew = await call("hands/renew with expired credential", "POST", "/v1/account/hands/renew", { auth: shortLived.credential,
      body: { connection_id: remoteA.ready.connection_id } });
    check("renew with an expired credential rejected", staleRenew.status === 401, staleRenew.value);
    check("live tool-host outlives its connect credential", hostA.socket.readyState === WebSocket.OPEN, hostA.record);
    const fresh = (await credential("fresh credential after expiry", k1, device)).value;
    const hostA2 = await toolHost("tool-host reconnect with fresh credential", fresh.credential, machine, { runtime: "device-hand-1-runtime-2b" });
    check("fresh credential after expiry is accepted (renewal)", hostA2.first?.frame?.type === "ready", hostA2.first);
    const renewedCall = await server.callHandTool({ machineId: machine, cmd: "printf RENEWED", callId: "device-call-2" });
    check("tool call works on the renewed connection", renewedCall.status === 200 && JSON.stringify(renewedCall.body).includes("DEVICE_OK " + machine), renewedCall.body);

    // 7. SSH host attestation (optional capability) and rotation proven by old and new keys.
    const fingerprints = [keypair().fingerprint, keypair().fingerprint];
    const attestChallenge = (await deviceChallenge("ssh-host-keys challenge", device, "ssh-host-keys")).value;
    const attested = await call("attest ssh host keys", "PUT", `/v1/hand-devices/${owner}/${device}/ssh-host-keys`, { body: { challenge: attestChallenge.challenge,
      signature: signWith(k1, ["ssh-host-keys", origin, owner, device, 1, attestChallenge.challenge, [...fingerprints].sort().join(",")]), fingerprints } });
    check("host key attestation stored", attested.status === 200 && attested.value.ssh_host_keys.length === 2, attested.value);
    const oldCredential = (await credential("credential before rotation", k1, device)).value;
    const k2 = keypair();
    const rc = (await deviceChallenge("rotate challenge", device, "rotate")).value;
    const rotateFields = ["rotate", origin, owner, device, 1, rc.challenge, k2.publicKey];
    const halfRotate = await call("rotate signed only by old key", "POST", `/v1/hand-devices/${owner}/${device}/rotate`,
      { body: { challenge: rc.challenge, signature: signWith(k1, rotateFields), new_public_key: k2.publicKey, new_signature: signWith(keypair(), rotateFields) } });
    check("rotation without proof of the new key rejected", halfRotate.status === 401 && halfRotate.value.error === "signature_invalid", halfRotate.value);
    const rc2 = (await deviceChallenge("rotate challenge 2", device, "rotate")).value;
    const rotateFields2 = ["rotate", origin, owner, device, 1, rc2.challenge, k2.publicKey];
    const toolClosed = closed(hostA2.socket);
    const rotated = await call("rotate", "POST", `/v1/hand-devices/${owner}/${device}/rotate`,
      { body: { challenge: rc2.challenge, signature: signWith(k1, rotateFields2), new_public_key: k2.publicKey, new_signature: signWith(k2, rotateFields2) } });
    check("rotation succeeds with old+new proofs", rotated.status === 200 && rotated.value.key_version === 2 && rotated.value.fingerprint === k2.fingerprint
      && rotated.value.ssh_host_keys.length === 0, rotated.value);
    await toolClosed;
    check("rotation closes the live device tool-host for reconnect", hostA2.record.close?.code === 1012 && hostA2.record.close.reason === "hand_device_rotated", hostA2.record.close);
    const oldVersion = await toolHost("old key-version credential", oldCredential.credential, machine, { runtime: "device-hand-1-runtime-3" });
    check("old key-version credential rejected", oldVersion.status === 401, oldVersion.value);
    const oldKey = await credential("old key signs a new challenge", k1, device);
    check("old key can no longer obtain credentials", oldKey.status === 401 && oldKey.value.error === "signature_invalid", oldKey.value);
    const newKeyCred = await credential("new key credential", k2, device);
    check("new key obtains version-2 credentials", newKeyCred.status === 201 && newKeyCred.value.key_version === 2, newKeyCred.value);

    // 8. Revoke while connected: live sockets close, reattach and challenges fail, stolen key is useless.
    const hostB = await toolHost("device tool-host after rotation", newKeyCred.value.credential, machine, { runtime: "device-hand-1-runtime-4" });
    check("rotated device republishes", hostB.first?.frame?.type === "ready", hostB.first);
    const remoteB = await remoteHost("device hands/host after rotation", (await credential("credential for hands/host 2", k2, device)).value.credential, machine);
    check("rotated device screen publisher published", remoteB.published?.type === "published", remoteB.published);
    const unexpired = (await credential("credential held across revoke", k2, device)).value;
    const heldChallenge = (await deviceChallenge("challenge held across revoke", device, "credential")).value;
    const bothClosed = Promise.all([closed(hostB.socket), closed(remoteB.socket)]);
    const revoked = await call("revoke device", "DELETE", `/v1/account/hand-devices/${device}`, { auth: apiKey });
    check("revoke reports closed live connections", revoked.status === 200 && revoked.value.status === "revoked" && revoked.value.closed_connections >= 2, revoked.value);
    await bothClosed;
    check("revoked tool-host socket closed 1008 hand_device_revoked", hostB.record.close?.code === 1008 && hostB.record.close.reason === "hand_device_revoked", hostB.record.close);
    check("revoked screen publisher socket closed", remoteB.record.close !== undefined, remoteB.record.close);
    const revokedAgain = await call("revoke again", "DELETE", `/v1/account/hand-devices/${device}`, { auth: apiKey });
    check("revoke is idempotent", revokedAgain.status === 200 && revokedAgain.value.status === "revoked", revokedAgain.value);
    const reattach = await toolHost("reattach with unexpired credential after revoke", unexpired.credential, machine, { runtime: "device-hand-1-runtime-5" });
    check("reattach after revoke is 401", reattach.status === 401, reattach.value);
    const revokedChallenge = await deviceChallenge("challenge after revoke", device, "credential");
    check("challenge after revoke is 401 hand_reenroll_required", revokedChallenge.status === 401 && revokedChallenge.value.error === "hand_reenroll_required", revokedChallenge.value);
    const stolen = await call("stolen key signs a held challenge", "POST", `/v1/hand-devices/${owner}/${device}/credentials`,
      { body: { challenge: heldChallenge.challenge, signature: signWith(k2, ["credential", origin, owner, device, 2, heldChallenge.challenge]) } });
    check("stolen revoked key gets 401", stolen.status === 401 && stolen.value.error === "hand_reenroll_required", stolen.value);
    const legacyAfterRevoke = await toolHost("account key for revoked machine", apiKey, machine);
    check("account key for a revoked machine stays 403 hand_device_required", legacyAfterRevoke.status === 403 && legacyAfterRevoke.value.error === "hand_device_required", legacyAfterRevoke.value);
    const reuse = await enroll(k2, machine, { label: "re-enroll with the revoked key" });
    check("revoked keys never re-enroll", reuse.status === 409 && reuse.value.error === "hand_device_key_in_use", reuse.value);
    const k3 = keypair();
    const reenrolled = await enroll(k3, machine, { label: "re-enroll with a new key" });
    check("re-enrollment with a new key creates a new device", reenrolled.status === 201 && reenrolled.value.id !== device, reenrolled.value);

    // 9. Owner forget revokes the device but the device-required mark is permanent; policy fences all account keys.
    const forgot = await call("forget machine", "DELETE", "/v1/account/hands/" + machine + "?force=1", { auth: apiKey });
    check("forget succeeds", forgot.status === 200, forgot.value);
    const afterForget = await call("list after forget", "GET", "/v1/account/hand-devices", { auth: apiKey });
    check("forgotten machine devices removed from listing", !afterForget.value.data.some(entry => entry.machine_id === machine), afterForget.value);
    const legacyAfterForget = await toolHost("account key after forget", apiKey, machine);
    check("forget never clears the downgrade fence", legacyAfterForget.status === 403 && legacyAfterForget.value.error === "hand_device_required", legacyAfterForget.value);
    const policyOn = await call("require device keys", "PUT", "/v1/account/hand-devices/policy", { auth: apiKey, body: { require_device_keys: true } });
    check("policy can be tightened by an API key", policyOn.status === 200 && policyOn.value.policy.require_device_keys === true, policyOn.value);
    const legacyUnderPolicy = await toolHost("legacy Hand under require_device_keys", apiKey, "legacy-hand-2", { identityHeaders: false });
    check("require_device_keys rejects account-key publishers", legacyUnderPolicy.status === 403 && legacyUnderPolicy.value.error === "hand_device_required", legacyUnderPolicy.value);
    const policyOff = await call("relax policy with API key", "PUT", "/v1/account/hand-devices/policy", { auth: apiKey, body: { require_device_keys: false } });
    check("an API key cannot relax the policy", policyOff.status === 403, policyOff.value);

    // 10. Server bootstrap grant: single use, publishes with device credentials, fences the server bearer.
    const host = "55555555-5555-4555-8555-555555555555";
    const minted = await (await fetch(base + "/__fixture/grant", { method: "POST", body: JSON.stringify({ owner, host, name: "synthetic@server" }) })).json();
    check("server grant minted", /^ncxhg1\./.test(minted.grant), minted);
    const serverKey = keypair(), serverMachine = "server:" + host;
    const grantDigest = createHash("sha256").update(minted.grant.split(".")[3]).digest("base64url");
    const grantBody = { algorithm: "ed25519", public_key: serverKey.publicKey, signature: signWith(serverKey, ["enroll", origin, owner, grantDigest, serverMachine, serverKey.publicKey]) };
    const grantEnroll = await call("server grant enrollment", "POST", `/v1/hand-hosts/${owner}/${host}/hands/device`, { auth: minted.grant, body: grantBody });
    check("server device enrolled with its grant", grantEnroll.status === 201 && grantEnroll.value.machine_id === serverMachine, grantEnroll.value);
    const grantReplay = await call("server grant replay", "POST", `/v1/hand-hosts/${owner}/${host}/hands/device`, { auth: minted.grant, body: grantBody });
    check("server grant is single use", grantReplay.status === 401, grantReplay.value);
    const serverCred = (await credential("server device credential", serverKey, grantEnroll.value.id)).value;
    const grantOnAccount = await call("server grant on account route", "GET", "/v1/account/hand-devices", { auth: minted.grant });
    check("grant never authenticates account routes", grantOnAccount.status === 401, grantOnAccount.value);
    const serverTool = await toolHost("server device tool-host", serverCred.credential, serverMachine);
    check("server device publishes its machine on /v1/account/tool-host", serverTool.first?.frame?.type === "ready", serverTool.first);
    const attestation = await call("other account cannot read server attestations via device routes", "POST",
      `/v1/hand-devices/${otherOwner}/${grantEnroll.value.id}/challenges`, { body: { purpose: "ssh-host-keys" } });
    check("server device unknown under another owner", attestation.status === 401, attestation.value);
    void otherApiKey;
    const seen = (predicate) => server.observations.some(record => record?.type === "hand.connection" && predicate(record));
    check("legacy account-key Hand observed as legacy", seen(r => r.auth_mode === "account_api_key" && r.legacy === true && r.machine_id === "legacy-hand" && !r.outcome));
    check("downgrade attempts observed as rejected", seen(r => r.auth_mode === "account_api_key" && r.machine_id === machine && r.reason_code === "hand_device_required"));
    check("device connections observed with device_key auth", seen(r => r.auth_mode === "device_key" && r.machine_id === machine && r.surface === "tool_host")
      && seen(r => r.auth_mode === "device_key" && r.surface === "remote"));
    serverTool.socket.close(); legacy.socket.close(); remoteA.socket?.close();
    await sleep(300);
  } finally {
    const observations = (server?.observations ?? []).filter(record => record?.type === "hand.connection" && (record.auth_mode || record.reason_code));
    await writeFile(join(output, "http.json"), JSON.stringify(http, null, 2) + "\n");
    await writeFile(join(output, "sockets.json"), JSON.stringify(redact(sockets), null, 2) + "\n");
    await writeFile(join(output, "observations.json"), JSON.stringify(redact(observations), null, 2) + "\n");
    await writeFile(join(output, "checks.json"), JSON.stringify(checks, null, 2) + "\n");
    const leaked = JSON.stringify(server?.logs ?? []).match(/ncxh[dg]1\.[0-9a-f-]{36}\.[0-9a-f-]{36}\.[A-Za-z0-9_-]{43}/);
    await writeFile(join(output, "summary.json"), JSON.stringify({ command: "node --test test/hand-device-keys-journey.test.mjs (js/managed)",
      ttl_seconds: TTL_SECONDS, checks: checks.length, failed: checks.filter(entry => !entry.ok).map(entry => entry.name),
      legacy_observations: observations.filter(record => record.auth_mode === "account_api_key").length,
      device_observations: observations.filter(record => record.auth_mode === "device_key").length,
      secrets_in_worker_logs: !!leaked }, null, 2) + "\n");
    await server?.stop();
    console.log("hand-device-keys evidence: " + output);
    assert.ok(!leaked, "device credentials must never appear in worker logs");
  }
});
