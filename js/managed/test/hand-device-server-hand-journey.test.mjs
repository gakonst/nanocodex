// Server Hand / SSH recovery trusts the registered device identity without
// manual key management. One chained journey against the shipped surfaces:
//
//   real managed Worker (owner DO: grant mint, device enrollment, listing,
//   revocation, HandDeviceSshHostKeys entrypoint) + real egress Worker (Vault
//   credential broker DO, ssh.internal) in one workerd, a real OpenSSH sshd,
//   the shipped SERVER_HAND_INSTALL script delivered over Vault-pinned SSH, and
//   the shipped nanocodex-hand server-host publisher.
//
// Stand-ins for dependencies outside the behavior under test:
// - docker (support/server-hand/docker.cjs): the server's container runtime on
//   the SSH session's PATH. It records every invocation and runs the image
//   entrypoint /usr/local/bin/nanocodex-remote as the shipped nanocodex-hand,
//   mapping the container's bind mounts to host paths. The container's
//   /ssh-host-keys maps to this sshd's host key directory (the role of
//   /etc/ssh on a real server, which a test sshd cannot own).
// - labwc / wlr-randr (support/server-hand/labwc.cjs): the headless desktop
//   compositor the publisher starts before enrolling. The desktop media
//   surface (waymote) is excluded, so the screen is never published; device
//   enrollment, credentials and SSH host key attestation are the real paths.
// - DNS: sshd.example.com resolves to the loopback sshd (egress TCP fixture).
//
// Requires OpenSSH sshd and the built Hand:
//   cargo build --locked -p nanocodex-hand-daemon --bin nanocodex-hand
// NANOCODEX_HAND_EXECUTABLE / NANOCODEX_TEST_SSHD override their paths; in CI
// their absence fails the journey. Evidence:
// output/hand-device-keys/server-hand-journey/<ts>/ (or NANOCODEX_EVIDENCE_DIR).
import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { createHash, generateKeyPairSync, sign as edSign } from "node:crypto";
import { appendFileSync, chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer, connect as tcpConnect } from "node:net";
import { tmpdir, userInfo } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { startHandDeviceServer } from "./support/hand-device-server.mjs";

const repo = fileURLToPath(new URL("../../../", import.meta.url));
const egressRoot = join(repo, "js/egress/");
const standIns = fileURLToPath(new URL("./support/server-hand/", import.meta.url));
const SSHD = process.env.NANOCODEX_TEST_SSHD ?? "/usr/sbin/sshd";
const hand = process.env.NANOCODEX_HAND_EXECUTABLE
  ?? join(process.env.CARGO_TARGET_DIR ?? join(repo, "target"), "debug", "nanocodex-hand");
const TARGET = "sshd.example.com";
const REFERENCE = "lab";
const IMAGE = "registry.example/nanocodex-hand@sha256:" + "0".repeat(64);
const TTL_SECONDS = 10;
const DOMAIN = "nanocodex-hand-device:v1";
// A system-installed Hand on a developer machine claims /srv/nanocodex as the
// Hand home (root-owned /opt/nanocodex marker). A user namespace makes that
// marker not root-owned, so the publisher uses this journey's private HOME.
const wrapper = existsSync("/opt/nanocodex/installation.json") ? ["unshare", "-Ur"] : [];
const missing = [!existsSync(SSHD) && "OpenSSH sshd (" + SSHD + ")", !existsSync(hand) && "nanocodex-hand (" + hand + ")"].filter(Boolean);

const redact = text => String(text)
  .replace(/ncxh[dg]1\.[A-Za-z0-9._:-]+/g, match => match.slice(0, 7) + "[redacted]")
  .replace(/ncx_live_[A-Za-z0-9_-]+/g, "ncx_live_[redacted]")
  .replace(/-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g, "[private-key]");
const redactJSON = value => JSON.parse(redact(JSON.stringify(value ?? null)));
const alive = pid => { try { process.kill(pid, 0); return true; } catch { return false; } };

