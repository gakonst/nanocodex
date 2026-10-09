// Real shipped CLI Hand daemon against the real managed Worker (Miniflare):
// first-run device key creation and enrollment, Hand-only device credentials
// across expiries, CLI rotation and crash recovery in both directions, SSH host
// key (re-)attestation, key-file refusal, and revocation without any fallback
// to the account API key. Only the account principal and clocks are fixtures.
//
// Requires the built binaries: cargo build --locked -p nanocodex-bin --bin nanocodex --bin nanocodex-hand
// NANOCODEX_BIN / NANOCODEX_HAND_EXECUTABLE override their paths. Evidence: output/hand-device-keys/cli-journey/<ts>/.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, randomBytes } from "node:crypto";
import { appendFileSync, chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, statSync, symlinkSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
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

test("the shipped CLI Hand enrolls a device key, rotates it crash-safely and stops on revocation", { timeout: 600_000 }, async t => {
  if (!existsSync(binary) || !existsSync(handExecutable)) {
    t.skip("build the CLI and Hand first: cargo build --locked -p nanocodex-bin --bin nanocodex --bin nanocodex-hand (or set NANOCODEX_BIN / NANOCODEX_HAND_EXECUTABLE)");
    return;
  }
  mkdirSync(evidence, { recursive: true });
  const server = await startHandDeviceServer({ output: evidence, ttlSeconds });
  t.after(() => server.stop());
  const home = mkdtempSync(join(tmpdir(), "hand-device-home-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const sshDirectory = join(home, "ssh-host-keys"), emptySsh = join(home, "no-ssh-host-keys");
  mkdirSync(sshDirectory); mkdirSync(emptySsh);
  const hostKey = sshHostKey();
  writeFileSync(join(sshDirectory, "ssh_host_ed25519_key.pub"), hostKey.line);
  const environment = (overrides = {}) => ({
    PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: join(home, ".config"), XDG_STATE_HOME: join(home, ".local/state"),
    XDG_CACHE_HOME: join(home, ".cache"), NANOCODEX_MANAGED_URL: server.base, NANOCODEX_API_KEY: server.apiKey,
    NANOCODEX_HAND_SSH_HOST_KEY_DIR: sshDirectory, NANOCODEX_HAND_EXECUTABLE: handExecutable, RUST_LOG: "nanocodex2=info", NO_COLOR: "1", ...overrides,
  });
  const command = args => [...wrapper, binary, ...args];
  const secrets = text => String(text).replaceAll(server.apiKey, "[api-key]").replace(/ncxh[dg]1\.[A-Za-z0-9._:-]+/g, "[device-credential]");

  function cli(step, args, overrides) {
    const [file, ...rest] = command(["hand", "devices", ...args]);
    const result = spawnSync(file, rest, { env: environment(overrides), encoding: "utf8", timeout: 60_000 });
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
  const listedByCli = cli("first-run.cli-list", ["list", "--json"]);
  assert.equal(listedByCli.status, 0, listedByCli.stderr);
  assert.ok(JSON.parse(listedByCli.stdout).data.some(device => device.id === firstDevice));
  await tool("first-run.tool-call", machine);

  // 2. Credentials expire every ttlSeconds: the Hand stays published across >=3 expiries and still executes tools.
  await delay((ttlSeconds * 3 + 5) * 1000);
  assert.equal(hand.exitCode, undefined, "daemon is still running after three credential lifetimes");
  assert.ok(!hand.lines.some(line => line.status === "error"));
  await tool("expiry.tool-call-after-3-lifetimes", machine);

  // 3. CLI rotation while connected: the service closes old-key sockets; the Hand reconnects with the new key and re-attests.
  const rotated = cli("rotate", ["rotate"]);
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
  const third = cli("crash.rotate-without-attestation", ["rotate"], { NANOCODEX_HAND_SSH_HOST_KEY_DIR: emptySsh });
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
  const retried = cli("unlanded.rotate", ["rotate"]);
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
  const revoked = cli("revoke", ["revoke", firstDevice]);
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
  const reenrolled = cli("reenroll", ["reenroll"]);
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
