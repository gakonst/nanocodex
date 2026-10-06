import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { constants } from "node:fs";
import { access, lstat, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { userInfo } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { test } from "node:test";
import { WebSocketServer } from "ws";

// Run only on a disposable, logged-in macOS user with no installed Hand:
// NANOCODEX_HAND_INSTALL_E2E=1 NANOCODEX_TEST_CLI=/absolute/bin/nanocodex \
// NANOCODEX_TEST_HAND=/absolute/bin/nanocodex2 node --test \
// js/desktop-runtime/test/hand-install-login.integration.test.mjs
// Both binaries must be real, adjacent shipped artifacts. Nothing is built here.
// A private HOME does NOT isolate launchd's gui/<uid>/com.nanocodex.hand label.
const label = "com.nanocodex.hand";
const repository = fileURLToPath(new URL("../../../", import.meta.url));
const prereq = process.env.NANOCODEX_HAND_INSTALL_E2E !== "1"
  ? "Requires explicit NANOCODEX_HAND_INSTALL_E2E=1 (installs a fixture LaunchAgent)"
  : process.platform !== "darwin" ? "Requires macOS with a logged-in GUI user"
  : !process.env.NANOCODEX_TEST_CLI || !process.env.NANOCODEX_TEST_HAND
    ? "Requires NANOCODEX_TEST_CLI and NANOCODEX_TEST_HAND shipped binary paths" : false;

async function exists(path) {
  try { await lstat(path); return true; }
  catch (error) { if (error.code === "ENOENT") return false; throw error; }
}

// stdin is never recorded: even synthetic account keys stay out of evidence.
async function run(binary, args, env, input = "", timeout = 75_000, signal) {
  signal?.throwIfAborted();
  return await new Promise((resolveResult, reject) => {
    const grouped = Boolean(signal) && process.platform === "darwin";
    const child = spawn(binary, args, { env, detached: grouped, stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "", stderr = "", timedOut = false;
    const stop = () => {
      // Stop the CLI and its automatic connect helper together before teardown.
      try { if (grouped && child.pid) process.kill(-child.pid, "SIGKILL"); else child.kill("SIGKILL"); }
      catch (error) { if (error.code !== "ESRCH") reject(error); }
    };
    const timer = setTimeout(() => { timedOut = true; stop(); }, timeout);
    signal?.addEventListener("abort", stop, { once: true });
    child.stdout.on("data", data => { stdout = (stdout + data).slice(-65_536); });
    child.stderr.on("data", data => { stderr = (stderr + data).slice(-65_536); });
    child.stdin.on("error", error => { if (error.code !== "EPIPE") reject(error); });
    child.on("error", error => { clearTimeout(timer); signal?.removeEventListener("abort", stop); reject(error); });
    child.on("close", (code, exitSignal) => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", stop);
      resolveResult({ code, signal: exitSignal, timedOut, stdout, stderr });
    });
    child.stdin.end(input);
  });
}

async function until(predicate, description, timeout = 15_000) {
  const deadline = Date.now() + timeout;
  do {
    const value = await predicate();
    if (value) return value;
    await delay(100);
  } while (Date.now() < deadline);
  throw new Error(`Timed out: ${description}`);
}

test("installed dormant macOS Hand connects automatically to the exact saved CLI login", {
  skip: prereq, timeout: 300_000,
}, async t => {
  const identity = userInfo(); // OS account home, never the caller's overridden HOME.
  if (identity.uid === 0) { t.skip("Refusing root; requires a disposable GUI user"); return; }
  const cli = await realpath(resolve(process.env.NANOCODEX_TEST_CLI));
  const hand = await realpath(resolve(process.env.NANOCODEX_TEST_HAND));
  assert.equal(basename(cli), "nanocodex", "CLI must have its shipped name");
  assert.equal(basename(hand), "nanocodex2", "Hand must have its shipped name");
  assert.equal(dirname(cli), dirname(hand), "Login resolves the adjacent shipped companion");
  await Promise.all([access(cli, constants.X_OK), access(hand, constants.X_OK)]);
  const gui = `gui/${identity.uid}`;
  const target = `${gui}/${label}`;
  const launchEnv = { PATH: "/usr/bin:/bin:/usr/sbin:/sbin", HOME: identity.homedir };
  const launch = args => run("/bin/launchctl", args, launchEnv, "", 15_000);
  const guiProbe = await launch(["print", gui]);
  if (guiProbe.code !== 0) { t.skip("No accessible launchd GUI domain; no service was touched"); return; }
  const ownerFiles = [
    join(identity.homedir, "Library/LaunchAgents", `${label}.plist`),
    ...(process.env.HOME ? [join(process.env.HOME, "Library/LaunchAgents", `${label}.plist`)] : []),
    `/Library/LaunchAgents/${label}.plist`, `/Library/LaunchDaemons/${label}.plist`,
    `/System/Library/LaunchAgents/${label}.plist`, `/System/Library/LaunchDaemons/${label}.plist`,
  ];
  async function assertNoOwner() {
    for (const path of ownerFiles) {
      assert.equal(await exists(path), false, `Refusing installed Hand owner: ${path}`);
    }
    for (const domain of [gui, `user/${identity.uid}`, "system"]) {
      const probe = await launch(["print", `${domain}/${label}`]);
      assert.equal(probe.code, 113, `Refusing loaded or uninspectable Hand owner in ${domain}; launchctl exit ${probe.code}`);
    }
  }
  await assertNoOwner(); // An explicit run refuses live owners, rather than reporting success.
  const lock = `/tmp/ncx-hand-install-e2e-${identity.uid}.lock`;
  await mkdir(lock, { mode: 0o700 }); // Refuse concurrent fixture runs for this GUI label.
  let home, fixturePlist, updaterDisabled, server, sockets, env;
  let cleanupComplete = false;
  const started = new Date().toISOString();
  const evidence = { started, commands: [], transport: [], checks: [], result: "running" };
  const output = join(repository, "output/hand-install-login", started.replaceAll(":", "-") + `-${process.pid}`);
  const redact = text => String(text).replace(/ncx_live_[A-Za-z0-9_-]+/g, "[redacted-key]");
  const record = (check, observed) => evidence.checks.push({ check, observed });
  const cmd = async (binary, args, { input = "", extraEnv = {} } = {}) => {
    const result = await run(binary, args, { ...env, ...extraEnv }, input, 75_000, t.signal);
    result.stdout = redact(result.stdout); result.stderr = redact(result.stderr);
    evidence.commands.push({ executable: binary, args, ...result });
    assert.equal(result.timedOut, false, `${basename(binary)} ${args.join(" ")} exceeded timeout`);
    return result;
  };
  const ok = async (binary, args, options) => {
    const result = await cmd(binary, args, options);
    assert.equal(result.code, 0, JSON.stringify(result));
    return result;
  };
  const status = async () => JSON.parse((await ok(cli, ["hand", "status"])).stdout);
  const plist = async () => {
    const result = await run("/usr/bin/plutil", ["-convert", "json", "-o", "-", fixturePlist], launchEnv);
    assert.equal(result.code, 0, "Fixture plist must be inspectable before cleanup or assertions");
    return JSON.parse(result.stdout);
  };
  const loadedPath = text => text.split("\n").map(line => line.trim()).find(line => line.startsWith("path = "))?.slice(7);
  t.after(async () => {
    try {
      if (updaterDisabled) assert.equal(await readFile(updaterDisabled, "utf8"), "", "Fixture updater opt-out must remain in place through cleanup");
      if (fixturePlist && await exists(fixturePlist)) {
        const configuration = await plist();
        assert.equal(configuration.Label, label);
        assert.equal(configuration.EnvironmentVariables.HOME, home);
        assert.deepEqual(configuration.ProgramArguments, [hand, "hand"]);
        const probe = await launch(["print", target]);
        if (probe.code === 0) {
          assert.equal(await realpath(loadedPath(probe.stdout) ?? "/missing-fixture-path"), await realpath(fixturePlist),
            "Cleanup refuses a loaded job not backed by this exact fixture plist");
          // Boot out by the checked fixture path, never an unverified shared label.
          const stopped = await launch(["bootout", gui, fixturePlist]);
          assert.equal(stopped.code, 0, "Fixture launchctl bootout failed");
          await until(async () => (await launch(["print", target])).code === 113, "fixture job unloaded", 105_000);
        } else assert.equal(probe.code, 113, "Cleanup cannot establish that fixture is unloaded");
      }
      if (fixturePlist && !await exists(fixturePlist)) {
        assert.equal((await launch(["print", target])).code, 113,
          "Missing fixture plist with a loaded job: preserve HOME for manual recovery");
      }
      if (home) {
        const logPath = join(home, ".nanocodex/service/daemon.log");
        if (await exists(logPath)) evidence.daemonLog = redact((await readFile(logPath, "utf8")).slice(-65_536));
      }
      cleanupComplete = true;
      record("cleanup", "only the verified fixture job unloaded; fixture HOME removed");
    } catch (error) {
      evidence.result = "failed";
      evidence.cleanupError = redact(error.message);
      throw error;
    } finally {
      for (const socket of sockets?.clients ?? []) socket.terminate();
      sockets?.close();
      if (server?.listening) {
        server.closeAllConnections();
        await new Promise(resolveClosed => server.close(resolveClosed));
      }
      if (cleanupComplete && home) await rm(home, { recursive: true, force: true });
      await rm(lock, { recursive: true });
      evidence.finished = new Date().toISOString();
      evidence.cleanupComplete = cleanupComplete;
      await mkdir(output, { recursive: true });
      await writeFile(join(output, "transcript.json"), redact(JSON.stringify(evidence, null, 2)) + "\n", { mode: 0o600 });
      t.diagnostic(`Evidence: ${join(output, "transcript.json")}`);
    }
  });

  try {
    await assertNoOwner();
    // /var/folders temp paths can exceed the Hand's Unix-domain socket limit.
    home = await realpath(await mkdtemp("/tmp/nhi-"));
    // The shipped CLI checks updater configuration before dispatching commands.
    // Use its supported persistent opt-out before invoking either executable:
    // a private HOME alone does not isolate the updater's launchd label.
    await mkdir(join(home, ".nanocodex"), { mode: 0o700 });
    const disabledPath = join(home, ".nanocodex/automatic-updates-disabled");
    await writeFile(disabledPath, "", { mode: 0o600 });
    updaterDisabled = disabledPath;
    fixturePlist = join(home, "Library/LaunchAgents", `${label}.plist`);
    const accountFile = join(home, "login.json");
    const otherFile = join(home, "other.json");
    const key = character => `ncx_live_${character.repeat(12)}_${character.repeat(43)}`;
    const generations = new Map([[`Bearer ${key("a")}`, "initial"], [`Bearer ${key("b")}`, "rotated"], [`Bearer ${key("c")}`, "other"]]);
    let identityRequests = 0, current;
    const connections = [], results = new Map(), fixtureErrors = [];
    server = createServer((request, response) => {
      const generation = generations.get(request.headers.authorization);
      response.setHeader("content-type", "application/json");
      if (request.url !== "/v1/me") {
        fixtureErrors.push(`Unexpected HTTP path: ${request.url}`);
        response.writeHead(404); response.end("{}"); return;
      }
      identityRequests++;
      evidence.transport.push({ type: "identity", generation: generation ?? "rejected" });
      if (!generation) { response.writeHead(401); response.end('{"error":"unauthorized"}'); return; }
      response.end(JSON.stringify({ authentication: "api_key", user: { id: generation === "other" ? "synthetic-other" : "synthetic-owner" },
        organization: { id: "synthetic-org" }, team: { id: "synthetic-team" }, role: "owner" }));
    });
    sockets = new WebSocketServer({ noServer: true });
    server.on("upgrade", (request, socket, head) => {
      const generation = generations.get(request.headers.authorization);
      if (request.url !== "/v1/account/tool-host" || !generation) {
        fixtureErrors.push("Unexpected or unauthorized WebSocket connection");
        socket.end("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n"); return;
      }
      sockets.handleUpgrade(request, socket, head, ws => {
        const connection = { socket: ws, generation, catalogs: 0 };
        connections.push(connection);
        evidence.transport.push({ type: "websocket", generation });
        ws.on("error", error => fixtureErrors.push(redact(error.message)));
        ws.on("message", data => {
          try {
            const frame = JSON.parse(String(data));
            if (frame.type === "catalog") {
              connection.catalogs++; current = connection;
              evidence.transport.push({ type: "catalog", generation });
              ws.send('{"type":"ready"}');
            }
            if (frame.type === "result") {
              results.set(frame.call_id, frame);
              ws.send(JSON.stringify({ type: "ack", call_id: frame.call_id }));
            }
            if (frame.type === "drain") ws.send('{"type":"draining"}');
          } catch (error) { fixtureErrors.push(redact(error.message)); }
        });
      });
    });
    server.listen(0, "127.0.0.1"); await once(server, "listening");
    const origin = `http://127.0.0.1:${server.address().port}`;
    // Allowlist prevents a real account, provider, helper, or endpoint leaking in.
    env = { PATH: "/usr/bin:/bin:/usr/sbin:/sbin", HOME: home, USERPROFILE: home,
      CODEX_HOME: join(home, ".codex"), NANOCODEX_DIR: join(home, ".nanocodex"), NANOCODEX_ACCOUNT_FILE: accountFile,
      NANOCODEX_MANAGED_URL: origin, NANOCODEX_COMPUTER: "off", NO_COLOR: "1" };
    evidence.inputs = { cli, hand, home, origin, accountFile, otherFile, credentialLabels: ["initial", "rotated", "other", "rejected"] };
    const login = (binary, character, extraEnv) => ok(binary,
      binary === cli ? ["account", "login", "--with-api-key"] : ["login", "--with-api-key"],
      { input: key(character) + "\n", extraEnv });
    const shell = async name => {
      assert(current?.catalogs > 0, "A real Hand must publish its catalog before execution");
      assert.equal(current.socket.readyState, 1);
      const marker = `hand_install_${name}_ok`;
      current.socket.send(JSON.stringify({ type: "call", session_id: "synthetic-install-login", call_id: name,
        model: "gpt-6-astra", name: "exec_command", input: { cmd: `printf ${marker}` },
        output_token_budget: 1024, output_byte_budget: 131072, deadline_at: Date.now() + 10_000 }));
      const result = await until(() => results.get(name), `real exec_command result ${name}`);
      assert.equal(result.outcome?.status, "completed", redact(JSON.stringify(result)));
      assert.equal(result.outcome.output.exit_code, 0);
      assert.match(result.outcome.output.output, new RegExp(marker));
      record(`real exec_command ${name}`, result.outcome);
    };

    await ok(cli, ["hand", "install", "--prepare"]);
    let state = await status();
    assert.equal(state.installed, true); assert.equal(state.loaded, false); assert.equal(state.pid, null);
    assert.equal(await exists(accountFile), false);
    assert.equal(identityRequests, 0); assert.equal(connections.length, 0);
    const dormant = await readFile(fixturePlist, "utf8");
    assert.equal((await plist()).EnvironmentVariables.NANOCODEX_COMPUTER, "off", "Service must preserve the explicit optional-provider opt-out");
    record("install before credentials", { installed: state.installed, loaded: state.loaded, pid: state.pid, identityRequests });

    // Exercise setup from an actually absent installation, not just idempotence.
    // This is our checked, unloaded fixture file; no launchd mutation is needed.
    assert.equal((await launch(["print", target])).code, 113);
    await rm(fixturePlist);
    await ok(cli, ["setup", "--skip-account", "--skip-computer"]);
    assert.equal(await readFile(fixturePlist, "utf8"), dormant, "Setup prepares the same dormant service without credentials");
    assert.equal((await status()).loaded, false);
    assert.equal(identityRequests, 0); assert.equal(connections.length, 0);
    record("setup without login", "installed and dormant without account requests");

    const denied = await cmd(cli, ["account", "login", "--with-api-key"], { input: key("d") + "\n" });
    assert.notEqual(denied.code, 0); assert.equal(denied.signal, null);
    assert.equal(await exists(accountFile), false);
    assert.equal(await readFile(fixturePlist, "utf8"), dormant);
    assert.equal((await status()).loaded, false); assert.equal(connections.length, 0);
    record("failed login", "credentials rejected; installed dormant service retained");

    await login(cli, "a");
    await until(() => current?.generation === "initial" && current.catalogs > 0, "initial authenticated catalog");
    state = await status();
    assert.equal(state.installed, true); assert.equal(state.loaded, true); assert(state.pid > 0);
    assert.equal(state.executable, hand);
    const initialPid = state.pid;
    const bound = await plist();
    assert.equal(bound.EnvironmentVariables.NANOCODEX_ACCOUNT_FILE, accountFile);
    assert.equal(bound.EnvironmentVariables.NANOCODEX_MANAGED_URL, origin);
    assert.equal(bound.EnvironmentVariables.NANOCODEX_API_KEY, undefined);
    assert.equal(bound.EnvironmentVariables.NC_API_KEY, undefined);
    await shell("initial");
    record("login auto-connected exact saved identity", { pid: initialPid, accountFile, origin });

    const initialConnections = connections.length;
    await login(hand, "a");
    assert.equal((await status()).pid, initialPid);
    assert.equal(connections.length, initialConnections, "Unchanged login must preserve the publisher connection");
    await shell("unchanged");
    record("unchanged nanocodex2 login", { pid: initialPid, connections: connections.length });

    await login(hand, "b");
    await until(() => current?.generation === "rotated" && current.catalogs > 0, "rotated-key authenticated catalog");
    const rotated = await status();
    assert(rotated.pid > 0); assert.notEqual(rotated.pid, initialPid);
    assert.equal(connections.filter(connection => connection.socket.readyState === 1).length, 1);
    await shell("rotated");
    record("credential rotation", { previousPid: initialPid, pid: rotated.pid, authenticatedGeneration: current.generation });

    const ownerPlist = await readFile(fixturePlist, "utf8");
    const ownerConnections = connections.length;
    await login(cli, "c", { NANOCODEX_ACCOUNT_FILE: otherFile });
    assert.equal(await exists(otherFile), true, "Second account login is saved even when Hand attachment is refused");
    const refused = await cmd(cli, ["hand", "connect", "--account-file", otherFile, "--managed-url", origin]);
    assert.notEqual(refused.code, 0); assert.equal(refused.signal, null);
    assert.equal(await readFile(fixturePlist, "utf8"), ownerPlist);
    assert.equal((await status()).pid, rotated.pid); assert.equal(connections.length, ownerConnections);
    assert.equal(connections.some(connection => connection.generation === "other"), false);
    await shell("preserved_owner");
    record("second account file refusal", { ownerPid: rotated.pid, configurationPreserved: true });

    await ok(cli, ["account", "status"]);
    await ok(hand, ["status"]);
    await ok(cli, ["account", "logout"], { extraEnv: { NANOCODEX_ACCOUNT_FILE: otherFile } });
    await ok(hand, ["logout"]);
    assert.equal((await status()).pid, rotated.pid);
    assert.equal(await readFile(fixturePlist, "utf8"), ownerPlist);
    assert.equal(connections.length, ownerConnections, "Status and logout must not invoke Hand reconnection");
    await shell("after_logout");
    record("status and logout have no activation hook", { pid: rotated.pid, connections: connections.length });
    assert.deepEqual(fixtureErrors, []);
    evidence.result = "passed";
  } catch (error) {
    evidence.result = "failed";
    evidence.error = redact(error.stack ?? error.message);
    throw error;
  }
});