test("server_hand bootstrap enrolls the server's device; SSH recovery trusts its attested host keys until revoked",
  { timeout: 300_000, skip: missing.length && !process.env.CI ? "unavailable: " + missing.join(", ") : false }, async () => {
  assert.deepEqual(missing, [], "CI requires " + missing.join(", "));
  const evidence = process.env.NANOCODEX_EVIDENCE_DIR
    ?? join(repo, "output/hand-device-keys/server-hand-journey", new Date().toISOString().replace(/[:.]/g, "-"));
  mkdirSync(evidence, { recursive: true });
  const work = mkdtempSync(join(tmpdir(), "ncx-server-hand-"));
  chmodSync(work, 0o700);
  const transcript = join(evidence, "transcript.jsonl"), assertions = [];
  const record = (step, value) => appendFileSync(transcript, JSON.stringify({ at: new Date().toISOString(), step, ...redactJSON(value) }) + "\n");
  const check = (name, condition, detail) => {
    assertions.push({ name, ok: !!condition, ...(detail === undefined ? {} : { detail: redactJSON(detail) }) });
    assert.ok(condition, name + ": " + redact(JSON.stringify(detail ?? null)));
  };
  let server, sshd;
  const sshdLog = join(evidence, "sshd.log");
  const containerRoot = join(work, "docker");
  const { serverBin, containerBin } = writeStandIns(work);
  // The server account's session environment: its state home and container runtime.
  const serverEnvironment = { PATH: serverBin + ":/usr/bin:/bin", XDG_STATE_HOME: join(work, "server-state"), NCX_DOCKER_ROOT: containerRoot,
    NCX_SERVER_SSH_DIR: join(work, "server-ssh"), NCX_HAND_EXECUTABLE: hand, NCX_CONTAINER_PATH: containerBin + ":/usr/bin:/bin", NCX_WRAPPER: wrapper.join(",") };
  try {
    // The shipped setup script and server Hand id derivation, bundled from source.
    const setup = await importBundled(join(repo, "js/managed/src/ssh-hand-setup.ts"));
    const user = userInfo().username;
    const port = await freePort();
    server = await startHandDeviceServer({ output: join(evidence, "managed"), ttlSeconds: TTL_SECONDS, workers: [await egressWorker()] });
    const { base, origin, owner, apiKey } = server;
    const egress = await server.worker("egress");
    const id = await setup.serverHandID(owner, REFERENCE), machine = "server:" + id;
    record("setup", { owner, reference: REFERENCE, host_id: id, machine, origin, openssh: String(spawnSync("ssh", ["-V"]).stderr).trim(), hand, wrapper,
      server_environment: serverEnvironment });

    // --- the server: sshd with host key A
    const hostKeys = serverEnvironment.NCX_SERVER_SSH_DIR;
    for (const directory of [hostKeys, serverEnvironment.XDG_STATE_HOME, containerRoot]) mkdirSync(directory, { mode: 0o700 });
    const keygen = () => {
      for (const name of ["ssh_host_ecdsa_key", "ssh_host_ecdsa_key.pub"]) rmSync(join(hostKeys, name), { force: true });
      execFileSync("ssh-keygen", ["-q", "-t", "ecdsa", "-b", "256", "-N", "", "-C", "synthetic-server-host", "-f", join(hostKeys, "ssh_host_ecdsa_key")]);
      return execFileSync("ssh-keygen", ["-l", "-E", "sha256", "-f", join(hostKeys, "ssh_host_ecdsa_key.pub")]).toString().split(" ")[1];
    };
    writeFileSync(join(work, "authorized_keys"), "", { mode: 0o600 });
    const startSshd = async () => {
      writeFileSync(join(work, "sshd_config"), [
        "Port " + port, "ListenAddress 127.0.0.1", "HostKey " + join(hostKeys, "ssh_host_ecdsa_key"), "PidFile " + join(work, "sshd.pid"),
        "AuthorizedKeysFile " + join(work, "authorized_keys"), "StrictModes no", "UsePAM no", "PasswordAuthentication no",
        "KbdInteractiveAuthentication no", "PubkeyAuthentication yes", "AllowUsers " + user, "LogLevel VERBOSE",
        "SetEnv " + Object.entries(serverEnvironment).map(([key, value]) => key + "=" + value).join(" "), "",
      ].join("\n"));
      const child = spawn(SSHD, ["-D", "-e", "-f", join(work, "sshd_config")], { stdio: ["ignore", "ignore", "pipe"] });
      child.stderr.on("data", data => appendFileSync(sshdLog, data));
      await waitForPort(port);
      return child;
    };
    const stopSshd = async () => { if (!sshd) return; const done = new Promise(resolve => sshd.once("exit", resolve)); sshd.kill("SIGTERM"); await done; sshd = undefined; };
    const fpA = keygen();
    sshd = await startSshd();
    record("sshd.start", { host_key: fpA, port });

    // --- public managed account routes (synthetic API key principal) and egress private routes
    const http = async (label, method, path, { auth = apiKey, body } = {}) => {
      const response = await fetch(base + path, { method, headers: { ...(auth ? { authorization: "Bearer " + auth } : {}),
        ...(body === undefined ? {} : { "content-type": "application/json" }) }, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
        signal: AbortSignal.timeout(15_000) });
      const text = await response.text(); let value; try { value = text ? JSON.parse(text) : null; } catch { value = text; }
      record("http", { label, method, path, auth: auth ? auth.slice(0, 7) + "[redacted]" : null, request: body ?? null, status: response.status, response: value });
      return { status: response.status, body: value };
    };
    const internal = async (url, method, body, headers = {}) => {
      const response = await egress.fetch(url, { method, headers: { "content-type": "application/json", ...headers }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      const text = await response.text(); let value; try { value = text ? JSON.parse(text) : null; } catch { value = text; }
      return { status: response.status, body: value };
    };
    const subject = "S".repeat(43);
    check("egress subject bound to the synthetic owner", (await internal("https://broker.internal/subjects/" + subject, "PUT", { user_id: owner })).status === 200);
    const vaultPath = "https://broker.internal/users/" + owner + "/credentials/ssh/" + REFERENCE;
    const listTarget = async () => {
      const listed = await internal("https://broker.internal/users/" + owner + "/credentials", "GET");
      const entry = listed.body.ssh.find(identity => identity.reference === REFERENCE);
      // The Vault generated the client key; only its public half is installed on the server.
      writeFileSync(join(work, "authorized_keys"), entry.public_key + "\n", { mode: 0o600 });
      return entry;
    };
    const saveTarget = async (label, target) => {
      await internal(vaultPath, "DELETE");
      const saved = await internal(vaultPath, "PUT", { generate: true, hostname: TARGET, port, username: user, ...target });
      record("vault.save", { label, target: { hostname: TARGET, port, username: user, ...target }, status: saved.status });
      check(label + " saved", saved.status === 204, saved);
      return listTarget();
    };
    // Exactly the egress request shape server_hand uses: device trust opted in, no fingerprint ever sent.
    const ssh = async (scenario, command, { stdin } = {}) => {
      const before = server.logs.length;
      const result = await internal("https://ssh.internal/v1/execute", "POST", { identity_ref: REFERENCE, hostname: TARGET, port, username: user,
        command, host_key_trust: "device", ...(stdin === undefined ? {} : { stdin }) }, { "x-nanocodex-subject": subject });
      for (let attempt = 0; attempt < 40 && !server.logs.slice(before).some(line => line.includes("host_key_trust_mode")); attempt++) await delay(25);
      const audit = auditRecord(server.logs.slice(before));
      record("ssh", { scenario, request: { identity_ref: REFERENCE, hostname: TARGET, port, username: user,
        command: command.length > 3 ? [...command.slice(0, 2), "[SERVER_HAND_INSTALL]", ...command.slice(3)] : command,
        host_key_trust: "device", ...(stdin === undefined ? {} : { stdin: "[grant redacted]" }) }, status: result.status,
        response: result.body && { ...result.body, ...(typeof result.body.stdout === "string" ? { stdout: result.body.stdout.slice(0, 400) } : {}),
          ...(typeof result.body.stderr === "string" ? { stderr: result.body.stderr.slice(0, 400) } : {}) }, audit });
      return { ...result, audit };
    };
    const reached = ["printf", "reached-%s", "server"];
    const rejected = (result, scenario) => check(scenario + ": host-key authentication fails closed", result.status === 200
      && result.body.exit_code === 255 && result.body.stdout === "" && /server host-key authentication failed/.test(result.body.stderr), result);

    // 1. No TOFU: a device-trust target without a Vault pin cannot reach a server that has no device yet.
    await saveTarget("unpinned device-trust target", { host_key_trust: "device" });
    const unpinned = await ssh("bootstrap without a Vault pin", reached);
    check("first bootstrap without a Vault-pinned host key is refused (no TOFU)", unpinned.status === 403 && unpinned.body.error === "ssh_host_key_unattested", unpinned);

    // 2. The owner pins host key A in the Vault with device trust: the only manual host-key step.
    const target = await saveTarget("pinned device-trust target", { host_key_sha256: fpA, host_key_trust: "device" });
    check("Vault target carries pin A and device trust", target.host_key_sha256 === fpA && target.host_key_trust === "device", target);
    const preflight = await ssh("server_hand docker preflight", ["sh", "-c", "test \"$(uname -s)\" = Linux && command -v docker >/dev/null && docker info >/dev/null 2>&1"]);
    check("server_hand preflight over the Vault pin", preflight.status === 200 && preflight.body.exit_code === 0, preflight);

    // 3. server_hand connect: the owner DO records the host and mints the one-time grant.
    const before = await http("devices before bootstrap", "GET", "/v1/account/hand-devices");
    check("no device for the server before bootstrap", before.status === 200 && !before.body.data.some(device => device.machine_id === machine), before.body);
    const label = user + "@" + TARGET;
    const minted = await (await fetch(base + "/__fixture/grant", { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ owner, host: id, name: label }) })).json();
    record("grant.mint", { rpc: "AccountHostedTools.mintServerHandDeviceGrant", owner, host_id: id, name: label, response: minted });
    check("one-time ncxhg1 grant bound to owner and host, at most 10 minutes", typeof minted.grant === "string"
      && minted.grant.startsWith("ncxhg1." + owner + "." + id + ".") && minted.expires_at > Date.now() && minted.expires_at <= Date.now() + 600_000, { expires_at: minted.expires_at });
    const grant = minted.grant, secret = grant.split(".")[3];

    // 4. The shipped install script runs over Vault-pinned SSH; the grant travels only on stdin.
    const endpoint = origin + "/v1/hand-hosts/" + owner + "/" + id + "/hands";
    const install = await ssh("SERVER_HAND_INSTALL", ["sh", "-c", setup.SERVER_HAND_INSTALL, "nanocodex-hand", id, endpoint, label, IMAGE], { stdin: grant + "\n" });
    check("install script exits 0", install.status === 200 && install.body.exit_code === 0, install);
    check("install reached the server through the Vault pin", install.audit?.host_key_source === "vault_pin" && install.audit?.host_key_sha256 === fpA, install.audit);
    const container = "nanocodex-hand-" + id;
    const state = join(serverEnvironment.XDG_STATE_HOME, "nanocodex/hands", id);
    const dockerCalls = readFileSync(join(containerRoot, "docker-calls.jsonl"), "utf8");
    check("grant never appears in docker argv, env or mounts", !dockerCalls.includes(secret));
    const run = dockerCalls.trim().split("\n").map(line => JSON.parse(line)).find(call => call.argv[0] === "run");
    check("container runs the image's server publisher with the private grant file", run && run.argv.includes("--device-grant-file")
      && run.argv.includes("/state/device-grant") && run.argv.includes("server:" + id) && run.argv.includes("NANOCODEX_HAND_SSH_HOST_KEY_DIR=/ssh-host-keys"), run?.argv);

    // 5. The publisher enrolls with its own device key, deletes the grant and attests host key A.
    const devices = async step => (await http(step, "GET", "/v1/account/hand-devices")).body;
    const serverDevice = list => list.data.find(device => device.machine_id === machine && device.status === "active");
    let enrolled;
    for (const deadline = Date.now() + 60_000; Date.now() < deadline; await delay(500)) {
      enrolled = serverDevice(await devices("poll enrollment"));
      if (enrolled?.ssh_host_keys?.some(key => key.fingerprint === fpA) && !existsSync(join(state, "device-grant"))) break;
    }
    check("grant file removed after enrollment", !existsSync(join(state, "device-grant")), readdirSync(state));
    check("server device enrolled and active through the bootstrap grant", enrolled?.enrolled_by?.kind === "server_grant" && enrolled.key_version === 1, enrolled);
    check("host key A attested automatically by the device", enrolled?.ssh_host_keys?.length === 1 && enrolled.ssh_host_keys[0].fingerprint === fpA, enrolled?.ssh_host_keys);
    const deviceState = JSON.parse(readFileSync(join(state, "hand-device/device.json"), "utf8"));
    record("device.json", { device_state: deviceState });
    check("device.json binds this server machine and device", deviceState.machine_id === machine && deviceState.device_id === enrolled.id, deviceState);
    check("device key is private: 0600 file in a 0700 directory", (statSync(join(state, "hand-device/device-key.v1")).mode & 0o777) === 0o600
      && (statSync(join(state, "hand-device")).mode & 0o777) === 0o700);
    const pid = JSON.parse(readFileSync(join(containerRoot, container + ".json"), "utf8")).pid;
    const processView = readFileSync("/proc/" + pid + "/cmdline", "utf8") + readFileSync("/proc/" + pid + "/environ", "utf8");
    check("publisher argv/env carry no grant or device credential", !processView.includes(secret) && !/ncxh[dg]1\./.test(processView));

    // 6. Replay of the consumed grant, validly signed by a fresh key, is refused.
    const { publicKey, privateKey } = generateKeyPairSync("ed25519");
    const raw = publicKey.export({ format: "jwk" }).x;
    const digest = createHash("sha256").update(secret).digest("base64url");
    const signature = edSign(null, Buffer.from([DOMAIN, "enroll", origin, owner, digest, machine, raw].join("\n")), privateKey).toString("base64url");
    const replay = await http("replay the consumed grant", "POST", "/v1/hand-hosts/" + owner + "/" + id + "/hands/device",
      { auth: grant, body: { algorithm: "ed25519", public_key: raw, signature, name: "replayed" } });
    check("replayed grant returns 401", replay.status === 401, replay.body);
    check("replay enrolled no second device", (await devices("after replay")).data.filter(device => device.machine_id === machine).length === 1);

    // 7. Recovery while sshd still presents A: the Vault pin stays primary.
    const pinned = await ssh("recovery, sshd key A", reached);
    check("SSH recovery with key A", pinned.status === 200 && pinned.body.exit_code === 0 && pinned.body.stdout === "reached-server", pinned);
    check("key A matched the Vault pin", pinned.audit?.host_key_source === "vault_pin", pinned.audit);

    // 8. The server's sshd host key rotates to B. Not yet attested: fails closed.
    await stopSshd();
    const fpB = keygen();
    check("rotated host key differs", fpB !== fpA);
    sshd = await startSshd();
    record("sshd.rotate", { host_key: fpB });
    rejected(await ssh("recovery, sshd key B, not yet attested", reached), "unattested rotated key B");

    // 9. The server Hand restarts (reboot / container restart) and re-attests automatically.
    const restart = spawnSync(join(serverBin, "docker"), ["restart", container], { env: { ...process.env, ...serverEnvironment }, encoding: "utf8" });
    record("container.restart", { command: "docker restart " + container, status: restart.status, stderr: restart.stderr });
    check("server Hand container restarted", restart.status === 0, restart.stderr);
    let reattested;
    for (const deadline = Date.now() + 60_000; Date.now() < deadline; await delay(500)) {
      reattested = serverDevice(await devices("poll re-attestation"));
      if (reattested?.ssh_host_keys?.some(key => key.fingerprint === fpB)) break;
    }
    check("the same device re-attested key B without re-enrollment", reattested?.id === enrolled.id && reattested.key_version === 1
      && reattested.ssh_host_keys.length === 1 && reattested.ssh_host_keys[0].fingerprint === fpB, reattested);

    // 10. Recovery succeeds through the device attestation; nobody edited the Vault target.
    const recovered = await ssh("recovery, sshd key B, attested by the server device", reached);
    check("SSH recovery after the host key rotation", recovered.status === 200 && recovered.body.exit_code === 0 && recovered.body.stdout === "reached-server", recovered);
    check("key B matched the server device's attestation", recovered.audit?.host_key_source === "device:" + enrolled.id && recovered.audit?.host_key_sha256 === fpB, recovered.audit);
    const unchanged = await listTarget();
    check("Vault target unchanged (still pins A, device trust)", JSON.stringify(unchanged) === JSON.stringify(target), unchanged);

    // 11. The owner revokes the server device: device trust fails closed and the publisher stops.
    const revoked = await http("revoke the server device", "DELETE", "/v1/account/hand-devices/" + enrolled.id);
    check("device revoked", revoked.status === 200 && revoked.body.status === "revoked", revoked.body);
    const afterRevoke = await ssh("recovery, sshd key B, device revoked", reached);
    rejected(afterRevoke, "revoked device's attestation");
    check("revoked device contributes no attested keys", afterRevoke.audit?.attested_host_keys === 0, afterRevoke.audit);
    const current = JSON.parse(readFileSync(join(containerRoot, container + ".json"), "utf8")).pid;
    let stopped = false;
    for (const deadline = Date.now() + 60_000; Date.now() < deadline && !stopped; await delay(500)) stopped = !alive(current);
    check("revoked publisher stops instead of falling back to any other credential", stopped);
    check("server device no longer listed active", serverDevice(await devices("after revoke")) === undefined);

    const logs = server.logs.join("\n") + readFileSync(join(containerRoot, container + ".log"), "utf8") + readFileSync(sshdLog, "utf8");
    check("logs carry no grant, device credential or private key", !logs.includes(secret) && !/ncxhd1\.[A-Za-z0-9]/.test(logs) && !logs.includes("PRIVATE KEY"));
  } finally {
    if (existsSync(containerRoot)) {
      for (const name of readdirSync(containerRoot)) {
        if (name.endsWith(".json")) { try { process.kill(-JSON.parse(readFileSync(join(containerRoot, name), "utf8")).pid, "SIGKILL"); } catch { /* exited */ } }
        if (name.endsWith(".log")) writeFileSync(join(evidence, "publisher.log"), redact(readFileSync(join(containerRoot, name), "utf8")));
        if (name === "docker-calls.jsonl") writeFileSync(join(evidence, name), redact(readFileSync(join(containerRoot, name), "utf8")));
      }
    }
    writeFileSync(join(evidence, "assertions.json"), JSON.stringify(assertions, null, 2));
    if (server) writeFileSync(join(evidence, "workerd.log"), redact(server.logs.join("\n")));
    if (sshd) sshd.kill("SIGTERM");
    await server?.stop();
    rmSync(work, { recursive: true, force: true });
  }
});

