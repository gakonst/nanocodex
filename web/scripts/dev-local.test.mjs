import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { EventEmitter } from "node:events";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createServer as createNetServer } from "node:net";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import test from "node:test";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

import {
  LocalStackLifecycle,
  acquireLocalDevelopmentLease,
  assertLocalDevelopmentPortAvailable,
  ensureLocalDependencies,
  ensureLocalEvalSchema,
  ensureLocalOAuthRelay,
  localDevelopmentInstance,
  localDevelopmentOrigin,
  localDevelopmentPublicOrigin,
  localDevelopmentStatePath,
  localConnectorEnvironment,
  localDependencyRequirements,
  localOAuthRelayChildLaunch,
  localOAuthRelayKey,
  localStackChildIsAlive,
  localStackChildOptions,
  loadRootEnvironment,
  mainWorktreeEnvironmentPath,
  managedChildEnvironment,
  parseLocalDevOptions,
  providerFreeWebEnvironment,
  requireLocalProcessGroups,
  rejectWorkerEnvironmentFiles,
  resolveLocalAuthMode,
  stopLocalStackChildren,
  terminateLocalStackChild,
  verifyLocalGitAdvertisement,
  verifyLocalHealthResponse,
  verifyLocalConnectHealthResponse,
  verifyLocalDocumentResponse,
  verifyLocalModelPreconnect,
  verifyLocalState,
  verifyLocalMultiplayer,
  viteChildConfiguration,
  waitForManagedStack,
  websiteChildLaunch,
} from "./dev-local.mjs";
import { prepareDevWasm } from "./check-dev-wasm.mjs";
import { localOAuthRelayChallengeProof } from "../localOAuthRelayEnvelope.mjs";

const execFileAsync = promisify(execFile);
const devLocalScript = fileURLToPath(new URL("./dev-local.mjs", import.meta.url));
const devLocalModuleUrl = new URL("./dev-local.mjs", import.meta.url).href;

function fixtureProcessSource(role, { watchParent = false } = {}) {
  return `
    import { spawn } from "node:child_process";
    ${watchParent
      ? `import { watchLocalStackParent } from ${JSON.stringify(devLocalModuleUrl)};
         watchLocalStackParent();`
      : ""}
    const descendant = spawn(
      process.execPath,
      ["--input-type=commonjs", "-e", "setInterval(() => {}, 1000)"],
      { stdio: "ignore" },
    );
    const record = { role: ${JSON.stringify(role)}, leader: process.pid, descendant: descendant.pid };
    if (process.send) process.send(record);
    else process.stdout.write(JSON.stringify(record) + "\\n");
    setInterval(() => {}, 1000);
  `;
}

function lifecycleHarnessSource(roles) {
  return `
    import { LocalStackLifecycle } from ${JSON.stringify(devLocalModuleUrl)};
    const lifecycle = new LocalStackLifecycle({ graceMs: 200 });
    lifecycle.installSignalHandlers();
    try {
      ${roles.slice(0, -1).map((role) => `
        lifecycle.spawn(
          process.execPath,
          ["--input-type=module", "-e", ${JSON.stringify(fixtureProcessSource(role))}],
          { stdio: ["ignore", "inherit", "inherit"] },
          ${JSON.stringify(role)},
        );
      `).join("\n")}
      await lifecycle.run(
        process.execPath,
        ["--input-type=module", "-e", ${JSON.stringify(fixtureProcessSource(roles.at(-1)))}],
        { stdio: ["ignore", "inherit", "inherit"] },
        ${JSON.stringify(roles.at(-1))},
      );
    } catch (error) {
      if (!lifecycle.signal) throw error;
    } finally {
      try { await lifecycle.stop(); } finally { lifecycle.removeSignalHandlers(); }
    }
  `;
}

function spawnJsonLineHarness(source) {
  const child = spawn(process.execPath, ["--input-type=module", "-e", source], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  child.stdout.setEncoding("utf8");
  let buffered = "";
  const records = [];
  const waiters = [];
  child.stdout.on("data", (chunk) => {
    buffered += chunk;
    while (buffered.includes("\n")) {
      const newline = buffered.indexOf("\n");
      const line = buffered.slice(0, newline);
      buffered = buffered.slice(newline + 1);
      if (line) records.push(JSON.parse(line));
    }
    for (const waiter of waiters.splice(0)) waiter();
  });
  return { child, records, waiters };
}

async function waitForFixtureRecords(harness, count, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (harness.records.length < count) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new Error(`received only ${harness.records.length} fixture records`);
    await new Promise((resolveWait, rejectWait) => {
      const timeout = setTimeout(
        () => rejectWait(new Error(`received only ${harness.records.length} fixture records`)),
        remaining,
      );
      harness.waiters.push(() => {
        clearTimeout(timeout);
        resolveWait();
      });
    });
  }
  return harness.records;
}

function processExit(child) {
  return new Promise((resolveExit, rejectExit) => {
    child.once("error", rejectExit);
    child.once("exit", (code, signal) => resolveExit({ code, signal }));
  });
}

async function assertProcessesExit(records, timeoutMs = 3_000) {
  const pids = records.flatMap(({ leader, descendant }) => [leader, descendant]);
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const alive = pids.filter((pid) => {
      try { process.kill(pid, 0); return true; } catch (error) {
        if (error?.code === "ESRCH") return false;
        throw error;
      }
    });
    if (alive.length === 0) return;
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 20));
  }
  assert.fail(`fixture processes remained alive: ${pids.join(", ")}`);
}

function forceStopFixture(harness) {
  if (harness.child.exitCode === null && harness.child.signalCode === null) {
    harness.child.kill("SIGKILL");
  }
  for (const { leader, descendant } of harness.records) {
    for (const pid of [-leader, descendant]) {
      try { process.kill(pid, "SIGKILL"); } catch (error) {
        if (error?.code !== "ESRCH" && error?.code !== "EPERM") throw error;
      }
    }
  }
}

test("local development installs every package required to start the web stack", () => {
  const requirements = localDependencyRequirements();
  const bindings = requirements.find(({ root }) => basename(root) === "bindings");
  const react = requirements.find(({ root }) => basename(root) === "react");
  const terminal = requirements.find(({ root }) => basename(root) === "terminal");
  const web = requirements.find(({ root }) => basename(root) === "web");
  const connectDialog = requirements.find(({ root }) => basename(root) === "connect-dialog");
  const connectPlayground = requirements.find(({ root }) => basename(root) === "connect-playground");
  const connectApi = requirements.find(({ root }) => basename(root) === "connect-api");
  assert.deepEqual(bindings?.requiredFiles, ["node_modules/wata/package.json"]);
  assert.deepEqual(bindings?.exactVersionPackages, ["wata"]);
  assert.deepEqual(react?.requiredFiles, ["node_modules/nanocodex/package.json"]);
  assert.ok(terminal);
  assert.deepEqual(terminal.requiredFiles, [
    "node_modules/streamdown/package.json",
    "node_modules/typescript/bin/tsc",
  ]);
  assert.deepEqual(terminal.exactVersionPackages, ["streamdown"]);
  assert.ok(web);
  assert.deepEqual(web.requiredFiles, [
    "node_modules/accounts/package.json",
    "node_modules/wrangler/bin/wrangler.js",
  ]);
  assert.deepEqual(web.exactVersionPackages, ["accounts"]);
  assert.deepEqual(connectDialog?.requiredFiles, ["node_modules/wrangler/bin/wrangler.js"]);
  assert.deepEqual(connectPlayground?.requiredFiles, ["node_modules/wrangler/bin/wrangler.js"]);
  assert.ok(connectApi);
  assert.equal(
    connectApi.root,
    fileURLToPath(new URL("../../services/connect-api", import.meta.url)),
  );
  assert.deepEqual(connectApi.requiredFiles, [
    "node_modules/accounts/package.json",
    "node_modules/wrangler/bin/wrangler.js",
  ]);
  assert.deepEqual(connectApi.exactVersionPackages, ["accounts"]);
  assert.equal(requirements.length, 9);
});

