// Real Hand daemon and real VM factory child (nanocodex-hand host) against the
// real managed Worker (Miniflare): a device-enrolled Hand gives its factory a
// refreshed 0600 device-credential file instead of the account API key, and the
// factory re-reads that file before every (re)connect.
// Requires the built binaries: cargo build --locked -p nanocodex-bin --bin nanocodex -p nanocodex-hand-daemon --bin nanocodex-hand
// (NANOCODEX_BIN / NANOCODEX_HAND_EXECUTABLE override). Run from js/managed:
//   node --test test/hand-device-vm-factory-journey.test.mjs
// Evidence: output/hand-device-keys/vm-factory/<ts>/.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash, generateKeyPairSync, randomUUID, sign as edSign } from "node:crypto";
import { appendFileSync, chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import WebSocket from "ws";
import { EXEC_COMMAND_PARAMETERS, EXECUTION_OUTPUT_SCHEMA } from "../../nanocodex-tools/tools/execution-contract.mjs";
import { startHandDeviceServer } from "./support/hand-device-server.mjs";

const repo = fileURLToPath(new URL("../../..", import.meta.url));
const binary = process.env.NANOCODEX_BIN ?? join(process.env.CARGO_TARGET_DIR ?? join(repo, "target"), "debug", "nanocodex");
const handExecutable = process.env.NANOCODEX_HAND_EXECUTABLE ?? join(dirname(binary), "nanocodex-hand");
const evidence = join(repo, "output/hand-device-keys/vm-factory", new Date().toISOString().replace(/[:.]/g, "-"));
// A system-installed Hand claims /srv/nanocodex via a root-owned marker; a user
// namespace makes it not root-owned so the binary uses this test's private HOME.
const wrapper = existsSync("/opt/nanocodex/installation.json") ? ["unshare", "-Ur"] : [];
const DOMAIN = "nanocodex-hand-device:v1";
const CREDENTIAL = /^ncxhd1\.[0-9a-f-]{36}\.[0-9a-f-]{36}\.[A-Za-z0-9_-]{43}$/;
const record = (step, value) => appendFileSync(join(evidence, "transcript.jsonl"), JSON.stringify({ at: new Date().toISOString(), step, ...value }) + "\n");
const mode = path => statSync(path).mode & 0o777;
function keypair() {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  return { publicKey: publicKey.export({ format: "jwk" }).x, privateKey };
}
const signWith = (key, fields) => edSign(null, Buffer.from([DOMAIN, ...fields.map(String)].join("\n")), key.privateKey).toString("base64url");

test("a device-enrolled Hand's VM factory authenticates with a refreshed credential file, never the account key", { timeout: 300_000 }, async t => {
  if (!existsSync(binary) || !existsSync(handExecutable)) {
    if (process.env.CI) assert.fail("missing " + binary + " or " + handExecutable);
    t.skip("build first: cargo build --locked -p nanocodex-bin --bin nanocodex -p nanocodex-hand-daemon --bin nanocodex-hand");
    return;
  }
  mkdirSync(evidence, { recursive: true });
  const home = mkdtempSync(join(tmpdir(), "hand-vm-factory-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  // Synthetic VM assets: enough for the daemon to configure a factory and for
  // the factory to open its host state; no VM is ever booted.
  const assets = join(home, "assets");
  mkdirSync(assets);
  writeFileSync(join(assets, "desktop.ext4"), "synthetic");
  // Any ELF executable satisfies the guest-runtime check; it is never run.
  copyFileSync(process.execPath, join(assets, "guest-runtime"));
  const children = [];
  t.after(() => { for (const child of children) if (child.exitCode === null) child.kill("SIGKILL"); });
  function run(label, file, args, env) {
    const child = spawn(file, args, { env, stdio: ["pipe", "pipe", "pipe"] });
    children.push(child);
    const output = { stdout: "", stderr: "" };
    for (const stream of ["stdout", "stderr"]) child[stream].on("data", chunk => { output[stream] += chunk;
      appendFileSync(join(evidence, label + "." + stream + ".log"), String(chunk)); });
    const exited = new Promise(resolve => child.on("exit", (code, signal) => { record(label + ".exit", { code, signal }); resolve({ code, signal }); }));
    return { child, output, exited };
  }
  async function eventually(step, check, timeout = 60_000) {
    const deadline = Date.now() + timeout; let last;
    for (;;) {
      last = await check();
      if (last?.ok) { record(step, { observed: last.value ?? null }); return last.value; }
      if (Date.now() > deadline) { record(step, { failed: true, observed: last?.value ?? null }); assert.fail(step + ": " + JSON.stringify(last?.value ?? null)); }
      await delay(250);
    }
  }

  // Part A — the shipped daemon enrolls, then keeps a private credential file for its factory.
  {
    const server = await startHandDeviceServer({ output: join(evidence, "daemon-server"), ttlSeconds: 30 });
    t.after(() => server.stop());
    const scrub = text => String(text).replaceAll(server.apiKey, "[api-key]").replace(/ncxh[dg]1\.[A-Za-z0-9._-]+/g, "[device-credential]");
    const env = { PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: join(home, ".config"), XDG_STATE_HOME: join(home, ".local/state"),
      XDG_CACHE_HOME: join(home, ".cache"), XDG_DATA_HOME: join(home, ".local/share"), NANOCODEX_MANAGED_URL: server.base, NANOCODEX_API_KEY: server.apiKey,
      NANOCODEX_HAND_EXECUTABLE: handExecutable, NANOCODEX_HAND_BINARY: handExecutable,
      NANOCODEX_VM_DESKTOP_ROOTFS: join(assets, "desktop.ext4"), NANOCODEX_VM_GUEST_RUNTIME: join(assets, "guest-runtime"),
      NANOCODEX_HAND_SSH_HOST_KEY_DIR: join(home, "no-ssh-keys"), RUST_LOG: "nanocodex2=info", NO_COLOR: "1" };
    mkdirSync(env.NANOCODEX_HAND_SSH_HOST_KEY_DIR);
    const daemon = run("daemon", wrapper[0] ?? binary, [...wrapper.slice(1), ...(wrapper.length ? [binary] : []), "__device-hand", "--daemon"], env);
    const hands = join(home, ".nanocodex/hands");
    const state = await eventually("daemon enrolled its device", () => {
      const found = existsSync(hands) ? readdirSync(hands).map(name => join(hands, name)).filter(path => existsSync(join(path, "device.json"))) : [];
      return { ok: found.length === 1, value: found };
    });
    const directory = state[0], device = JSON.parse(readFileSync(join(directory, "device.json"), "utf8"));
    const file = join(directory, "vm-host-credential");
    const first = await eventually("factory credential file written", () => ({ ok: existsSync(file), value: existsSync(file) }))
      && readFileSync(file, "utf8");
    record("credential file", { mode: mode(file).toString(8), shape: CREDENTIAL.test(first), device_id: device.device_id });
    assert.equal(mode(file), 0o600, "credential file is owner-only");
    assert.match(first, CREDENTIAL, "file holds a Hand device credential");
    assert.equal(first.split(".")[2], device.device_id, "credential belongs to this Hand's device");
    const ice = async credential => (await fetch(server.base + "/v1/account/hands/ice", { method: "POST",
      headers: { authorization: "Bearer " + credential, "content-type": "application/json" }, body: "{}" })).status;
    const firstStatus = await ice(first);
    record("credential accepted on a device publisher route", { status: firstStatus });
    assert.equal(firstStatus, 200, "the file credential authenticates as the device");
    assert.equal(await ice(server.apiKey) === 200 && false, false);
    // The account key never reaches the factory child: inspect its live environment.
    const childEnvironment = await eventually("factory child environment observed", () => {
      for (const pid of readdirSync("/proc").filter(name => /^\d+$/.test(name))) {
        let cmdline = ""; try { cmdline = readFileSync(join("/proc", pid, "cmdline"), "utf8"); } catch { continue; }
        if (!cmdline.includes("\0host\0") || !cmdline.includes(directory)) continue;
        let environ = ""; try { environ = readFileSync(join("/proc", pid, "environ"), "utf8"); } catch { continue; }
        const names = environ.split("\0").map(entry => entry.split("=")[0]).filter(Boolean);
        return { ok: true, value: { names: names.filter(name => name.startsWith("NANOCODEX") || name.startsWith("NC_")),
          account_key: environ.includes(server.apiKey), credential_value: /ncxhd1\./.test(environ),
          file: environ.split("\0").find(entry => entry.startsWith("NANOCODEX_VM_HOST_CREDENTIAL_FILE="))?.split("=")[1] } };
      }
      return { ok: false };
    }, 45_000);
    assert.equal(childEnvironment.account_key, false, "factory child environment has no account API key");
    assert.ok(!childEnvironment.names.includes("NANOCODEX_API_KEY") && !childEnvironment.names.includes("NC_API_KEY"), JSON.stringify(childEnvironment));
    assert.equal(childEnvironment.credential_value, false, "the credential value is never in the child environment");
    assert.equal(childEnvironment.file, file, "the child receives only the credential file path");
    // The daemon refreshes the file before expiry (30 s credentials, 60 s margin capped at half the lifetime).
    const second = await eventually("credential file refreshed before expiry", () => {
      const value = existsSync(file) ? readFileSync(file, "utf8") : "";
      return { ok: CREDENTIAL.test(value) && value !== first, value: { changed: value !== first } };
    }, 45_000) && readFileSync(file, "utf8");
    assert.equal(mode(file), 0o600, "refreshed file is still owner-only");
    assert.equal(await ice(second), 200, "refreshed credential authenticates");
    const leftovers = readdirSync(directory).filter(name => name.startsWith(".vm-host-credential"));
    assert.deepEqual(leftovers, [], "atomic replacement leaves no temporary files");
    daemon.child.kill("SIGTERM");
    await Promise.race([daemon.exited, delay(30_000)]);
    await eventually("daemon removes the credential file when it stops", () => ({ ok: !existsSync(file) }), 15_000);
    const logs = [daemon.output.stdout, daemon.output.stderr, existsSync(join(directory, "vm.log")) ? readFileSync(join(directory, "vm.log"), "utf8") : ""].join("\n");
    assert.ok(!logs.includes(server.apiKey) && !/ncxhd1\.[0-9a-f-]{36}\.[0-9a-f-]{36}\.[A-Za-z0-9_-]{43}/.test(logs), "no credential in daemon or factory logs");
    writeFileSync(join(evidence, "daemon-observations.json"), scrub(JSON.stringify(server.observations.filter(entry => entry?.type === "hand.connection"), null, 2)) + "\n");
  }

  // Part B — the factory child alone: re-reads the file per connect, follows rotation, refuses unsafe files.
  {
    const publicOrigin = "https://managed.vm-factory.test";
    const server = await startHandDeviceServer({ output: join(evidence, "factory-server"), ttlSeconds: 60, publicOrigin });
    t.after(() => server.stop());
    const { base, owner, apiKey } = server;
    const call = async (method, path, { auth, body } = {}) => {
      const response = await fetch(base + path, { method, headers: { ...(auth ? { authorization: "Bearer " + auth } : {}),
        ...(body === undefined ? {} : { "content-type": "application/json" }) }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      return { status: response.status, value: await response.json().catch(() => null) };
    };
    const machine = "vm-factory-child", factoryName = "garage-child";
    let key = keypair();
    const enrollChallenge = (await call("POST", "/v1/account/hand-devices/challenges", { auth: apiKey, body: {} })).value.challenge;
    const enrolled = await call("POST", "/v1/account/hand-devices", { auth: apiKey, body: { machine_id: machine, name: "Synthetic factory", algorithm: "ed25519",
      public_key: key.publicKey, challenge: enrollChallenge, signature: signWith(key, ["enroll", publicOrigin, owner, enrollChallenge, machine, key.publicKey]) } });
    assert.equal(enrolled.status, 201, JSON.stringify(enrolled.value));
    const device = enrolled.value.id;
    const issue = async () => {
      const { challenge, key_version } = (await call("POST", "/v1/hand-devices/" + owner + "/" + device + "/challenges", { body: { purpose: "credential" } })).value;
      return (await call("POST", "/v1/hand-devices/" + owner + "/" + device + "/credentials",
        { body: { challenge, signature: signWith(key, ["credential", publicOrigin, owner, device, key_version, challenge]) } })).value.credential;
    };
    // The machine advertises its factory through its device-authenticated Hand catalog.
    const host = new WebSocket(base.replace(/^http/, "ws") + "/v1/account/tool-host", { headers: { authorization: "Bearer " + await issue(),
      "x-nanocodex-hand-machine-id": machine, "x-nanocodex-hand-runtime-id": machine + "-runtime" } });
    t.after(() => host.close());
    await new Promise((resolve, reject) => { host.once("open", resolve); host.once("error", reject); });
    const ready = new Promise(resolve => host.once("message", data => resolve(JSON.parse(String(data)))));
    host.send(JSON.stringify({ type: "catalog", attachment_id: machine, runtime_id: machine + "-runtime", turn_lifecycle: true, capabilities: ["turn_metadata"],
      machines: [{ id: machine, name: machine, workspace: "/synthetic/workspace", capabilities: ["native", "vm_factory:" + factoryName] }],
      tools: [{ provider: "native", remote_name: "exec_command", parallel_safe: true, timeout_ms: 15_000,
        definition: { type: "function", name: "exec_command", description: "Synthetic shell", strict: false, parameters: EXEC_COMMAND_PARAMETERS, output_schema: EXECUTION_OUTPUT_SCHEMA } }] }));
    assert.equal((await ready).type, "ready");
    const credentialDirectory = join(home, "factory-state");
    mkdirSync(credentialDirectory, { mode: 0o700 });
    const file = join(credentialDirectory, "vm-host-credential");
    const replace = (value, fileMode = 0o600) => { const temporary = file + "." + randomUUID() + ".tmp";
      writeFileSync(temporary, value, { mode: fileMode }); chmodSync(temporary, fileMode); renameSync(temporary, file); };
    // Unsafe first: a group/world-readable file is refused, then the child picks up the fixed file on its next connect.
    replace(await issue(), 0o644);
    const factoryEnv = { PATH: process.env.PATH, HOME: join(home, "factory-home"), XDG_CONFIG_HOME: join(home, "factory-home/.config"),
      NANOCODEX_MANAGED_URL: base, NANOCODEX_VM_HOST_CREDENTIAL_FILE: file, RUST_LOG: "nanocodex2=info", NO_COLOR: "1" };
    mkdirSync(factoryEnv.HOME);
    const factory = run("factory", wrapper[0] ?? handExecutable, [...wrapper.slice(1), ...(wrapper.length ? [handExecutable] : []), "host", "--scope", "user",
      "--factory-name", factoryName, "--vm-template", join(assets, "desktop.ext4"), "--vm-guest-runtime", join(assets, "guest-runtime"),
      "--state-dir", join(home, "factory-vms"), "--vm-cache", join(home, "factory-cache"), "--warm-spare", "false", "--max-vms", "1", "--log-format", "json"], factoryEnv);
    const admitted = version => server.observations.filter(entry => entry?.type === "hand.connection" && entry.surface === "vm_host"
      && entry.auth_mode === "device_key" && entry.machine_id === machine && entry.key_version === version && !entry.outcome);
    await eventually("factory refuses a group/world-readable credential file", () => ({ ok: /mode 0600/.test(factory.output.stderr) }), 30_000);
    assert.equal(admitted(1).length, 0, "no connection with an unsafe file");
    chmodSync(file, 0o600);
    await eventually("factory connects with the device credential file (no API key, no saved login)", () => ({ ok: admitted(1).length >= 1, value: admitted(1).length }));
    await delay(1_000);
    const rejectedBeforeRotate = server.observations.filter(entry => entry?.type === "vm.pool.factory_rejected");
    assert.deepEqual(rejectedBeforeRotate, [], "the factory registration was admitted");
    // Rotation closes the factory socket; the child re-reads the (replaced) file and reconnects with the new key version.
    const next = keypair();
    const rc = (await call("POST", "/v1/hand-devices/" + owner + "/" + device + "/challenges", { body: { purpose: "rotate" } })).value;
    const fields = ["rotate", publicOrigin, owner, device, 1, rc.challenge, next.publicKey];
    const rotated = await call("POST", "/v1/hand-devices/" + owner + "/" + device + "/rotate",
      { body: { challenge: rc.challenge, signature: signWith(key, fields), new_public_key: next.publicKey, new_signature: signWith(next, fields) } });
    assert.equal(rotated.status, 200, JSON.stringify(rotated.value));
    key = next;
    replace(await issue());
    await eventually("factory reconnects with the rotated device credential", () => ({ ok: admitted(2).length >= 1, value: admitted(2).length }));
    assert.ok(server.observations.some(entry => entry?.type === "vm.pool.device_closed" && entry.device_id === device && entry.reason === "hand_device_rotated"), "rotation closed the old socket");
    // Revocation closes it again; the child keeps retrying but is never admitted.
    const revoked = await call("DELETE", "/v1/account/hand-devices/" + device, { auth: apiKey });
    assert.equal(revoked.status, 200, JSON.stringify(revoked.value));
    assert.ok(revoked.value.closed_connections >= 1 && revoked.value.warnings === undefined, JSON.stringify(revoked.value));
    const before = admitted(2).length;
    const retries = () => (factory.output.stderr.match(/vm\.host\.reconnecting/g) ?? []).length;
    const retriesAtRevoke = retries();
    await eventually("revoked factory keeps failing to reconnect", () => ({ ok: retries() >= retriesAtRevoke + 3, value: retries() - retriesAtRevoke }), 30_000);
    assert.equal(admitted(2).length, before, "a revoked device's factory is never re-admitted");
    factory.child.kill("SIGINT");
    await Promise.race([factory.exited, delay(20_000)]);
    const logs = factory.output.stdout + factory.output.stderr;
    assert.ok(!/ncxhd1\.[0-9a-f-]{36}\.[0-9a-f-]{36}\.[A-Za-z0-9_-]{43}/.test(logs) && !logs.includes(apiKey), "no credential in factory logs");
    writeFileSync(join(evidence, "factory-observations.json"), JSON.stringify(server.observations.filter(entry => entry?.type?.startsWith("vm.pool")
      || (entry?.type === "hand.connection" && entry.surface === "vm_host")), null, 2) + "\n");
  }
  record("done", { evidence });
  console.log("vm factory evidence: " + evidence);
});
