// Real shipped CLI Hand daemon against the real managed Worker (Miniflare):
// first-run device key creation and enrollment, Hand-only device credentials
// across expiries, automatic re-issue on reconnects forced by a network drop
// after each expiry, CLI rotation and crash recovery in both directions, SSH
// host key (re-)attestation, key-file refusal, and revocation without any
// fallback to the account API key. Only the account principal and clocks are
// fixtures; the CLI and daemon reach the Worker through a recording relay.
//
// Requires the built binaries: cargo build --locked -p nanocodex-bin --bin nanocodex -p nanocodex-hand-daemon --bin nanocodex-hand
// NANOCODEX_BIN / NANOCODEX_HAND_EXECUTABLE override their paths. Evidence: output/hand-device-keys/cli-journey/<ts>/.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, randomBytes } from "node:crypto";
import { appendFileSync, chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, statSync, symlinkSync, unlinkSync, writeFileSync } from "node:fs";
import { createServer, request as httpRequest } from "node:http";
import { connect } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { brotliDecompressSync, gunzipSync, inflateSync } from "node:zlib";
import { startHandDeviceServer } from "./support/hand-device-server.mjs";

const repo = fileURLToPath(new URL("../../..", import.meta.url));
const binary = process.env.NANOCODEX_BIN
  ?? join(process.env.CARGO_TARGET_DIR ?? join(repo, "target"), "debug", "nanocodex");
// The CLI forwards daemon entrypoints to the Hand executable built beside it.
const handExecutable = process.env.NANOCODEX_HAND_EXECUTABLE ?? join(dirname(binary), "nanocodex-hand");
const evidence = join(repo, "output/hand-device-keys/cli-journey", new Date().toISOString().replace(/[:.]/g, "-"));
const ttlSeconds = 10;
// A system-installed Hand on a developer machine claims /srv/nanocodex as the
// Hand home (root-owned /opt/nanocodex marker). A user namespace makes that
// marker not root-owned, so the binary uses this test's private HOME instead.
const wrapper = existsSync("/opt/nanocodex/installation.json") ? ["unshare", "-Ur"] : [];

function record(step, value) {
  appendFileSync(join(evidence, "transcript.jsonl"), JSON.stringify({ at: new Date().toISOString(), step, ...value }) + "\n");
}
const fingerprint = raw => "SHA256:" + createHash("sha256").update(raw).digest("base64").replace(/=+$/, "");
const mode = path => statSync(path).mode & 0o777;

// OpenSSH wire blob for a synthetic ed25519 host public key.
function sshHostKey() {
  const string = buffer => Buffer.concat([Buffer.from([0, 0, 0, buffer.length]), buffer]);
  const blob = Buffer.concat([string(Buffer.from("ssh-ed25519")), string(randomBytes(32))]);
  return { line: "ssh-ed25519 " + blob.toString("base64") + " synthetic@hand-device-journey\n", fingerprint: fingerprint(blob) };
}
// RFC 8410 OneAsymmetricKey v2 (the layout ring writes), from a Node key pair.
function pkcs8v2() {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const seed = Buffer.from(privateKey.export({ format: "jwk" }).d, "base64url");
  const pub = Buffer.from(publicKey.export({ format: "jwk" }).x, "base64url");
  const der = Buffer.concat([Buffer.from("3053020101300506032b657004220420", "hex"), seed, Buffer.from("a123032100", "hex"), pub]);
  return { der, fingerprint: fingerprint(pub) };
}
function keyFingerprint(path) {
  try {
    const key = createPublicKey(createPrivateKey({ key: readFileSync(path), format: "der", type: "pkcs8" }));
    return fingerprint(Buffer.from(key.export({ format: "jwk" }).x, "base64url"));
  } catch { return undefined; }
}