test("local eval bootstrap applies and repairs the canonical D1 schema", async () => {
  const calls = [];
  await ensureLocalEvalSchema("/tmp/nanocodex-eval-schema-test", {
    environment: { PATH: "/usr/bin", OPENAI_API_KEY: "must-not-be-forwarded" },
    execute: async (...arguments_) => calls.push(arguments_),
  });

  assert.equal(calls.length, 2);
  const [migration, repair] = calls;
  assert.deepEqual(migration[1].slice(-2), [
    "--config",
    resolve(fileURLToPath(new URL("..", import.meta.url)), "wrangler.jsonc"),
  ]);
  assert.deepEqual(repair[1].slice(-2), [
    "--config",
    resolve(fileURLToPath(new URL("..", import.meta.url)), "wrangler.jsonc"),
  ]);
  assert.equal(repair[1][repair[1].indexOf("--command") + 1].includes(
    "CREATE TABLE IF NOT EXISTS worksets",
  ), true);
  assert.equal(repair[1][repair[1].indexOf("--command") + 1].includes(
    "CREATE TABLE IF NOT EXISTS cluster_nodes",
  ), true);
  assert.equal(repair[2].env.OPENAI_API_KEY, undefined);
  assert.equal(repair[2].env.CI, "true");
});

async function writeDependencyFixture(root, declaredVersion, installedVersion) {
  await mkdir(resolve(root, "node_modules/accounts"), { recursive: true });
  await writeFile(resolve(root, "package.json"), JSON.stringify({
    dependencies: { accounts: declaredVersion },
  }));
  await writeFile(resolve(root, "node_modules/accounts/package.json"), JSON.stringify({
    name: "accounts",
    version: installedVersion,
  }));
}

test("a stale direct dependency installs only its deduplicated package root", async (context) => {
  const fixtureRoot = await mkdtemp(join(tmpdir(), "nanocodex-local-dependencies-"));
  context.after(() => rm(fixtureRoot, { recursive: true, force: true }));
  const staleRoot = resolve(fixtureRoot, "stale");
  const matchingRoot = resolve(fixtureRoot, "matching");
  await Promise.all([
    writeDependencyFixture(staleRoot, "0.17.0", "0.16.2"),
    writeDependencyFixture(matchingRoot, "0.17.0", "0.17.0"),
  ]);
  const requirement = (root) => ({
    root,
    requiredFiles: ["node_modules/accounts/package.json"],
    exactVersionPackages: ["accounts"],
  });
  const calls = [];

  await ensureLocalDependencies(
    { PATH: "/bin" },
    async (...arguments_) => calls.push(arguments_),
    [requirement(staleRoot), requirement(matchingRoot), requirement(staleRoot)],
  );

  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], "npm");
  assert.deepEqual(calls[0][1], ["ci", "--prefix", staleRoot]);
});

test("matching direct dependency versions do not reinstall", async (context) => {
  const fixtureRoot = await mkdtemp(join(tmpdir(), "nanocodex-local-dependencies-"));
  context.after(() => rm(fixtureRoot, { recursive: true, force: true }));
  await writeDependencyFixture(fixtureRoot, "0.17.0", "0.17.0");
  const calls = [];

  await ensureLocalDependencies(
    {},
    async (...arguments_) => calls.push(arguments_),
    [{
      root: fixtureRoot,
      requiredFiles: ["node_modules/accounts/package.json"],
      exactVersionPackages: ["accounts"],
    }],
  );

  assert.deepEqual(calls, []);
});

test("completed one-shot commands surrender their process-group capabilities", async () => {
  const lifecycle = new LocalStackLifecycle({ graceMs: 100 });
  await lifecycle.run(
    process.execPath,
    ["-e", ""],
    { stdio: "ignore" },
    "one-shot fixture",
  );
  assert.equal(lifecycle.children.length, 0);
  await lifecycle.stop();
});

test("an inaccessible reused process group is never treated as an owned live child", () => {
  const inaccessible = () => {
    const error = new Error("operation not permitted");
    error.code = "EPERM";
    throw error;
  };
  const child = { exitCode: 0, pid: 42_424, signalCode: null };

  assert.equal(localStackChildIsAlive(child, inaccessible, "darwin"), false);
  assert.equal(terminateLocalStackChild(child, "SIGKILL", inaccessible, "darwin"), false);
});

test("local development fails closed where descendant shutdown cannot be proved", () => {
  assert.throws(
    () => requireLocalProcessGroups("win32"),
    /requires Unix process-group semantics/,
  );
  assert.doesNotThrow(() => requireLocalProcessGroups("darwin"));
  assert.doesNotThrow(() => requireLocalProcessGroups("linux"));
});

