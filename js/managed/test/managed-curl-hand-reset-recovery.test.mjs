import assert from "node:assert/strict";
import { fork, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

// Account-object reset journey. Public managed HTTP over the real curl
// executable -> account ingress proxy -> shipped managed worker in workerd ->
// public /v1/account/tool-host WebSocket -> a synthetic local Hand process
// running the shipped attachment and native process tools. The only fault is a
// real workerd reset of the owner's AccountHostedTools object (ctx.abort), the
// failure a deploy or eviction produces: the managed caller's stub to the old
// instance is broken and the Hand WebSocket is disconnected. Recovery must read
// the original call identity's receipt through a fresh stub to the same owner
// object and the same Hand runtime, never resend, and keep explicit cancel.
const root = fileURLToPath(new URL("..", import.meta.url));
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const command = "node --test js/managed/test/managed-curl-hand-reset-recovery.test.mjs";
const machine = "synthetic-curl-reset-hand";
const output = join(repo, "output/managed-curl-hand-recovery", `reset-${Date.now()}-${process.pid}`);
const workspace = join(output, "hand");
const settings = { model: "gpt-6.1-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false };

function frames(raw) {
  return raw.replaceAll("\r\n", "\n").split("\n\n").flatMap(frame => {
    const data = frame.split("\n").filter(line => line.startsWith("data:")).map(line => line.slice(5).trimStart()).join("\n");
    if (!data) return [];
    try { return [{ type: frame.match(/^event: (.+)$/m)?.[1], data: JSON.parse(data) }]; } catch { return []; }
  });
}

test("managed curl journey recovers Hand receipts across a real account object reset without resending", { timeout: 600_000 }, async () => {
  await mkdir(workspace, { recursive: true });
  const runtime = [], hand = [], checks = [], assets = [], tokens = [];
  const result = { command, machine, scenarios: {} };
  let sequence = 0, token, owner, base, mf, failure;
  const publishers = new Map();

  async function curl(label, path, { method = "GET", body, headers = {}, expected = 200, timeout = 150 } = {}) {
    assert.ok(path.startsWith("/v1/"), "only public API routes");
    const name = `${String(++sequence).padStart(3, "0")}-${label}`, prefix = join(output, "curl", name);
    await mkdir(join(output, "curl"), { recursive: true });
    if (body !== undefined) await writeFile(`${prefix}.request.json`, JSON.stringify(body, null, 2));
    const args = ["--silent", "--show-error", "--no-buffer", "--max-time", String(timeout), "--request", method,
      "--dump-header", `${prefix}.headers`, "--config", "-", "-H", "Content-Type: application/json",
      ...Object.entries(headers).flatMap(([key, value]) => ["-H", `${key}: ${value}`]),
      ...(body === undefined ? [] : ["--data-binary", `@${prefix}.request.json`]), new URL(path, base).href];
    const started = Date.now(), child = spawn("curl", args, { stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "", stderr = "";
    child.stdout.on("data", bytes => { stdout += bytes; });
    child.stderr.on("data", bytes => { stderr += bytes; });
    child.stdin.end(`header = ${JSON.stringify(`Authorization: Bearer ${token}`)}\n`);
    const exit = await new Promise((done, reject) => { child.once("error", reject); child.once("close", (code, signal) => done({ code, signal })); });
    const responseHeaders = await readFile(`${prefix}.headers`, "utf8").catch(() => "");
    const status = Number([...responseHeaders.matchAll(/^HTTP\/\S+ (\d+)/gm)].at(-1)?.[1]);
    assert.ok(tokens.every(secret => !stdout.includes(secret) && !responseHeaders.includes(secret)), "credential must not be echoed");
    const duration = Date.now() - started;
    await writeFile(`${prefix}.response`, stdout);
    await writeFile(`${prefix}.receipt.json`, JSON.stringify({ label, method, path, headers, expected, status, ...exit,
      stderr, duration_ms: duration, curl_argv: args, auth_stdin: "Authorization: Bearer <synthetic API key, not recorded>" }, null, 2));
    checks.push({ label, status, duration_ms: duration, evidence: `curl/${name}` });
    assert.equal(status, expected, `${label}: HTTP ${status}: ${stdout.slice(0, 1500)}`);
    assert.equal(exit.code, 0, `${label}: curl exit ${exit.code}: ${stderr}`);
    return { status, raw: stdout, duration, json: () => JSON.parse(stdout), frames: () => frames(stdout), name };
  }
  const execResults = response => response.frames().map(frame => frame.data.event).filter(Boolean)
    .filter(event => event.type === "tool.result" && event.payload.tool === "exec");
  function terminal(response, expected = "turn_completed") {
    const receipt = response.frames().find(frame => frame.type === "run")?.data;
    assert.ok(receipt?.agent_id && receipt?.turn_id, `${response.name}: SSE admission receipt`);
    const end = response.frames().find(frame => frame.data.turn_id === receipt.turn_id
      && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.data.type));
    assert.equal(end?.data.type, expected, `${response.name}: terminal ${response.raw.slice(-2000)}`);
    return end.data;
  }
  const handText = response => {
    const results = execResults(response);
    assert.equal(results.length, 1, `${response.name}: one Code Mode cell`);
    return JSON.stringify(results[0].payload ?? results[0]);
  };
  const step = s => `Run this on the Hand. HAND_STEP ${JSON.stringify({ workdir: `/${machine}`, yield: 30_000, ...s })}`;
  const turn = (label, agent, s) => curl(label, `/v1/agents/${agent}/turns`, { method: "POST", expected: 202,
    headers: { Accept: "text/event-stream", "Idempotency-Key": `${label}-${crypto.randomUUID()}` }, body: { input: step(s) } });

  const file = async name => { try { return await readFile(join(workspace, name), "utf8"); } catch (error) { if (error.code !== "ENOENT") throw error; } };
  const waitFor = async (predicate, description, ms = 20_000) => {
    const deadline = Date.now() + ms;
    while (Date.now() < deadline) { const value = await predicate(); if (value) return value; await delay(25); }
    assert.fail(`${description}: ${JSON.stringify(hand.slice(-15))}`);
  };
  async function publish(label) {
    const endpoint = new URL("/v1/account/tool-host", base); endpoint.protocol = "ws:";
    const child = fork(join(root, "test/fixtures/managed-curl-hand-publisher.mjs"),
      [JSON.stringify({ endpoint: endpoint.href, workspace, machine, label, timeoutMs: 40_000 })], { stdio: ["ignore", "pipe", "pipe", "ipc"] });
    child.stdout.on("data", bytes => runtime.push(`[hand ${label}] ${bytes}`));
    child.stderr.on("data", bytes => runtime.push(`[hand ${label}] ${bytes}`));
    child.on("message", event => hand.push(event));
    child.send({ token });
    publishers.set(label, child);
    await waitFor(() => hand.some(event => event.label === label && event.kind === "ready"), `${label} ready`);
    return child;
  }
  const callFrames = marker => hand.filter(event => event.kind === "frame" && event.direction === "broker"
    && event.frame.type === "call" && String(event.frame.cmd ?? "").includes(marker));
  const brokerFrames = (label, type) => hand.filter(event => event.label === label && event.kind === "frame"
    && event.direction === "broker" && event.frame.type === type);
  const resets = hop => runtime.filter(line => line.includes("fixture.network_fault\"") && line.includes('"fault":"reset"')
    && line.includes(`"hop":"${hop}"`)).length;
  const reconciles = () => runtime.filter(line => line.includes('"type":"hand.receipt.reconcile"'))
    .map(line => JSON.parse(line.slice(line.indexOf("{"))));
  const opensAfter = (label, at) => hand.filter(event => event.label === label && event.kind === "socket" && event.event === "open" && event.at >= at);
  async function arm(user, fault) {
    const armed = await (await mf.getWorker("managed")).fetch("https://fixture.test/__fixture/fault", { method: "POST", body: JSON.stringify({ user, ...fault }) });
    assert.equal(armed.status, 200, await armed.clone().text());
  }

  try {
    const bundle = await build({ entryPoints: [join(root, "test/fixtures/managed-curl-hand-worker.mjs")], bundle: true, write: false,
      format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
      banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
      external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
      plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
        const path = join(args.resolveDir, args.path), contents = await readFile(path), name = `fixture-${assets.length}.wasm`;
        assets.push({ type: "CompiledWasm", path: name, contents, sha256: createHash("sha256").update(contents).digest("hex"), source: path });
        return { path: `./${name}`, external: true };
      }); } }], logLevel: "silent" });
    const proxy = await build({ stdin: { contents: `import {routeManaged} from '../account/worker/managedProxy.ts';
      export default {async fetch(request,env){return await routeManaged(request,env,new URL(request.url)) ?? new Response(null,{status:404})}}`, resolveDir: root },
      bundle: true, write: false, format: "esm", platform: "node", conditions: ["workerd"], target: "es2022", external: ["cloudflare:*", "node:*"], logLevel: "silent" });
    const common = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
    mf = new Miniflare({ host: "127.0.0.1", port: 0, durableObjectsPersist: join(output, "state/sqlite"), r2Persist: join(output, "state/r2"),
      handleRuntimeStdio(stdout, stderr) {
        createInterface({ input: stdout }).on("line", line => runtime.push(line));
        createInterface({ input: stderr }).on("line", line => runtime.push(line));
      }, workers: [
        { ...common, name: "account", modules: true, script: proxy.outputFiles[0].text, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
        { ...common, name: "managed", modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text },
          ...assets.map(({ type, path, contents }) => ({ type, path, contents }))],
          durableObjects: {
            NANOCODEX_SESSIONS: { className: "DurableAgentSession", useSQLite: true },
            NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
            NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true },
            NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true },
            NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
            NANOCODEX_USER_DATA: { className: "UserDataScope", useSQLite: true },
            NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
            NANOCODEX_MEMORY: { className: "FixtureModel", useSQLite: true },
            MODEL: { className: "FixtureModel", useSQLite: true },
          }, serviceBindings: { NANOCODEX: "provider" },
          r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES", "NANOCODEX_USER_DATA_OBJECTS"] },
        { ...common, name: "provider", modules: true,
          script: "export default {fetch(request,env){return env.MODEL.getByName('fixture-model').fetch(request)}};",
          durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } } },
      ] });
    base = await mf.ready;
    const user = crypto.randomUUID();
    const enrolled = await (await mf.getWorker("managed")).fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user }) });
    assert.equal(enrolled.status, 200, await enrolled.clone().text());
    ({ token } = await enrolled.json()); tokens.push(token); owner = user;

    await publish("A");
    const agent = (await curl("create-agent", "/v1/agents", { method: "POST", body: { settings }, expected: 201 })).json().agent_id;
    result.agent = agent;
    await curl("hands-online", "/v1/account/hands");
    const warm = await turn("warm-up", agent, { cmd: "printf WARM_OK" });
    terminal(warm); assert.match(handText(warm), /WARM_OK/);

    // Each scenario records its measurements even when it fails, so one run
    // yields a complete pre/post comparison; failures are asserted together.
    const failures = [];
    const scenario = async (name, body) => { try { await body(); } catch (error) { failures.push(`${name}: ${error.message}`); result.scenarios[name] = { ...result.scenarios[name], failed: error.message.slice(0, 600) }; } };

    // 1. The owner's account object resets while the dispatched command runs.
    // The managed caller's stub is broken; the Hand reconnects to the new
    // instance with the same runtime. The original identity's receipt is
    // recovered through a fresh stub: one effect, never resent, never cancelled.
    await scenario("reset_running", async () => {
      const cancels = brokerFrames("A", "cancel").length, before = resets("managed"), reconciled = reconciles().length;
      const started = Date.now();
      const response = await turn("reset-running-command", agent, { cmd: "printf Z >> reset.log; sleep 2; printf RESET_RECOVERED # __RESET_ACCOUNT_OBJECT__" });
      const end = terminal(response), text = handText(response);
      const record = reconciles().slice(reconciled).at(-1);
      result.scenarios.reset_running = { terminal: end.type, final_message: end.final_message, duration_ms: response.duration,
        call_frames: callFrames("reset.log").length, cancel_frames: brokerFrames("A", "cancel").length - cancels,
        effect: await file("reset.log"), reconcile: record, reattached: opensAfter("A", started).length };
      assert.ok(resets("managed") > before, "real object reset injected after dispatch");
      assert.match(text, /RESET_RECOVERED/, "receipt recovered after the reset");
      assert.equal(await file("reset.log"), "Z", "exactly one side effect");
      assert.equal(callFrames("reset.log").length, 1, "never redispatched");
      assert.equal(brokerFrames("A", "cancel").length, cancels, "a reset never cancels the command");
      assert.ok(opensAfter("A", started).length >= 1, "the same Hand runtime reattached");
      assert.equal(record?.outcome, "recovered", "receipt reconciliation outcome");
    });

    // 2. Selected-Hand lookup interrupted by a reset (stub throws before it
    // answers): one bounded read-only retry through a fresh stub; nothing ran
    // twice and the command runs once. A new agent has no reusable lookup.
    await scenario("reset_selected_lookup", async () => {
      const before = resets("snapshot");
      const lookupAgent = (await curl("create-lookup-agent", "/v1/agents", { method: "POST", body: { settings }, expected: 201 })).json().agent_id;
      await arm(owner, { hop: "snapshot", fault: "reset" });
      const response = await turn("reset-selected-lookup", lookupAgent, { cmd: "printf S >> lookup.log; printf LOOKUP_RECOVERED" });
      const end = terminal(response), text = handText(response);
      result.scenarios.reset_selected_lookup = { terminal: end.type, final_message: end.final_message, duration_ms: response.duration,
        lookup_resets: resets("snapshot") - before, call_frames: callFrames("lookup.log").length, effect: await file("lookup.log") ?? null };
      assert.ok(resets("snapshot") > before, "selected lookup reset injected");
      assert.match(text, /LOOKUP_RECOVERED/, "lookup retried through a fresh stub");
      assert.equal(await file("lookup.log"), "S");
      assert.equal(callFrames("lookup.log").length, 1);
      const lookup = runtime.find(line => line.includes('"type":"hand.selected_lookup.reconcile"'));
      result.scenarios.reset_selected_lookup.reconcile = lookup ? JSON.parse(lookup.slice(lookup.indexOf("{"))) : null;
      assert.match(lookup ?? "", /"outcome":"recovered"/, "interrupted lookup cause recorded with its recovery");
    });

    // 3. Explicit public cancellation after the reset reaches the same runtime
    // through a fresh stub; the command is terminated, never resent.
    await scenario("reset_explicit_cancel", async () => {
      const cancels = brokerFrames("A", "cancel").length, before = resets("managed");
      const admitted = (await curl("reset-cancel-admit", `/v1/agents/${agent}/turns`, { method: "POST", expected: 202,
        headers: { "Idempotency-Key": `reset-cancel-${crypto.randomUUID()}` },
        body: { input: step({ cmd: "printf C >> reset-cancel.log; sleep 5; printf X >> reset-cancel.log # __RESET_ACCOUNT_OBJECT__" }) } })).json();
      const startedAt = Date.now();
      await waitFor(async () => resets("managed") > before && await file("reset-cancel.log") === "C", "reset after cancel command started");
      // Cancel only after the reset broke the caller's stub and the same Hand
      // runtime reattached, while the receipt is being reconciled.
      await waitFor(() => opensAfter("A", startedAt).length >= 1, "same Hand runtime reattached after reset");
      await delay(300);
      await curl("reset-cancel", `/v1/agents/${agent}/turns/${admitted.turn_id}/cancel`, { method: "POST", expected: 202, headers: { "Idempotency-Key": crypto.randomUUID() } });
      let state;
      await waitFor(async () => { state = (await curl("reset-cancel-state", `/v1/agents/${agent}/turns/${admitted.turn_id}`)).json().state;
        return ["completed", "failed", "cancelled"].includes(state); }, "cancelled turn settled", 30_000);
      await delay(6_000);
      const history = (await curl("reset-cancel-history", `/v1/agents/${agent}/events/history?limit=256`)).raw;
      const delivery = runtime.filter(line => line.includes('"type":"hand.receipt.cancel"')).map(line => JSON.parse(line.slice(line.indexOf("{")))).at(-1);
      result.scenarios.reset_explicit_cancel = { cancel_delivery: delivery ?? null, turn_state: state, cancel_frames: brokerFrames("A", "cancel").length - cancels,
        call_frames: callFrames("reset-cancel.log").length, effect: await file("reset-cancel.log"),
        cancel_reported: /cancellation was sent to the Hand|cancellation was recorded and will be delivered/.test(history) };
      assert.equal(state, "cancelled");
      assert.match(delivery?.cancel ?? "", /^(requested|queued|terminal)$/, "cancel after the reset reached the owner ledger through a fresh stub");
      assert.ok(brokerFrames("A", "cancel").length > cancels, "explicit cancel frame delivered to the same Hand runtime");
      assert.equal(await file("reset-cancel.log"), "C", "cancelled command was terminated before its delayed marker");
      assert.equal(callFrames("reset-cancel.log").length, 1);
    });

    // 5 (run before 4, while runtime A is live). SYNTHETIC overload fault, not a
    // real ctx.abort: the managed /invoke hop throws Cloudflare's overloaded=true
    // error after dispatch. Overloaded objects must not be retried: no receipt
    // read or cancellation is sent, the outcome is unknown, the command is never
    // replayed and it runs exactly once.
    await scenario("synthetic_overload", async () => {
      const cancels = brokerFrames("A", "cancel").length, reconciled = reconciles().length;
      const requests = () => runtime.filter(line => line.includes('"type":"fixture.account_request"')).length;
      const before = requests();
      const response = await turn("synthetic-overload", agent, { cmd: "printf O >> overload.log; sleep 1; printf OVERLOAD_RAN # __OVERLOAD_ACCOUNT_OBJECT__" });
      const end = terminal(response), text = handText(response);
      await waitFor(async () => await file("overload.log") === "O", "overloaded command ran");
      await delay(1_500);
      const record = reconciles().slice(reconciled).at(-1);
      result.scenarios.synthetic_overload = { fault: "synthetic overloaded=true error (fixture), not a real overload", terminal: end.type,
        final_message: end.final_message, duration_ms: response.duration, receipt_or_cancel_requests: requests() - before,
        call_frames: callFrames("overload.log").length, cancel_frames: brokerFrames("A", "cancel").length - cancels,
        effect: await file("overload.log"), reconcile: record };
      assert.ok(runtime.some(line => line.includes("fixture.network_fault\"") && line.includes('"fault":"synthetic_overload"')), "synthetic overload injected");
      assert.equal(record?.error_flags?.overloaded, true, "original error flag overloaded preserved");
      assert.match(record?.error_class ?? "", /\/overloaded/, "classified as overloaded");
      assert.equal(record?.outcome, "receipt_unreachable");
      assert.equal(record?.polls, 0, "no receipt poll");
      assert.equal(requests() - before, 0, "zero receipt reads or cancellations reached the overloaded object");
      assert.doesNotMatch(text, /OVERLOAD_RAN/);
      assert.match(text, /Execution outcome is unknown; the command was not resent/);
      assert.equal(callFrames("overload.log").length, 1, "exactly one command call, never replayed");
      assert.equal(brokerFrames("A", "cancel").length, cancels, "no cancellation");
      assert.equal(await file("overload.log"), "O", "exactly one side effect");
    });

    // 4. Reset plus Hand runtime replacement: the old call stays pinned to its
    // original runtime and is reported unknown; the replacement never gets it.
    await scenario("reset_runtime_replaced", async () => {
      const before = resets("managed");
      const pending = turn("reset-runtime-replaced", agent, { cmd: "printf P >> reset-replaced.log; sleep 30; printf RESET_REPLACED_RAN # __RESET_ACCOUNT_OBJECT__" });
      await waitFor(async () => resets("managed") > before && await file("reset-replaced.log") === "P", "reset replaced command started");
      publishers.get("A").kill("SIGKILL");
      await publish("B");
      const response = await pending;
      const end = terminal(response), text = handText(response);
      result.scenarios.reset_runtime_replaced = { terminal: end.type, final_message: end.final_message, duration_ms: response.duration,
        call_frames: callFrames("reset-replaced.log").length, replacement_calls: brokerFrames("B", "call").length };
      assert.doesNotMatch(text, /RESET_REPLACED_RAN/);
      assert.match(text, /ambiguous|unknown|not resent|replaced|runtime identity/i);
      assert.equal(callFrames("reset-replaced.log").length, 1, "never sent to the replacement runtime");
      assert.equal(brokerFrames("B", "call").length, 0);
      const follow = await turn("reset-follow-up", agent, { cmd: "printf F >> reset-follow.log; printf RESET_FOLLOW_OK" });
      terminal(follow); assert.match(handText(follow), /RESET_FOLLOW_OK/);
      assert.equal(await file("reset-follow.log"), "F");
    });
    console.log(JSON.stringify({ evidence: output, scenarios: result.scenarios }));
    assert.deepEqual(failures, [], "every reset scenario recovers");
    for (const [name, text] of [["hand-wire", JSON.stringify(hand)], ["runtime", runtime.join("\n")]]) {
      assert.ok(tokens.every(secret => !text.includes(secret)), `${name} log must not contain an API key`);
    }
    console.log(JSON.stringify({ evidence: output, scenarios: result.scenarios }));
  } catch (error) { failure = error; result.error = error.stack; throw error; }
  finally {
    for (const [, child] of publishers) { if (child.exitCode === null && !child.killed) { try { child.send({ op: "close" }); } catch {} await Promise.race([new Promise(done => child.once("exit", done)), delay(3_000)]); child.kill("SIGKILL"); } }
    try { await mf?.dispose(); } catch {}
    const scrub = text => tokens.reduce((value, secret) => value.replaceAll(secret, "<redacted>"), text);
    await writeFile(join(output, "hand-wire.json"), scrub(JSON.stringify(hand, null, 2)) + "\n");
    await writeFile(join(output, "runtime.log"), scrub(runtime.join("\n")));
    await writeFile(join(output, "checks.json"), JSON.stringify(checks, null, 2));
    await writeFile(join(output, "result.json"), scrub(JSON.stringify(result, null, 2)) + "\n");
    await writeFile(join(output, "README.md"), `Run: \`${command}\`\n\nStatus: ${failure ? "FAIL: " + failure.message : "PASS"}\n\nEvidence: curl/*.{request.json,headers,response,receipt.json} (every managed API request), hand-wire.json (publisher frames per Hand process), hand/*.log (native side effects), runtime.log (workerd, including fixture.network_fault reset and hand.receipt.reconcile records), result.json (per-scenario measurements).\n`);
  }
});