/** The egress Worker as deployed (credential broker DO, ssh.internal), bound to the managed attestation entrypoint. */
async function egressWorker() {
  const bundled = await build({ entryPoints: [egressRoot + "src/egress.ts"], bundle: true, write: false, format: "esm", platform: "node",
    // The SSH key libraries are CommonJS and require Node built-ins (nodejs_compat).
    banner: { js: "import { createRequire as __ncxCreateRequire } from 'node:module'; const require = __ncxCreateRequire('/');" },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": repo + "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs" }, logLevel: "silent",
    plugins: [{ name: "tcp-dns-fixture", setup(builder) {
      builder.onResolve({ filter: /^cloudflare:sockets$/ }, args => args.importer.endsWith("tcp-dns-fixture.mjs")
        ? { path: "cloudflare:sockets", external: true } : { path: egressRoot + "test/fixtures/tcp-dns-fixture.mjs" });
      builder.onResolve({ filter: /^nanocodex\/wasm$/ }, () => ({ path: "./nanocodex.wasm", external: true }));
      builder.onResolve({ filter: /^\.\/whatsapp-runtime$/ }, () => ({ path: egressRoot + "test/whatsapp/runtime.fixture.ts" }));
      // Model-provider WASM glue is unused by SSH; an empty module stands in when it is not built.
      if (!existsSync(repo + "js/nanocodex/pkg-web/nanocodex.js")) {
        builder.onResolve({ filter: /\/pkg-web\/nanocodex\.js$/ }, () => ({ path: "pkg-web-absent", namespace: "unbuilt" }));
        builder.onLoad({ filter: /.*/, namespace: "unbuilt" }, () => ({ contents: "module.exports = {};", loader: "js" }));
      }
    } }] });
  const wasm = process.env.NANOCODEX_TEST_WASM ?? repo + "js/nanocodex/pkg-web/nanocodex_bg.wasm";
  return { name: "egress",
    modules: [{ type: "ESModule", path: "egress.mjs", contents: bundled.outputFiles[0].text },
      { type: "CompiledWasm", path: "nanocodex.wasm", contents: existsSync(wasm) ? readFileSync(wasm) : Uint8Array.from([0, 97, 115, 109, 1, 0, 0, 0]) }],
    compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"],
    bindings: { ENVIRONMENT: "test", CREDENTIAL_ENCRYPTION_KEY: Buffer.from("0123456789abcdef0123456789abcdef").toString("base64url") },
    serviceBindings: { HAND_DEVICE_SSH_HOST_KEYS: { name: "managed", entrypoint: "HandDeviceSshHostKeys" } },
    durableObjects: Object.fromEntries(Object.entries({ USER_CREDENTIALS: "UserCredentialBroker", AGENT_SUBJECTS: "AgentSubjectDirectory",
      USER_CONNECTORS: "UserConnectorBroker", MCP_CONNECTIONS: "McpConnectionDirectory", WHATSAPP_ACCOUNTS: "WhatsAppAccount",
      SPOTIFY_RATE_LIMITS: "SpotifyRateLimit", GMAIL_PUSH_MAILBOXES: "GmailPushMailbox" }).map(([key, className]) => [key, { className, useSQLite: true }])) };
}