test("the ChatGPT relay exits and releases its port when parent IPC disconnects before readiness", async () => {
  const probe = createNetServer();
  await new Promise((resolve, reject) => {
    probe.once("error", reject);
    probe.listen(0, "127.0.0.1", resolve);
  });
  const address = probe.address();
  assert.ok(address && typeof address !== "string");
  const port = address.port;
  await new Promise((resolve, reject) => probe.close((error) => error ? reject(error) : resolve()));

  const child = spawn(process.execPath, [devLocalScript, "--chatgpt-relay-child", String(port)], {
    cwd: fileURLToPath(new URL("..", import.meta.url)),
    env: process.env,
    stdio: ["ignore", "ignore", "pipe", "ipc"],
  });
  let stderr = "";
  let ready = false;
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  child.on("message", (message) => {
    if (message?.type === "nanocodex.chatgpt-relay.ready") ready = true;
  });
  const exited = new Promise((resolve) => {
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  try {
    child.disconnect();
    const exit = await Promise.race([
      exited,
      new Promise((_, rejectExit) => {
        setTimeout(
          () => rejectExit(new Error(`pre-ready relay did not exit: ${stderr}`)),
          5_000,
        ).unref();
      }),
    ]);
    assert.deepEqual(exit, { code: 0, signal: null });
    assert.equal(ready, false);

    const rebound = createNetServer();
    try {
      await new Promise((resolve, reject) => {
        rebound.once("error", reject);
        rebound.listen(port, "127.0.0.1", resolve);
      });
    } finally {
      await new Promise((resolve, reject) => {
        rebound.close((error) => error ? reject(error) : resolve());
      });
    }
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
  }
});

test("the detached ChatGPT relay exits when its parent IPC channel disappears", async () => {
  const child = spawn(process.execPath, [devLocalScript, "--chatgpt-relay-child"], {
    cwd: fileURLToPath(new URL("..", import.meta.url)),
    env: process.env,
    stdio: ["ignore", "ignore", "pipe", "ipc"],
  });
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  try {
    await new Promise((resolveReady, rejectReady) => {
      const timeout = setTimeout(
        () => rejectReady(new Error(`relay readiness timed out: ${stderr}`)),
        5_000,
      );
      child.once("error", rejectReady);
      child.on("message", (message) => {
        if (message?.type !== "nanocodex.chatgpt-relay.ready") return;
        clearTimeout(timeout);
        resolveReady();
      });
    });
    child.disconnect();
    const exit = await Promise.race([
      new Promise((resolveExit) => child.once("exit", (code, signal) => resolveExit({ code, signal }))),
      new Promise((_, rejectExit) => {
        setTimeout(
          () => rejectExit(new Error(`relay did not exit after IPC disconnect: ${stderr}`)),
          5_000,
        ).unref();
      }),
    ]);
    assert.deepEqual(exit, { code: 0, signal: null });
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
  }
});

test("local web environment cannot inherit provider or Cloudflare deployment credentials", () => {
  const environment = providerFreeWebEnvironment({
    PATH: "/bin",
    OPENAI_API_KEY: "provider-secret",
    CODEX_OAUTH_BOOTSTRAP: "oauth-secret",
    CHATGPT_REFRESH_TOKEN: "refresh-secret",
    CLOUDFLARE_API_TOKEN: "deployment-secret",
    UNLISTED_SECRET_SENTINEL: "unknown-secret",
    GIT_MIRROR_TOKEN: "ephemeral-local-token",
  });

  assert.deepEqual(environment, {
    PATH: "/bin",
    GIT_MIRROR_TOKEN: "ephemeral-local-token",
  });
});

test("local connector app credentials use private auxiliary names", () => {
  assert.deepEqual(localConnectorEnvironment({
    GH_CLIENT_ID: "github-client",
    GH_CLIENT_SECRETS: "github-secret",
    GOOGLE_CLIENT_ID: "google-client",
    GOOGLE_CLIENT_SECRET: "google-secret",
    X_CLIENT_ID: "x-client",
    X_CLIENT_SECRET: "x-secret",
    SLACK_CLIENT_ID: "slack-client",
    SLACK_CLIENT_SECRET: "slack-secret",
    OPENAI_API_KEY: "must-not-project",
  }), {
    NANOCODEX_LOCAL_GITHUB_OAUTH_CLIENT_ID: "github-client",
    NANOCODEX_LOCAL_GITHUB_OAUTH_CLIENT_SECRET: "github-secret",
    NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_ID: "google-client",
    NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_SECRET: "google-secret",
    NANOCODEX_LOCAL_X_OAUTH_CLIENT_ID: "x-client",
    NANOCODEX_LOCAL_X_OAUTH_CLIENT_SECRET: "x-secret",
    NANOCODEX_LOCAL_SLACK_OAUTH_CLIENT_ID: "slack-client",
    NANOCODEX_LOCAL_SLACK_OAUTH_CLIENT_SECRET: "slack-secret",
  });
  assert.deepEqual(localConnectorEnvironment({
    GH_CLIENT_ID: "ambient-github-id",
    GOOGLE_CLIENT_ID: "ambient-google-id",
    X_CLIENT_ID: "ambient-x-id",
    SLACK_CLIENT_ID: "ambient-slack-id",
  }), {});
  assert.deepEqual(localConnectorEnvironment({
    NANOCODEX_GOOGLE_OAUTH_CLIENT_ID: "explicit-incomplete-id",
  }), {
    NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_ID: "explicit-incomplete-id",
  });
});

test("partial ambient connector credentials do not break passkey-only local development", () => {
  assert.deepEqual(localConnectorEnvironment({
    GH_CLIENT_ID: "github-without-secret",
    GOOGLE_CLIENT_ID: "google-without-secret",
    X_CLIENT_SECRET: "x-without-id",
  }), {});
  assert.deepEqual(localConnectorEnvironment({
    NANOCODEX_GOOGLE_OAUTH_CLIENT_ID: "explicit-google-without-secret",
  }), {
    NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_ID: "explicit-google-without-secret",
  });
});

test("the local launcher loads the main Git worktree environment from a linked worktree", async () => {
  const temporaryDirectory = await mkdtemp(join(tmpdir(), "nanocodex-main-worktree-env-"));
  const previousApiKey = process.env.OPENAI_API_KEY;
  const previousMode = process.env.NANOCODEX_AUTH_MODE;
  const previousSentinel = process.env.NANOCODEX_ROOT_ENV_SENTINEL;
  try {
    delete process.env.OPENAI_API_KEY;
    delete process.env.NANOCODEX_AUTH_MODE;
    delete process.env.NANOCODEX_ROOT_ENV_SENTINEL;
    const mainWorktree = join(temporaryDirectory, "main");
    const linkedWorktree = join(temporaryDirectory, "linked");
    const linkedGitDirectory = join(mainWorktree, ".git", "worktrees", "linked");
    await mkdir(linkedGitDirectory, { recursive: true });
    await mkdir(linkedWorktree, { recursive: true });
    await writeFile(join(linkedWorktree, ".git"), `gitdir: ${linkedGitDirectory}\n`);
    await writeFile(join(linkedGitDirectory, "commondir"), "../..\n");
    await writeFile(
      join(mainWorktree, ".env"),
      "NANOCODEX_AUTH_MODE=api_key\nOPENAI_API_KEY=main-provider-secret\nNANOCODEX_ROOT_ENV_SENTINEL=main-worktree\n",
    );
    await writeFile(
      join(linkedWorktree, ".env"),
      "OPENAI_API_KEY=linked-provider-secret\nNANOCODEX_ROOT_ENV_SENTINEL=linked-worktree\n",
    );

    assert.equal(
      await mainWorktreeEnvironmentPath(mainWorktree),
      join(mainWorktree, ".env"),
    );
    assert.equal(
      await mainWorktreeEnvironmentPath(linkedWorktree),
      join(mainWorktree, ".env"),
    );
    await loadRootEnvironment(undefined, linkedWorktree);
    assert.equal(process.env.NANOCODEX_ROOT_ENV_SENTINEL, "main-worktree");
    const options = parseLocalDevOptions([], process.env);
    assert.equal(options.requestedMode, "api_key");
    assert.equal(await resolveLocalAuthMode(options, process.env), "api_key");
    assert.equal(managedChildEnvironment(process.env).OPENAI_API_KEY, "main-provider-secret");
    assert.equal(providerFreeWebEnvironment(process.env).OPENAI_API_KEY, undefined);
    await assert.rejects(loadRootEnvironment(undefined, linkedWorktree), /already loaded/);
  } finally {
    if (previousApiKey === undefined) delete process.env.OPENAI_API_KEY;
    else process.env.OPENAI_API_KEY = previousApiKey;
    if (previousMode === undefined) delete process.env.NANOCODEX_AUTH_MODE;
    else process.env.NANOCODEX_AUTH_MODE = previousMode;
    if (previousSentinel === undefined) delete process.env.NANOCODEX_ROOT_ENV_SENTINEL;
    else process.env.NANOCODEX_ROOT_ENV_SENTINEL = previousSentinel;
    await rm(temporaryDirectory, { recursive: true, force: true });
  }
});

test("the actual Vite child launch cannot inherit or reload a provider sentinel", async () => {
  const temporaryDirectory = await mkdtemp(join(tmpdir(), "nanocodex-env-sentinel-"));
  try {
    const envPath = join(temporaryDirectory, "child.env");
    await writeFile(envPath, "RELOADED_ENV_SECRET_SENTINEL=env-file-secret\n");
    const inherited = {
      ...process.env,
      GIT_MIRROR_TOKEN: "ephemeral-local-token",
      NODE_OPTIONS: `--env-file=${envPath}`,
      NANOCODEX_CODEX_AUTH_FILE: "/private/codex/auth.json",
      OPENAI_API_KEY: "provider-secret",
      ROOT_ENV_SECRET_SENTINEL: "root-secret",
    };
    const names = [
      "GIT_MIRROR_TOKEN",
      "NANOCODEX_CODEX_AUTH_FILE",
      "NANOCODEX_LOCAL_MODEL_ACCESS",
      "NANOCODEX_LOCAL_MODEL_AUTH_MODE",
      "OPENAI_API_KEY",
      "RELOADED_ENV_SECRET_SENTINEL",
      "ROOT_ENV_SECRET_SENTINEL",
    ];
    const websiteLaunch = websiteChildLaunch(inherited, localDevelopmentOrigin(), {
      CLOUDFLARE_LOAD_DEV_VARS_FROM_DOT_ENV: "false",
      NANOCODEX_LOCAL_MODEL_ACCESS: "managed",
      NANOCODEX_LOCAL_MODEL_AUTH_MODE: "api_key",
    }, names);
    const liveWebsiteLaunch = websiteChildLaunch(
      inherited,
      localDevelopmentOrigin(),
      { CLOUDFLARE_LOAD_DEV_VARS_FROM_DOT_ENV: "false" },
    );
    assert.deepEqual(
      liveWebsiteLaunch.options.stdio,
      ["ignore", "inherit", "inherit", "ipc"],
    );
    const websiteResult = await execFileAsync(
      websiteLaunch.command,
      websiteLaunch.arguments,
      { cwd: websiteLaunch.options.cwd, env: websiteLaunch.options.env },
    );
    assert.deepEqual(JSON.parse(websiteResult.stdout), {
      GIT_MIRROR_TOKEN: "ephemeral-local-token",
      NANOCODEX_CODEX_AUTH_FILE: null,
      NANOCODEX_LOCAL_MODEL_ACCESS: "managed",
      NANOCODEX_LOCAL_MODEL_AUTH_MODE: "api_key",
      OPENAI_API_KEY: null,
      RELOADED_ENV_SECRET_SENTINEL: null,
      ROOT_ENV_SECRET_SENTINEL: null,
    });

    const managedResult = await execFileAsync(
      process.execPath,
      [devLocalScript, "--environment-sentinel", ...names],
      { env: managedChildEnvironment(inherited) },
    );
    assert.deepEqual(JSON.parse(managedResult.stdout), {
      GIT_MIRROR_TOKEN: null,
      NANOCODEX_CODEX_AUTH_FILE: "/private/codex/auth.json",
      NANOCODEX_LOCAL_MODEL_ACCESS: null,
      NANOCODEX_LOCAL_MODEL_AUTH_MODE: null,
      OPENAI_API_KEY: "provider-secret",
      RELOADED_ENV_SECRET_SENTINEL: null,
      ROOT_ENV_SECRET_SENTINEL: null,
    });
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true });
  }
});