// Plain HTTP/WebSocket relay standing between the Hand and the Worker, i.e. the
// Hand's network path. It records the device-credential exchanges it carries
// (credentials only as digests in evidence; the presented values stay in memory
// for the expired-credential replay) and can drop every live connection, as a
// network partition or an edge restart would. It never alters traffic.
async function startRelay() {
  const sockets = new Set(), exchanges = [], issued = new Map(), presented = new Map();
  let upstream;
  const ref = credential => createHash("sha256").update(credential).digest("hex").slice(0, 12);
  const track = socket => { sockets.add(socket); socket.once("close", () => sockets.delete(socket)); };
  const devicePath = /^\/v1\/hand-devices\/[^/]+\/[^/]+\/(challenges|credentials|rotate|ssh-host-keys)$/;
  const deviceBearer = headers => /^Bearer (ncxhd1\.\S+)$/.exec(headers.authorization ?? "")?.[1];
  const server = createServer((request, response) => {
    const entry = { at: Date.now(), method: request.method, path: request.url.replace(/\?.*$/, "") };
    const operation = devicePath.exec(entry.path)?.[1];
    if (operation) entry.device_operation = operation;
    const bearer = deviceBearer(request.headers);
    if (bearer) entry.credential_ref = ref(bearer);
    exchanges.push(entry);
    const sent = [];
    if (operation) request.on("data", chunk => sent.push(chunk));
    const target = new URL(upstream);
    const forward = httpRequest({ host: target.hostname, port: target.port, method: request.method, path: request.url, headers: request.headers }, reply => {
      entry.status = reply.statusCode; entry.responded_at = Date.now();
      response.writeHead(reply.statusCode, reply.headers);
      const received = [];
      if (operation) reply.on("data", chunk => received.push(chunk));
      reply.on("end", () => {
        if (!operation) return;
        try { const body = JSON.parse(Buffer.concat(sent).toString() || "{}"); if (typeof body.purpose === "string") entry.purpose = body.purpose; } catch { /* not JSON */ }
        try {
          const raw = Buffer.concat(received), encoding = String(reply.headers["content-encoding"] ?? "").trim();
          const decoded = encoding === "gzip" ? gunzipSync(raw) : encoding === "br" ? brotliDecompressSync(raw) : encoding === "deflate" ? inflateSync(raw) : raw;
          const body = JSON.parse(decoded.toString());
          if (typeof body.error === "string") entry.error = body.error;
          if (typeof body.credential === "string") {
            const expires = typeof body.expires_at === "number" ? body.expires_at : Date.parse(body.expires_at);
            Object.assign(entry, { issued_ref: ref(body.credential), expires_at: expires, key_version: body.key_version });
            issued.set(entry.issued_ref, entry);
          }
        } catch (error) { entry.observe_error = String(error?.message ?? error); }
      });
      reply.pipe(response);
    });
    forward.on("error", () => response.destroy());
    request.pipe(forward);
  });
  server.on("connection", track);
  server.on("upgrade", (request, socket, head) => {
    const entry = { at: Date.now(), method: request.method, path: request.url.replace(/\?.*$/, ""), upgrade: true };
    const bearer = deviceBearer(request.headers);
    if (bearer) {
      entry.credential_ref = ref(bearer);
      const { authorization: _, ...headers } = request.headers;
      presented.set(entry.credential_ref, { credential: bearer, path: request.url, headers });
    }
    exchanges.push(entry);
    const target = new URL(upstream);
    const relay = connect(Number(target.port), target.hostname);
    track(relay);
    const close = () => { entry.closed_at ??= Date.now(); relay.destroy(); socket.destroy(); };
    relay.on("connect", () => {
      let lines = request.method + " " + request.url + " HTTP/1.1\r\n";
      for (let index = 0; index < request.rawHeaders.length; index += 2) lines += request.rawHeaders[index] + ": " + request.rawHeaders[index + 1] + "\r\n";
      relay.write(lines + "\r\n");
      if (head?.length) relay.write(head);
      relay.once("data", chunk => { entry.status = Number(/^HTTP\/1\.1 (\d{3})/.exec(chunk.toString("latin1"))?.[1]); entry.responded_at = Date.now(); });
      relay.pipe(socket); socket.pipe(relay);
    });
    for (const side of [relay, socket]) { side.on("error", close); side.on("close", close); }
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const base = "http://127.0.0.1:" + server.address().port;
  return {
    base, exchanges, issued,
    set upstream(value) { upstream = value; },
    /** The headers and credential a tool-host upgrade presented (in memory only). */
    presented: credentialRef => presented.get(credentialRef),
    /** Destroy every live connection through the relay; returns how many were open. */
    drop() { const live = [...sockets]; for (const socket of live) socket.destroy(); return live.length; },
    stop() { for (const socket of sockets) socket.destroy(); return new Promise(resolve => server.close(resolve)); },
  };
}

test("the shipped CLI Hand enrolls a device key, rotates it crash-safely and stops on revocation", { timeout: 600_000 }, async t => {
  if (!existsSync(binary) || !existsSync(handExecutable)) {
    // CI builds the binaries first: a missing binary there is a failure, never a green skip.
    if (process.env.CI) assert.fail("missing " + binary + " or " + handExecutable);
    t.skip("build the CLI and Hand first: cargo build --locked -p nanocodex-bin --bin nanocodex -p nanocodex-hand-daemon --bin nanocodex-hand (or set NANOCODEX_BIN / NANOCODEX_HAND_EXECUTABLE)");
    return;
  }
  mkdirSync(evidence, { recursive: true });
  // The CLI and daemon reach the Worker through the relay; the Worker sees the relay as its public origin.
  const relay = await startRelay();
  t.after(() => relay.stop());
  t.after(() => writeFileSync(join(evidence, "relay-exchanges.json"), JSON.stringify(relay.exchanges, null, 2) + "\n"));
  const server = await startHandDeviceServer({ output: evidence, ttlSeconds, publicOrigin: relay.base });
  relay.upstream = server.base;
  t.after(() => server.stop());
  const home = mkdtempSync(join(tmpdir(), "hand-device-home-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const sshDirectory = join(home, "ssh-host-keys"), emptySsh = join(home, "no-ssh-host-keys");
  mkdirSync(sshDirectory); mkdirSync(emptySsh);
  const hostKey = sshHostKey();
  writeFileSync(join(sshDirectory, "ssh_host_ed25519_key.pub"), hostKey.line);
  const environment = (overrides = {}) => ({
    PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: join(home, ".config"), XDG_STATE_HOME: join(home, ".local/state"),
    XDG_CACHE_HOME: join(home, ".cache"), NANOCODEX_MANAGED_URL: relay.base, NANOCODEX_API_KEY: server.apiKey,
    NANOCODEX_HAND_SSH_HOST_KEY_DIR: sshDirectory, NANOCODEX_HAND_EXECUTABLE: handExecutable, RUST_LOG: "nanocodex2=info", NO_COLOR: "1", ...overrides,
  });
  const command = args => [...wrapper, binary, ...args];
  const secrets = text => String(text).replaceAll(server.apiKey, "[api-key]").replace(/ncxh[dg]1\.[A-Za-z0-9._:-]+/g, "[device-credential]");

  // Asynchronous: the relay serving the CLI runs on this event loop.
  async function cli(step, args, overrides) {
    const [file, ...rest] = command(["hand", "devices", ...args]);
    const child = spawn(file, rest, { env: environment(overrides), stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "", stderr = "";
    child.stdout.setEncoding("utf8").on("data", chunk => { stdout += chunk; });
    child.stderr.setEncoding("utf8").on("data", chunk => { stderr += chunk; });
    const timer = setTimeout(() => child.kill("SIGKILL"), 60_000);
    const status = await new Promise(resolve => child.on("close", code => resolve(code)));
    clearTimeout(timer);
    const result = { status, stdout, stderr };
    record(step, { command: ["nanocodex", "hand", "devices", ...args].join(" "), status: result.status, stdout: secrets(result.stdout), stderr: secrets(result.stderr).slice(-4000) });
    return result;
  }
  let daemons = 0;
  function daemon(step, overrides) {
    const index = ++daemons;
    const [file, ...rest] = command(["__device-hand", "--daemon"]);
    const child = spawn(file, rest, { env: environment(overrides), stdio: ["ignore", "pipe", "pipe"] });
    const lines = [], waiters = new Set();
    let buffer = "", exitCode;
    const wake = () => { for (const waiter of [...waiters]) waiter(); };
    child.stdout.on("data", chunk => {
      buffer += chunk; let newline;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline); buffer = buffer.slice(newline + 1);
        appendFileSync(join(evidence, "daemon-" + index + ".stdout.jsonl"), secrets(line) + "\n");
        try { lines.push(JSON.parse(line)); } catch { continue; }
        wake();
      }
    });
    child.stderr.on("data", chunk => appendFileSync(join(evidence, "daemon-" + index + ".stderr.log"), secrets(chunk)));
    const exited = new Promise(resolve => child.on("exit", (code, signal) => {
      exitCode = { code, signal }; record(step + ".exit", exitCode); resolve(exitCode); wake();
    }));
    async function waitFor(predicate, timeout = 90_000) {
      const deadline = Date.now() + timeout;
      for (;;) {
        const found = lines.find(predicate);
        if (found) return found;
        if (exitCode || Date.now() > deadline) return undefined;
        await new Promise(resolve => { const done = () => { waiters.delete(done); resolve(); }; waiters.add(done); setTimeout(done, 500); });
      }
    }
    async function stop() {
      if (!exitCode) child.kill("SIGTERM");
      if (!await Promise.race([exited, delay(20_000)])) child.kill("SIGKILL");
      return exited;
    }
    t.after(() => { if (!exitCode) child.kill("SIGKILL"); });
    record(step, { command: "nanocodex __device-hand --daemon", index });
    return { lines, waitFor, stop, exited, get exitCode() { return exitCode; } };
  }
  async function api(path, init = {}) {
    const response = await fetch(new URL(path, server.base), { ...init, headers: { authorization: "Bearer " + server.apiKey, origin: server.origin, ...init.headers } });
    return { status: response.status, body: await response.json().catch(() => null) };
  }
  const devices = async () => (await api("/v1/account/hand-devices")).body;
  const connections = machine => server.observations.filter(entry => entry?.type === "hand.connection" && entry.machine_id === machine && entry.auth_mode);
  function stateDirectory() {
    const hands = join(home, ".nanocodex/hands");
    const found = readdirSync(hands).map(name => join(hands, name)).filter(path => existsSync(join(path, "device.json")));
    assert.equal(found.length, 1, "exactly one enrolled Hand state directory");
    return found[0];
  }
  async function eventually(step, check, timeout = 30_000) {
    const deadline = Date.now() + timeout; let last;
    for (;;) {
      last = await check();
      if (last.ok) { record(step, { observed: last.value }); return last.value; }
      if (Date.now() > deadline) break;
      await delay(500);
    }
    record(step, { failed: true, observed: last?.value });
    assert.fail(step + ": " + JSON.stringify(last?.value));
  }
  // A model-facing exec_command on the device-authenticated Hand, in its real workspace.
  async function tool(step, machine) {
    const result = await server.callHandTool({ machineId: machine, cmd: "echo device-hand-$((40+2))", workdir: join(home, "Nanocodex") });
    record(step, { result });
    assert.equal(result.status, 200, JSON.stringify(result));
    assert.equal(result.body?.success, true, JSON.stringify(result.body));
    assert.match(String(result.body.output), /device-hand-42/);
  }

  // 1. First run: key created privately, device enrolled, tool-host published with device credentials.
  let hand = daemon("first-run");
  assert.ok(await hand.waitFor(line => line.status === "connected"), "daemon reports connected: " + JSON.stringify(hand.lines.slice(-3)));
  const directory = stateDirectory(), key = join(directory, "device-key.v1");
  const state = () => JSON.parse(readFileSync(join(directory, "device.json"), "utf8"));
  assert.equal(mode(directory), 0o700); assert.equal(mode(key), 0o600);
  assert.ok(lstatSync(key).isFile() && !lstatSync(key).isSymbolicLink());
  assert.equal(statSync(key).uid, process.getuid());
  const enrolled = state();
  record("first-run.state", { directory_mode: mode(directory).toString(8), key_mode: mode(key).toString(8), device: enrolled });
  assert.equal(enrolled.owner_id, server.owner); assert.equal(enrolled.key_version, 1); assert.equal(enrolled.origin, server.origin);
  const machine = enrolled.machine_id, firstDevice = enrolled.device_id;
  const local = keyFingerprint(key);
  if (local) assert.equal(enrolled.fingerprint, local, "device.json fingerprint is the key file's public key");
  assert.doesNotMatch(JSON.stringify(enrolled), /ncxhd1|PRIVATE|challenge/i, "device.json holds no secrets");
  await eventually("first-run.listed-active-with-host-key", async () => {
    const listed = (await devices())?.data?.find(device => device.id === firstDevice);
    return { ok: listed?.status === "active" && listed.machine_id === machine && listed.fingerprint === enrolled.fingerprint
      && listed.ssh_host_keys?.some(entry => entry.fingerprint === hostKey.fingerprint), value: listed };
  });
  await eventually("first-run.device-tool-host", async () => {
    const seen = connections(machine);
    return { ok: seen.some(entry => entry.auth_mode === "device_key" && entry.surface === "tool_host"), value: seen.slice(-5) };
  });
  const listedByCli = await cli("first-run.cli-list", ["list", "--json"]);
  assert.equal(listedByCli.status, 0, listedByCli.stderr);
  assert.ok(JSON.parse(listedByCli.stdout).data.some(device => device.id === firstDevice));
  await tool("first-run.tool-call", machine);

  // 2. Credentials expire every ttlSeconds: the Hand stays published across >=3 expiries and still executes tools.
  await delay((ttlSeconds * 3 + 5) * 1000);
  assert.equal(hand.exitCode, undefined, "daemon is still running after three credential lifetimes");
  assert.ok(!hand.lines.some(line => line.status === "error"));
  await tool("expiry.tool-call-after-3-lifetimes", machine);

  // 2b. Automatic renewal on reconnect, without rotation or restart: after each of
  // three expiries the network drops the live connection; the daemon reconnects
  // by itself with a NEW credential from a fresh challenge->signature, the expired
  // one is refused (401) when replayed, and tools work again on the new socket.
  const toolHostPath = "/v1/account/tool-host";
  const deviceToolHost = () => connections(machine).filter(entry => entry.auth_mode === "device_key" && entry.surface === "tool_host");
  const upgrades = (from = 0) => relay.exchanges.slice(from).filter(entry => entry.upgrade && entry.path === toolHostPath && entry.status === 101 && entry.credential_ref);
  const replay = saved => new Promise((resolve, reject) => {
    const target = new URL(server.base);
    const request = httpRequest({ host: target.hostname, port: target.port, method: "GET", path: saved.path,
      headers: { ...saved.headers, host: target.host, "sec-websocket-key": randomBytes(16).toString("base64"), authorization: "Bearer " + saved.credential } });
    request.on("response", response => {
      const body = [];
      response.on("data", chunk => body.push(chunk));
      response.on("end", () => resolve({ status: response.statusCode, body: secrets(Buffer.concat(body).toString()).slice(0, 300) }));
    });
    request.on("upgrade", (response, socket) => { socket.destroy(); resolve({ status: response.statusCode }); });
    request.on("error", reject);
    request.setTimeout(15_000, () => request.destroy(new Error("expired-credential replay timed out")));
    request.end();
  });
  const renewal = { ttl_seconds: ttlSeconds, device_id: firstDevice, machine_id: machine, daemon: daemons, cycles: [] };
  const daemonIndex = daemons, firstConnection = connections(machine).length;
  try {
    for (let cycle = 1; cycle <= 3; cycle++) {
      const step = "renewal." + cycle;
      const live = upgrades().at(-1);
      assert.ok(live, "the Hand's tool-host connection runs through the relay");
      const liveCredential = relay.issued.get(live.credential_ref);
      assert.ok(liveCredential, "the live tool-host credential came from an observed challenge->signature issuance");
      const upgradesBefore = upgrades().length;
      // The live socket outlives its credential (no reconnect), and still serves tools.
      const remaining = liveCredential.expires_at + 1000 - Date.now();
      if (remaining > 0) await delay(remaining);
      assert.equal(upgrades().length, upgradesBefore, "the tool-host socket outlived its credential without reconnecting");
      await tool(step + ".tool-call-after-expiry-before-drop", machine);
      // The expired credential is refused by the service.
      const replayed = await replay(relay.presented(live.credential_ref));
      record(step + ".expired-credential-replay", { credential_ref: live.credential_ref, expires_at: liveCredential.expires_at, result: replayed });
      assert.equal(replayed.status, 401, "expired credential replay on " + toolHostPath + ": " + JSON.stringify(replayed));
      // The network drops every live connection.
      const mark = relay.exchanges.length, observedBefore = deviceToolHost().length, issuedBefore = relay.issued.size;
      const droppedAt = Date.now(), dropped = relay.drop();
      record(step + ".drop", { dropped_sockets: dropped, dropped_at: droppedAt });
      assert.ok(dropped > 0, "the relay held the Hand's live connections");
      const reconnected = await eventually(step + ".reconnected-with-new-credential", async () => {
        const after = relay.exchanges.slice(mark), upgrade = upgrades(mark)[0];
        const issuance = upgrade && relay.issued.get(upgrade.credential_ref);
        const challenge = issuance && after.find(entry => entry.device_operation === "challenges" && entry.purpose === "credential"
          && entry.status === 201 && entry.at <= issuance.at);
        const observed = deviceToolHost().slice(observedBefore);
        return { ok: Boolean(upgrade && issuance && challenge && issuance.at >= droppedAt && upgrade.credential_ref !== live.credential_ref
          && issuance.status === 201 && issuance.key_version === 1 && observed.some(entry => entry.device_id === firstDevice && entry.key_version === 1)),
        value: { upgrade, issuance, challenge, observed, recent: after.slice(-12) } };
      }, 90_000);
      assert.equal(hand.exitCode, undefined, "the same daemon process reconnected");
      assert.equal(daemons, daemonIndex, "no daemon restart");
      assert.equal(state().key_version, 1, "no rotation");
      assert.ok(reconnected.issuance.expires_at > reconnected.upgrade.at, "the reconnect credential was valid when presented");
      await tool(step + ".tool-call-after-reconnect", machine);
      renewal.cycles.push({ cycle,
        expired_credential_ref: live.credential_ref, expired_credential_issued_at: liveCredential.at, expired_credential_expires_at: liveCredential.expires_at,
        expired_credential_replay_status: replayed.status, dropped_at: droppedAt, dropped_sockets: dropped,
        challenge_at: reconnected.challenge.at, credential_issued_at: reconnected.issuance.responded_at,
        new_credential_ref: reconnected.upgrade.credential_ref, new_credential_expires_at: reconnected.issuance.expires_at,
        reconnect_upgrade_at: reconnected.upgrade.at, reconnect_accepted_at: reconnected.upgrade.responded_at,
        issuances_before_drop: issuedBefore, issuances_after_reconnect: relay.issued.size,
        server_device_tool_host_connections_before: observedBefore, server_device_tool_host_connections_after: deviceToolHost().length,
        tool_call_after_reconnect_ok_at: Date.now() });
    }
    const rejected = connections(machine).slice(firstConnection).filter(entry => entry.outcome === "rejected" || entry.auth_mode === "account_api_key");
    renewal.rejected_or_legacy_connections = rejected;
    assert.deepEqual(rejected, [], "no rejected or account-key reconnect attempt");
    assert.ok(!hand.lines.some(line => line.status === "error"));
  } finally {
    writeFileSync(join(evidence, "renewal.json"), JSON.stringify(renewal, null, 2) + "\n");
  }

  // 3. CLI rotation while connected: the service closes old-key sockets; the Hand reconnects with the new key and re-attests.
  const rotated = await cli("rotate", ["rotate"]);
  assert.equal(rotated.status, 0, rotated.stderr);
  assert.equal(JSON.parse(rotated.stdout).key_version, 2);
  assert.equal(state().key_version, 2); assert.notEqual(state().fingerprint, enrolled.fingerprint);
  assert.ok(!existsSync(key + ".pending")); assert.equal(mode(key), 0o600);
  await eventually("rotate.listed-v2-reattested", async () => {
    const listed = (await devices())?.data?.find(device => device.id === firstDevice);
    return { ok: listed?.key_version === 2 && listed.fingerprint === state().fingerprint
      && listed.ssh_host_keys?.some(entry => entry.fingerprint === hostKey.fingerprint), value: listed };
  });
  await eventually("rotate.device-reconnected-v2", async () => {
    const seen = connections(machine);
    return { ok: seen.some(entry => entry.auth_mode === "device_key" && entry.key_version === 2 && entry.surface === "tool_host"), value: seen.slice(-5) };
  }, 60_000);
  await tool("rotate.tool-call", machine);
  await hand.stop();

  // 4. Crash after the service accepted a rotation but before promotion: the service is at v3, the
  // disk still has the v2 key and v2 device.json (with a stale attestation cache) plus the v3 key pending.
  const v2Key = readFileSync(key), v2State = readFileSync(join(directory, "device.json"));
  const third = await cli("crash.rotate-without-attestation", ["rotate"], { NANOCODEX_HAND_SSH_HOST_KEY_DIR: emptySsh });
  assert.equal(third.status, 0, third.stderr);
  const v3Fingerprint = state().fingerprint;
  renameSync(key, key + ".pending");
  writeFileSync(key, v2Key, { mode: 0o600 }); chmodSync(key, 0o600);
  writeFileSync(join(directory, "device.json"), v2State, { mode: 0o600 });
  assert.equal(state().key_version, 2);
  const beforeRecovery = (await devices())?.data?.find(device => device.id === firstDevice);
  record("crash.service-before-recovery", { device: beforeRecovery, local: state() });
  assert.equal(beforeRecovery.key_version, 3);
  assert.deepEqual(beforeRecovery.ssh_host_keys ?? [], [], "service dropped attestations on rotation");
  hand = daemon("crash.recovery-start");
  assert.ok(await hand.waitFor(line => line.status === "connected"), "recovered daemon connects");
  assert.equal(state().key_version, 3); assert.equal(state().fingerprint, v3Fingerprint);
  assert.ok(!existsSync(key + ".pending"), "pending key promoted");
  await eventually("crash.reattested-after-recovery", async () => {
    const listed = (await devices())?.data?.find(device => device.id === firstDevice);
    return { ok: listed?.ssh_host_keys?.some(entry => entry.fingerprint === hostKey.fingerprint), value: listed };
  });
  await hand.stop();

  // 5. A pending key the service never received: the Hand keeps its current key; the next rotation retries with it.
  const pending = pkcs8v2();
  writeFileSync(key + ".pending", pending.der, { mode: 0o600 }); chmodSync(key + ".pending", 0o600);
  hand = daemon("unlanded.start");
  assert.ok(await hand.waitFor(line => line.status === "connected"), "daemon connects with its current key");
  assert.equal(state().key_version, 3); assert.equal(state().fingerprint, v3Fingerprint);
  assert.ok(existsSync(key + ".pending"), "an unaccepted pending key is kept for the next rotation");
  await hand.stop();
  const retried = await cli("unlanded.rotate", ["rotate"]);
  assert.equal(retried.status, 0, retried.stderr);
  assert.equal(state().key_version, 4);
  assert.equal(state().fingerprint, pending.fingerprint, "rotation retried with the pending key");
  assert.ok(!existsSync(key + ".pending"));

  // 6. Unsafe key files are refused; nothing publishes.
  chmodSync(key, 0o644);
  hand = daemon("refuse.mode-0644");
  await hand.exited;
  assert.notEqual(hand.exitCode.code, 0);
  assert.match(JSON.stringify(hand.lines), /mode 0600/);
  chmodSync(key, 0o600);
  renameSync(key, key + ".real"); symlinkSync(key + ".real", key);
  hand = daemon("refuse.symlink");
  await hand.exited;
  assert.notEqual(hand.exitCode.code, 0);
  assert.match(JSON.stringify(hand.lines), /symbolic link/);
  unlinkSync(key); renameSync(key + ".real", key);

  // 7. Revocation through the CLI while connected: the Hand stops with an actionable error and never uses the account key.
  hand = daemon("revoke.start");
  assert.ok(await hand.waitFor(line => line.status === "connected"));
  const revoked = await cli("revoke", ["revoke", firstDevice]);
  assert.equal(revoked.status, 0, revoked.stderr);
  assert.equal(JSON.parse(revoked.stdout).status, "revoked");
  const stopped = await Promise.race([hand.exited, delay(90_000)]);
  assert.ok(stopped, "daemon stops after revocation");
  assert.notEqual(stopped.code, 0);
  assert.match(JSON.stringify(hand.lines), /hand_reenroll_required/);
  assert.match(JSON.stringify(hand.lines), /will not fall back to the account API key/);
  const after = await server.callHandTool({ machineId: machine, cmd: "echo revoked-$((40+2))", workdir: join(home, "Nanocodex") });
  record("revoke.tool-call-refused", { result: after });
  assert.notEqual(after.body?.success, true);
  assert.doesNotMatch(JSON.stringify(after.body), /revoked-42/);
  hand = daemon("revoke.restart");
  await hand.exited;
  assert.notEqual(hand.exitCode.code, 0);
  assert.match(JSON.stringify(hand.lines), /hand_reenroll_required/);
  const legacy = connections(machine).filter(entry => entry.auth_mode === "account_api_key");
  record("revoke.no-legacy", { legacy, connections: connections(machine) });
  assert.deepEqual(legacy, [], "no account API key publication was ever attempted for the device machine");

  // 8. Recovery: explicit local re-enrollment creates a new device for the same machine.
  const reenrolled = await cli("reenroll", ["reenroll"]);
  assert.equal(reenrolled.status, 0, reenrolled.stderr);
  assert.ok(!existsSync(join(directory, "device.json")) && !existsSync(key));
  hand = daemon("reenroll.start");
  assert.ok(await hand.waitFor(line => line.status === "connected"), "re-enrolled daemon connects");
  const fresh = state();
  assert.notEqual(fresh.device_id, firstDevice); assert.equal(fresh.machine_id, machine); assert.equal(fresh.key_version, 1);
  const listing = await devices();
  record("reenroll.list", { listing });
  assert.equal(listing.data.find(device => device.id === firstDevice)?.status, "revoked");
  assert.equal(listing.data.find(device => device.id === fresh.device_id)?.status, "active");
  await tool("reenroll.tool-call", machine);
  await hand.stop();
});