async function importBundled(entry) {
  const result = await build({ entryPoints: [entry], bundle: true, write: false, format: "esm", platform: "neutral", logLevel: "silent" });
  return import("data:text/javascript;base64," + Buffer.from(result.outputFiles[0].text).toString("base64"));
}

/** The egress audit record of one SSH call: trust mode, matched source and public fingerprint only. */
function auditRecord(lines) {
  const text = lines.join("\n"), start = text.indexOf("host_key_trust_mode");
  if (start < 0) return undefined;
  const window = text.slice(Math.max(0, start - 1500), start + 800);
  const field = name => window.match(new RegExp(name + "[\"']?\\s*[:=]\\s*[\"']?([A-Za-z0-9_:+/=-]+)"))?.[1];
  const attested = field("attested_host_keys");
  return { action: field("action"), host_key_trust_mode: field("host_key_trust_mode"), host_key_source: field("host_key_source"),
    host_key_sha256: field("host_key_sha256"), ...(attested === undefined ? {} : { attested_host_keys: Number(attested) }) };
}

/** Server-side stand-ins: docker on the SSH session PATH; labwc / wlr-randr inside the container. */
function writeStandIns(work) {
  const serverBin = join(work, "server-bin"), containerBin = join(work, "container-bin");
  mkdirSync(serverBin, { mode: 0o700 }); mkdirSync(containerBin, { mode: 0o700 });
  const launcher = script => "#!/bin/sh\nexec '" + process.execPath + "' '" + join(standIns, script) + "' \"$@\"\n";
  writeFileSync(join(serverBin, "docker"), launcher("docker.cjs"), { mode: 0o700 });
  writeFileSync(join(containerBin, "labwc"), launcher("labwc.cjs"), { mode: 0o700 });
  writeFileSync(join(containerBin, "wlr-randr"), "#!/bin/sh\nexit 0\n", { mode: 0o700 });
  return { serverBin, containerBin };
}

function freePort() {
  return new Promise((resolve, reject) => { const probe = createServer(); probe.listen(0, "127.0.0.1", () => { const { port } = probe.address(); probe.close(() => resolve(port)); }); probe.on("error", reject); });
}

async function waitForPort(port) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await new Promise(resolve => { const socket = tcpConnect(port, "127.0.0.1", () => { socket.destroy(); resolve(true); }); socket.on("error", () => resolve(false)); })) return;
    await delay(50);
  }
  throw new Error("sshd did not listen");
}