test("the Vite child disables env loading and rejects Wrangler dev-var files", async () => {
  assert.doesNotMatch(await readFile(devLocalScript, "utf8"), /--env-file(?:-if-exists)?/);
  assert.deepEqual(viteChildConfiguration("127.0.0.1", "5173"), {
    envDir: false,
    server: {
      host: "127.0.0.1",
      port: 5173,
      strictPort: true,
      watch: { ignored: ["**/.env*", "**/.dev.vars*"] },
    },
  });
  assert.deepEqual(
    viteChildConfiguration("localhost", "5173"),
    viteChildConfiguration("127.0.0.1", "5173"),
  );

  const temporaryDirectory = await mkdtemp(join(tmpdir(), "nanocodex-dev-vars-"));
  try {
    await writeFile(join(temporaryDirectory, ".dev.vars"), "OPENAI_API_KEY=secret\n");
    await assert.rejects(
      rejectWorkerEnvironmentFiles(temporaryDirectory),
      /website Worker env files are disabled/,
    );
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true });
  }
});

test("local development rejects a port already owned on either loopback family", async () => {
  const listener = createNetServer();
  await new Promise((resolveListen, rejectListen) => {
    listener.once("error", rejectListen);
    listener.listen(0, "127.0.0.1", resolveListen);
  });
  const address = listener.address();
  assert.ok(address && typeof address === "object");
  try {
    await assert.rejects(
      assertLocalDevelopmentPortAvailable("127.0.0.1", String(address.port)),
      /local development port .* is already in use/,
    );
  } finally {
    await new Promise((resolveClose, rejectClose) => {
      listener.close((error) => error ? rejectClose(error) : resolveClose());
    });
  }
  await assert.doesNotReject(
    assertLocalDevelopmentPortAvailable("127.0.0.1", String(address.port)),
  );
});

test("local stack children are isolated and shutdown still targets an exited group leader", async () => {
  assert.equal(localStackChildOptions({ stdio: "inherit" }, "linux").detached, true);
  assert.equal(localStackChildOptions({ stdio: "inherit" }, "win32").detached, false);
  const signals = [];
  const child = { exitCode: 0, pid: 42_424, signalCode: null };
  assert.equal(terminateLocalStackChild(
    child,
    "SIGTERM",
    (pid, signal) => { signals.push([pid, signal]); },
    "linux",
  ), true);
  assert.deepEqual(signals, [[-42_424, "SIGTERM"]]);

  signals.length = 0;
  let alive = true;
  await stopLocalStackChildren([child], [Promise.resolve()], {
    graceMs: 10,
    isAlive: () => alive,
    terminate: (target, signal) => {
      signals.push([target.pid, signal]);
      if (signal === "SIGKILL") alive = false;
    },
  });
  assert.deepEqual(signals, [[42_424, "SIGTERM"], [42_424, "SIGKILL"]]);
});

test("local stack shutdown kills a descendant after its process-group leader exits", {
  skip: process.platform === "win32",
}, async () => {
  const child = spawn(
    process.execPath,
    [
      "--input-type=commonjs",
      "-e",
      `
        const { spawn } = require("node:child_process");
        const descendant = spawn(
          process.execPath,
          ["--input-type=commonjs", "-e", "setInterval(() => {}, 1000)"],
          { stdio: "ignore" },
        );
        process.send({ pid: descendant.pid }, () => process.exit(0));
      `,
    ],
    localStackChildOptions({ stdio: ["ignore", "ignore", "inherit", "ipc"] }),
  );
  const exited = new Promise((resolveExit, rejectExit) => {
    child.once("error", rejectExit);
    child.once("exit", (code, signal) => resolveExit({ code, signal }));
  });
  void exited.catch(() => {});
  let descendantPid;
  let cleaned = false;
  try {
    descendantPid = await new Promise((resolvePid, rejectPid) => {
      const timeout = setTimeout(
        () => rejectPid(new Error("process-group fixture did not report its descendant")),
        1_000,
      );
      child.once("message", (message) => {
        clearTimeout(timeout);
        resolvePid(message.pid);
      });
      child.once("error", (error) => {
        clearTimeout(timeout);
        rejectPid(error);
      });
    });
    assert.deepEqual(await exited, { code: 0, signal: null });
    assert.doesNotThrow(() => process.kill(descendantPid, 0));

    await stopLocalStackChildren([child], [exited], { graceMs: 100 });
    let descendantExited = false;
    for (let attempt = 0; attempt < 100; attempt += 1) {
      try {
        process.kill(descendantPid, 0);
      } catch (error) {
        if (error?.code !== "ESRCH") throw error;
        descendantExited = true;
        break;
      }
      await new Promise((resolveDelay) => setTimeout(resolveDelay, 10));
    }
    assert.equal(descendantExited, true);
    cleaned = true;
  } finally {
    if (!cleaned && Number.isSafeInteger(child.pid)) {
      try { process.kill(-child.pid, "SIGKILL"); } catch (error) {
        if (error?.code !== "ESRCH" && error?.code !== "EPERM") throw error;
      }
    }
    if (!cleaned && Number.isSafeInteger(descendantPid)) {
      try { process.kill(descendantPid, "SIGKILL"); } catch (error) {
        if (error?.code !== "ESRCH") throw error;
      }
    }
  }
});

test("a repeated signal terminates an in-flight detached setup process group", {
  skip: process.platform === "win32",
}, async () => {
  const harness = spawnJsonLineHarness(lifecycleHarnessSource(["setup"]));
  try {
    const exited = processExit(harness.child);
    const records = await waitForFixtureRecords(harness, 1);
    assert.equal(records[0].role, "setup");

    assert.equal(harness.child.kill("SIGTERM"), true);
    harness.child.kill("SIGTERM");
    await Promise.race([
      exited,
      new Promise((_, rejectTimeout) => setTimeout(
        () => rejectTimeout(new Error("setup orchestrator hung after repeated signal")),
        2_000,
      )),
    ]);
    await assertProcessesExit(records);
  } finally {
    forceStopFixture(harness);
  }
});

test("shutdown terminates publisher and already-running website process groups", {
  skip: process.platform === "win32",
}, async () => {
  const harness = spawnJsonLineHarness(lifecycleHarnessSource(["website", "publisher"]));
  try {
    const exited = processExit(harness.child);
    const records = await waitForFixtureRecords(harness, 2);
    assert.deepEqual(new Set(records.map(({ role }) => role)), new Set(["website", "publisher"]));

    assert.equal(harness.child.kill("SIGINT"), true);
    await Promise.race([
      exited,
      new Promise((_, rejectTimeout) => setTimeout(
        () => rejectTimeout(new Error("publisher orchestrator did not exit after shutdown")),
        2_000,
      )),
    ]);
    await assertProcessesExit(records);
  } finally {
    forceStopFixture(harness);
  }
});

test("abrupt orchestrator loss terminates the detached website child and descendants", {
  skip: process.platform === "win32",
}, async () => {
  const childSource = fixtureProcessSource("website", { watchParent: true });
  const parentSource = `
    import { spawn } from "node:child_process";
    const website = spawn(
      process.execPath,
      ["--input-type=module", "-e", ${JSON.stringify(childSource)}],
      { detached: true, stdio: ["ignore", "ignore", "inherit", "ipc"] },
    );
    website.once("message", (record) => process.stdout.write(JSON.stringify(record) + "\\n"));
    setInterval(() => {}, 1000);
  `;
  const harness = spawnJsonLineHarness(parentSource);
  try {
    const exited = processExit(harness.child);
    const records = await waitForFixtureRecords(harness, 1);
    assert.equal(records[0].role, "website");

    assert.equal(harness.child.kill("SIGKILL"), true);
    assert.deepEqual(await exited, { code: null, signal: "SIGKILL" });
    await assertProcessesExit(records);
  } finally {
    forceStopFixture(harness);
  }
});

test("managed local readiness is an exact private child-process attestation", async () => {
  const child = spawn(
    process.execPath,
    [devLocalScript, "--managed-ready-sentinel"],
    {
      env: providerFreeWebEnvironment(process.env),
      stdio: ["ignore", "ignore", "inherit", "ipc"],
    },
  );
  const exited = new Promise((resolveExit, rejectExit) => {
    child.once("error", rejectExit);
    child.once("exit", (code, signal) => resolveExit({ code, signal }));
  });
  await waitForManagedStack(child, 1_000);
  assert.deepEqual(await exited, { code: 0, signal: null });
});

test("local development options select one explicit managed auth mode", () => {
  assert.deepEqual(parseLocalDevOptions([], {}), {
    requestedMode: undefined,
    withoutMultiplayer: false,
  });
  assert.deepEqual(parseLocalDevOptions(["--auth-mode=api_key"], {}), {
    requestedMode: "api_key",
    withoutMultiplayer: false,
  });
  assert.deepEqual(parseLocalDevOptions(
    ["--without-multiplayer"],
    { NANOCODEX_AUTH_MODE: "chatgpt" },
  ), {
    requestedMode: undefined,
    withoutMultiplayer: true,
  });
  assert.throws(
    () => parseLocalDevOptions(["--without-multiplayer", "--auth-mode=chatgpt"], {}),
    /cannot be combined/,
  );
  assert.throws(
    () => parseLocalDevOptions(["--auth-mode=chatgpt", "--auth-mode=api_key"], {}),
    /only once/,
  );
  assert.throws(() => parseLocalDevOptions(["--auth-mode=other"], {}), /must be api_key/);
});

test("localhost selects only an existing non-interactive model credential", async () => {
  const automatic = { requestedMode: undefined, withoutMultiplayer: false };
  let inspectedLogin = false;
  assert.equal(
    await resolveLocalAuthMode(
      automatic,
      { OPENAI_API_KEY: "provider-secret" },
      async () => {
        inspectedLogin = true;
        return true;
      },
    ),
    "api_key",
  );
  assert.equal(inspectedLogin, false);
  assert.equal(
    await resolveLocalAuthMode(automatic, {}, async () => true),
    "chatgpt",
  );
  await assert.rejects(
    resolveLocalAuthMode(
      { requestedMode: "chatgpt", withoutMultiplayer: false },
      {},
      async () => false,
    ),
    /requires an existing 0600 Codex login/,
  );
  await assert.rejects(
    resolveLocalAuthMode(
      { requestedMode: "api_key", withoutMultiplayer: false },
      {},
      async () => true,
    ),
    /requires OPENAI_API_KEY/,
  );

  inspectedLogin = false;
  assert.equal(
    await resolveLocalAuthMode(
      { requestedMode: undefined, withoutMultiplayer: true },
      {},
      async () => {
        inspectedLogin = true;
        return true;
      },
    ),
    undefined,
  );
  assert.equal(inspectedLogin, false);
});

test("localhost auto-discovers the host Codex auth file without reading it into the website", async () => {
  const temporaryDirectory = await mkdtemp(join(tmpdir(), "nanocodex-codex-auth-"));
  try {
    const authPath = join(temporaryDirectory, "auth.json");
    const accessPayload = Buffer.from(JSON.stringify({
      exp: Math.floor(Date.now() / 1_000) + 3_600,
    })).toString("base64url");
    await writeFile(authPath, `${JSON.stringify({
      auth_mode: "chatgpt",
      tokens: {
        access_token: `header.${accessPayload}.signature`,
        account_id: "account-1",
      },
    })}\n`, { mode: 0o600 });
    assert.equal(
      await resolveLocalAuthMode(
        { requestedMode: undefined, withoutMultiplayer: false },
        { NANOCODEX_CODEX_AUTH_FILE: authPath },
      ),
      "chatgpt",
    );
    assert.equal(
      providerFreeWebEnvironment({ NANOCODEX_CODEX_AUTH_FILE: authPath })
        .NANOCODEX_CODEX_AUTH_FILE,
      undefined,
    );
    await writeFile(authPath, "not a usable Codex login\n", { mode: 0o600 });
    await assert.rejects(
      resolveLocalAuthMode(
        { requestedMode: undefined, withoutMultiplayer: false },
        { NANOCODEX_CODEX_AUTH_FILE: authPath },
      ),
      /No existing local model credential/,
    );
    assert.equal(
      await resolveLocalAuthMode(
        { requestedMode: undefined, withoutMultiplayer: false },
        {
          NANOCODEX_CODEX_AUTH_FILE: authPath,
          OPENAI_API_KEY: "provider-secret",
        },
      ),
      "api_key",
    );
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true });
  }
});

test("missing localhost credentials fail without launching a login flow", async () => {
  const temporaryDirectory = await mkdtemp(join(tmpdir(), "nanocodex-no-auth-"));
  try {
    await assert.rejects(
      resolveLocalAuthMode(
        { requestedMode: undefined, withoutMultiplayer: false },
        {
          NANOCODEX_CODEX_AUTH_FILE: join(temporaryDirectory, "missing-auth.json"),
          PATH: temporaryDirectory,
        },
      ),
      /No existing local model credential.*never starts an OAuth or device-code flow/,
    );
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true });
  }
});

test("local development origin is the canonical loopback HTTP authority", () => {
  assert.equal(localDevelopmentOrigin().origin, "http://127.0.0.1:5173");
  assert.equal(localDevelopmentOrigin("http://127.0.0.1:6123").port, "6123");
  for (const invalid of [
    "https://localhost:5173",
    "http://0.0.0.0:5173",
    "http://127.0.0.1",
    "http://127.0.0.1:5173/path",
  ]) {
    assert.throws(() => localDevelopmentOrigin(invalid), /explicit loopback HTTP origin/);
  }
});

test("local development gives the primary checkout and worktrees stable isolated identities", () => {
  const primary = localDevelopmentInstance("/Users/example/nanocodex", { primary: true });
  const worktree = localDevelopmentInstance("/Users/example/nanocodex/.worktrees/passkey-fix");
  assert.deepEqual(primary, {
    defaultOrigin: "http://127.0.0.1:5173",
    id: "main",
    playgroundOrigin: "http://playground.nanocodex.localhost:5173",
    primary: true,
    publicOrigin: "http://nanocodex.localhost:5173",
  });
  assert.match(worktree.id, /^passkey-fix-[a-f0-9]{6}$/);
  assert.equal(worktree.primary, false);
  const worktreePort = new URL(worktree.defaultOrigin).port;
  assert.equal(
    worktree.publicOrigin,
    `http://${worktree.id}.nanocodex.localhost:${worktreePort}`,
  );
  assert.equal(
    worktree.playgroundOrigin,
    `http://playground-${worktree.id}.nanocodex.localhost:${worktreePort}`,
  );
  assert.match(worktree.defaultOrigin, /^http:\/\/127\.0\.0\.1:[2-4][0-9]{4}$/);
  assert.equal(
    localDevelopmentInstance("/tmp/other", { requestedName: "review-v2" }).id,
    "review-v2",
  );
  assert.equal(
    localDevelopmentPublicOrigin(primary.publicOrigin).origin,
    "http://nanocodex.localhost:5173",
  );
  assert.equal(
    localDevelopmentPublicOrigin(worktree.publicOrigin).origin,
    worktree.publicOrigin,
  );
  assert.equal(
    localDevelopmentStatePath("/Users/example", primary.id),
    "/Users/example/.nanocodex/web-development",
  );
  assert.equal(
    localDevelopmentStatePath("/Users/example", worktree.id),
    `/Users/example/.nanocodex/web-development/instances/${worktree.id}`,
  );
  for (const invalid of [
    "http://nanocodex.example:5173",
    "https://nanocodex.localhost:8443",
    "http://nanocodex.localhost",
    "http://nanocodex.other.localhost:5173",
  ]) {
    assert.throws(
      () => localDevelopmentPublicOrigin(invalid),
      /must be HTTP under nanocodex\.localhost with an explicit port/,
    );
  }
});

test("the fixed OAuth relay is adopted without Docker or provider credentials", async () => {
  assert.equal(localOAuthRelayKey({}), "nanocodex-local-oauth-relay-hmac-v1-only");
  assert.throws(
    () => localOAuthRelayKey({ NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY: "short" }),
    /32 through 1024/,
  );
  const launch = localOAuthRelayChildLaunch(
    { PATH: "/bin", OPENAI_API_KEY: "must-not-cross" },
    "oauth-key-with-at-least-thirty-two-characters",
  );
  assert.equal(launch.command, process.execPath);
  assert.match(launch.arguments[0], /local-oauth-relay\.mjs$/);
  assert.equal(launch.options.detached, true);
  assert.deepEqual(launch.options.env, {
    NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY: "oauth-key-with-at-least-thirty-two-characters",
    PATH: "/bin",
  });

  let spawned = false;
  const adopted = await ensureLocalOAuthRelay({}, {
    fetchRelay: async (input) => {
      const challenge = new URL(input).searchParams.get("challenge");
      return Response.json({
        service: "nanocodex-local-oauth-relay",
        status: "ok",
        version: 1,
        proof: await localOAuthRelayChallengeProof(
          challenge,
          "nanocodex-local-oauth-relay-hmac-v1-only",
        ),
      });
    },
    spawnRelay: () => { spawned = true; throw new Error("must not spawn"); },
  });
  assert.deepEqual(adopted, { adopted: true, origin: "http://127.0.0.1:47891" });
  assert.equal(spawned, false);
});

test("each instance state admits one owner without blocking another instance", async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-development-lease-"));
  try {
    const firstPath = join(directory, "first");
    const secondPath = join(directory, "second");
    const first = await acquireLocalDevelopmentLease(firstPath, {
      currentPid: 101,
      isProcessAlive: () => false,
    });
    const independent = await acquireLocalDevelopmentLease(secondPath, {
      currentPid: 202,
      isProcessAlive: () => false,
    });
    assert.match(first.processTitle, /^ncdx:[A-Za-z0-9_-]{16}$/);
    assert.match(independent.processTitle, /^ncdx:[A-Za-z0-9_-]{16}$/);
    assert.notEqual(first.processTitle, independent.processTitle);
    assert.equal((await stat(firstPath)).mode & 0o777, 0o700);
    await assert.rejects(
      acquireLocalDevelopmentLease(firstPath, {
        currentPid: 303,
        isProcessAlive: (pid) => pid === 101,
      }),
      /already running as process 101/,
    );
    await independent.release();
    await first.release();
    const second = await acquireLocalDevelopmentLease(firstPath, {
      currentPid: 303,
      isProcessAlive: () => false,
    });
    await second.release();
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("managed localhost requires exact non-interactive health and WebSocket attestations", async () => {
  assert.equal(await verifyLocalHealthResponse(Response.json({
    agent_configured: true,
    auth_mode: "chatgpt",
    credential_source: "managed",
    interactive_auth: false,
    status: "ok",
  }), "chatgpt"), true);
  await assert.rejects(
    verifyLocalHealthResponse(Response.json({
      agent_configured: true,
      auth_mode: "chatgpt",
      credential_source: "subscription",
      interactive_auth: true,
      status: "ok",
    }), "chatgpt"),
    /did not attest non-interactive managed chatgpt access/,
  );

  class AttestedWebSocket extends EventEmitter {
    static attestation = '{"type":"nanocodex.proxy.ready"}';
    static instances = [];
    readyState = 1;
    constructor(url, options) {
      super();
      this.url = url;
      this.options = options;
      this.closed = false;
      AttestedWebSocket.instances.push(this);
      queueMicrotask(() => this.emit(
        "message",
        Buffer.from(this.constructor.attestation),
        false,
      ));
    }
    close() { this.closed = true; }
  }
  const origin = localDevelopmentOrigin("http://127.0.0.1:55173");
  await verifyLocalModelPreconnect(origin, AttestedWebSocket, 1_000);
  const socket = AttestedWebSocket.instances[0];
  assert.equal(socket.url.origin, "ws://127.0.0.1:55173");
  assert.equal(socket.url.pathname, "/api/responses");
  assert.match(socket.url.searchParams.get("session_id"), /^[A-Za-z0-9_-]{43}$/);
  assert.deepEqual(socket.options, {
    handshakeTimeout: 1_000,
    origin: origin.origin,
  });
  assert.equal(socket.closed, true);

  class InvalidWebSocket extends AttestedWebSocket {
    static attestation = '{"type":"nanocodex.proxy.ready","credential":"forbidden"}';
  }
  await assert.rejects(
    verifyLocalModelPreconnect(origin, InvalidWebSocket, 1_000),
    /invalid attestation/,
  );
});

test("local readiness verifies every advertised application document", async () => {
  assert.equal(await verifyLocalDocumentResponse(new Response(
    '<!doctype html><meta name="nanocodex-theme" content="ready">',
    { headers: { "content-type": "text/html; charset=utf-8" } },
  ), "Account", ["nanocodex-theme"]), true);

  await assert.rejects(
    verifyLocalDocumentResponse(Response.json({ error: "not_found" }, { status: 404 }),
      "Account", ["nanocodex-theme"]),
    /Account document returned HTTP 404/,
  );
  await assert.rejects(
    verifyLocalDocumentResponse(new Response("plain", {
      headers: { "content-type": "text/plain" },
    }), "Connect dialog", ["connect-dialog/src/main.tsx"]),
    /Connect dialog document did not return HTML/,
  );
  await assert.rejects(
    verifyLocalDocumentResponse(new Response("<!doctype html>", {
      headers: { "content-type": "text/html" },
    }), "Connect playground", ["Private playground"]),
    /Connect playground document omitted its application marker/,
  );
});

test("local readiness proves pinned Source, commit, patch, eval, and Git state", async () => {
  const origin = localDevelopmentOrigin("http://127.0.0.1:55173");
  const head = "a".repeat(40);
  const blob = "b".repeat(40);
  const calls = [];
  const gitCalls = [];
  const repository = {
    branch: "master",
    commitPageSize: 32,
    head,
    indexedCommits: 1,
  };
  const generatedAt = "2026-08-23T00:00:00.000Z";
  const responses = new Map([
    [`/api/repository/snapshot?generation=${head}`, Response.json({
      generatedAt,
      repository,
      tree: [{
        path: "README.md",
        objectId: blob,
        contentUrl: `/api/repository/blob/${blob}`,
      }],
    }, { headers: { "x-repository-generation": head } })],
    [`/api/repository/commit-index?generation=${head}`, Response.json({
      generatedAt,
      hashes: [head],
      repository,
      scopeCounts: { all: 1, docs: 0, eval: 0, fix: 0, perf: 0 },
      version: 1,
    }, { headers: { "x-repository-generation": head } })],
    ["/api/evals", Response.json({ schemaVersion: 5, worksets: [] })],
    [`/api/repository/blob/${blob}`, new Response("# Nanocodex\n")],
    [`/api/repository/commits?generation=${head}&page=0`, Response.json(
      [{
        author: "Nanocodex",
        authoredAt: generatedAt,
        body: "",
        files: [],
        hash: head,
        parents: [],
        refs: ["HEAD -> master"],
        shortHash: head.slice(0, 7),
        stats: { additions: 0, deletions: 0, files: 0 },
        subject: "test commit",
      }],
      { headers: { "x-repository-generation": head } },
    )],
    [`/api/repository/commits/${head}/0000.diff`, new Response(
      `From ${head} Mon Sep 17 00:00:00 2001\n`,
      { headers: { "x-repository-generation": head } },
    )],
  ]);

  await verifyLocalState(origin, head, {
    environment: { PATH: "/bin" },
    request: async (url) => {
      const key = `${url.pathname}${url.search}`;
      calls.push(key);
      const response = responses.get(key);
      assert.ok(response, `unexpected readiness request ${key}`);
      return response;
    },
    verifyGit: async (...arguments_) => { gitCalls.push(arguments_); },
  });

  assert.deepEqual(calls, [
    `/api/repository/snapshot?generation=${head}`,
    `/api/repository/commit-index?generation=${head}`,
    "/api/evals",
    `/api/repository/blob/${blob}`,
    `/api/repository/commits?generation=${head}&page=0`,
    `/api/repository/commits/${head}/0000.diff`,
  ]);
  assert.deepEqual(gitCalls, [[
    origin,
    head,
    { PATH: "/bin" },
  ]]);
});

test("local Git readiness uses only the provider-free HTTP read advertisement", async () => {
  const origin = localDevelopmentOrigin("http://127.0.0.1:55173");
  const head = "a".repeat(40);
  const executions = [];
  const advertisement = [
    "ref: refs/heads/master\tHEAD",
    `${head}\tHEAD`,
    `${head}\trefs/heads/master`,
    "",
  ].join("\n");
  await verifyLocalGitAdvertisement(
    origin,
    head,
    { PATH: "/bin" },
    async (...arguments_) => {
      executions.push(arguments_);
      return advertisement;
    },
  );

  assert.equal(executions[0][0], "git");
  assert.deepEqual(executions[0][1].slice(-6), [
    "ls-remote",
    "--symref",
    "--exit-code",
    "http://127.0.0.1:55173/git",
    "HEAD",
    "refs/heads/master",
  ]);
  assert.ok(executions[0][1].includes("protocol.version=2"));
  assert.ok(executions[0][1].includes("credential.helper="));
  assert.equal(executions[0][1].some((argument) => argument.startsWith("http.sslCAInfo=")), false);
  assert.equal(executions[0][2].env.GIT_TERMINAL_PROMPT, "0");
  assert.equal(executions[0][2].env.OPENAI_API_KEY, undefined);

  await assert.rejects(
    verifyLocalGitAdvertisement(
      origin,
      head,
      { PATH: "/bin" },
      async () => advertisement.replaceAll(head, "b".repeat(40)),
    ),
    /did not resolve the current HEAD/,
  );
});

test("development WASM preflight always delegates freshness to the canonical builder", async () => {
  const executions = [];
  let inspections = 0;
  await prepareDevWasm({
    execute: async (...arguments_) => { executions.push(arguments_); },
    inspect: async () => {
      inspections += 1;
      return [];
    },
    isExecutable: async () => true,
  });

  assert.equal(inspections, 2);
  assert.equal(executions.length, 1);
  assert.equal(executions[0][0], "./scripts/build-js-package.sh");
  assert.deepEqual(executions[0][1], []);
  assert.equal(
    executions[0][2],
    fileURLToPath(new URL("../..", import.meta.url)).replace(/\/$/, ""),
  );
});

test("development WASM preflight invalidates partial output before repair", async () => {
  const executions = [];
  let executable = false;
  let inspections = 0;
  let invalidations = 0;
  await prepareDevWasm({
    execute: async (command, arguments_) => {
      executions.push([command, arguments_]);
      if (command === "npm") executable = true;
    },
    inspect: async () => {
      inspections += 1;
      return inspections === 1 ? ["nanocodex_bg.wasm (invalid)"] : [];
    },
    invalidate: async () => { invalidations += 1; },
    isExecutable: async () => executable,
  });

  assert.equal(invalidations, 1);
  assert.deepEqual(executions.map(([command]) => command), ["npm", "./scripts/build-js-package.sh"]);
});

test("development WASM preflight fails if canonical repair stays incomplete", async () => {
  await assert.rejects(
    prepareDevWasm({
      execute: async () => {},
      inspect: async () => ["nanocodex.d.ts"],
      invalidate: async () => {},
      isExecutable: async () => true,
    }),
    /remained incomplete: nanocodex\.d\.ts/,
  );
});

test("local readiness proves the real room lifecycle through the website boundary", async () => {
  const origin = localDevelopmentOrigin("http://127.0.0.1:55173");
  const roomId = `0198d214-0d9d-7a45-8a89-9c411950ab51~${"r".repeat(43)}`;
  const memberId = "0198d214-0d9d-7a45-8a89-9c411950ab52";
  const cookie = `nanocodex_room_${roomId.replaceAll("-", "")}=${"m".repeat(43)}`;
  const events = [];
  const responses = [
    Response.json({ auth_mode: "chatgpt", member_id: memberId, room_id: roomId }, {
      status: 201,
      headers: { "set-cookie": `${cookie}; Path=/v1/rooms; HttpOnly` },
    }),
    new Response(null, { status: 204 }),
  ];
  class ReadyRoomWebSocket extends EventEmitter {
    static instances = [];
    static readyFrame = {
      type: "ready",
      room_id: roomId,
      member_id: memberId,
      members: [{ id: memberId, name: "Local verifier" }],
      online_member_ids: [memberId],
      latest_cursor: "1",
      auth_mode: "chatgpt",
      can_target_agent: true,
      can_end_room: true,
    };
    readyState = 0;
    constructor(url, options) {
      super();
      this.url = url;
      this.options = options;
      ReadyRoomWebSocket.instances.push(this);
      queueMicrotask(() => {
        this.readyState = 1;
        events.push("socket_open");
        this.emit("open");
        this.emit(
          "message",
          Buffer.from(JSON.stringify(this.constructor.readyFrame)),
          false,
        );
      });
    }
    close(code, reason) {
      this.readyState = 3;
      this.closeArguments = [code, reason];
      events.push("socket_close");
      this.emit("close", code, Buffer.from(reason));
    }
    terminate() {
      this.readyState = 3;
      events.push("socket_terminate");
    }
  }
  const calls = [];
  await verifyLocalMultiplayer(origin, async (url, _signal, init) => {
    events.push(init?.method === "DELETE" ? "room_delete" : "room_create");
    calls.push({ url: url.href, ...init });
    return responses.shift();
  }, ReadyRoomWebSocket, 1_000);

  const createPayload = JSON.parse(calls[0].body);
  assert.deepEqual(Object.keys(createPayload).sort(), ["create_id", "display_name"]);
  assert.equal(createPayload.display_name, "Local verifier");
  assert.match(createPayload.create_id, /^[A-Za-z0-9_-]{43}$/);
  const socket = ReadyRoomWebSocket.instances[0];
  assert.equal(
    socket.url.href,
    `ws://127.0.0.1:55173/v1/rooms/${roomId}/ws?cursor=0`,
  );
  assert.deepEqual(socket.options, {
    handshakeTimeout: 1_000,
    headers: { cookie },
    origin: origin.origin,
  });
  assert.deepEqual(socket.closeArguments, [1_000, "readiness_complete"]);
  assert.deepEqual(events, ["room_create", "socket_open", "socket_close", "room_delete"]);

  assert.deepEqual(calls.map(({ url, method, headers }) => ({ url, method, headers })), [
    {
      url: "http://127.0.0.1:55173/v1/rooms",
      method: "POST",
      headers: { "content-type": "application/json", origin: origin.origin },
    },
    {
      url: `http://127.0.0.1:55173/v1/rooms/${roomId}`,
      method: "DELETE",
      headers: { cookie },
    },
  ]);
});

test("local room readiness rejects a malformed ready frame but still deletes the probe room", async () => {
  const origin = localDevelopmentOrigin("http://localhost:55173");
  const roomId = `0198d214-0d9d-7a45-8a89-9c411950ab51~${"r".repeat(43)}`;
  const memberId = "0198d214-0d9d-7a45-8a89-9c411950ab52";
  const cookie = `nanocodex_room_${roomId.replaceAll("-", "")}=${"m".repeat(43)}`;
  const methods = [];
  class InvalidRoomWebSocket extends EventEmitter {
    readyState = 0;
    constructor() {
      super();
      queueMicrotask(() => {
        this.readyState = 1;
        this.emit("open");
        this.emit("message", Buffer.from(JSON.stringify({
          type: "ready",
          room_id: roomId,
          member_id: memberId,
          members: [{ id: memberId, name: "Local verifier" }],
          online_member_ids: [memberId],
          latest_cursor: "1",
          auth_mode: "chatgpt",
          can_target_agent: true,
          can_end_room: true,
          unexpected: "field",
        })), false);
      });
    }
    close() { this.readyState = 3; }
    terminate() { this.readyState = 3; }
  }
  await assert.rejects(
    verifyLocalMultiplayer(
      origin,
      async (_url, _signal, init) => {
        methods.push(init?.method);
        if (init?.method === "POST") {
          return Response.json({ auth_mode: "chatgpt", member_id: memberId, room_id: roomId }, {
            status: 201,
            headers: { "set-cookie": `${cookie}; Path=/v1/rooms; HttpOnly` },
          });
        }
        return new Response(null, { status: 204 });
      },
      InvalidRoomWebSocket,
      1_000,
    ),
    /invalid ready frame/,
  );
  assert.deepEqual(methods, ["POST", "DELETE"]);
});
