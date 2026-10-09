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

// Public managed HTTP over the real curl executable -> account ingress proxy ->
// shipped managed worker (sessions, Code Mode WASM, AccountHostedTools broker,
// SQLite) in workerd -> public /v1/account/tool-host WebSocket -> a synthetic
// local Hand process running the shipped attachment and native process tools.
// Fixtures: first identity enrollment and the external model provider only.
const root = fileURLToPath(new URL("..", import.meta.url));
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const command = "node --test js/managed/test/managed-curl-hand-recovery.test.mjs";
const machine = "synthetic-curl-recovery-hand";
const output = join(repo, "output/managed-curl-hand-recovery", `${Date.now()}-${process.pid}`);
const workspace = join(output, "hand");
const settings = { model: "gpt-6.1-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false };

function frames(raw) {
  return raw.replaceAll("\r\n", "\n").split("\n\n").flatMap(frame => {
    const data = frame.split("\n").filter(line => line.startsWith("data:")).map(line => line.slice(5).trimStart()).join("\n");
    if (!data) return [];
    try { return [{ type: frame.match(/^event: (.+)$/m)?.[1], id: frame.match(/^id: (.+)$/m)?.[1], data: JSON.parse(data) }]; }
    catch { return []; }
  });
}

test("managed curl journey reconciles Hand receipts and reports unknown outcomes without resending", { timeout: 300_000 }, async () => {
  await mkdir(workspace, { recursive: true });
  const runtime = [], hand = [], checks = [], assets = [];
  const result = { command, machine, scenarios: {} };
  let sequence = 0, token, base, mf, failure;
  const publishers = new Map();

  // Every managed API request: real curl, bearer on stdin, full transcript saved.
  async function curl(label, path, { method = "GET", body, headers = {}, expected = 200, timeout = 120 } = {}) {
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
    assert.ok(!stdout.includes(token) && !responseHeaders.includes(token), "credential must not be echoed");
    await writeFile(`${prefix}.response`, stdout);
    await writeFile(`${prefix}.receipt.json`, JSON.stringify({ label, method, path, headers, expected, status, ...exit,
      stderr, duration_ms: Date.now() - started, curl_argv: args, auth_stdin: "Authorization: Bearer <synthetic API key, not recorded>" }, null, 2));
    checks.push({ label, status, evidence: `curl/${name}` });
    assert.equal(status, expected, `${label}: HTTP ${status}: ${stdout.slice(0, 1500)}`);
    assert.equal(exit.code, 0, `${label}: curl exit ${exit.code}: ${stderr}`);
    return { status, raw: stdout, json: () => JSON.parse(stdout), frames: () => frames(stdout), name };
  }
  const events = response => response.frames().map(frame => frame.data.event).filter(Boolean);
  const execResults = response => events(response).filter(event => event.type === "tool.result" && event.payload.tool === "exec");
  function terminal(response, expected = "turn_completed") {
    const receipt = response.frames().find(frame => frame.type === "run")?.data;
    assert.ok(receipt?.agent_id && receipt?.turn_id, `${response.name}: SSE admission receipt`);
    const end = response.frames().find(frame => frame.data.turn_id === receipt.turn_id
      && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.data.type));
    assert.equal(end?.data.type, expected, `${response.name}: terminal ${response.raw.slice(-2000)}`);
    return end.data;
  }
  const turn = (label, agent, step) => curl(label, `/v1/agents/${agent}/turns`, { method: "POST", expected: 202, timeout: 150,
    headers: { Accept: "text/event-stream", "Idempotency-Key": `${label}-${crypto.randomUUID()}` },
    body: { input: `Run this on the Hand. HAND_STEP ${JSON.stringify({ workdir: `/${machine}`, yield: 30_000, ...step })}` } });

  // Hand helpers: one OS process per publisher runtime.
  const file = async name => { try { return await readFile(join(workspace, name), "utf8"); } catch (error) { if (error.code !== "ENOENT") throw error; } };
  const waitFor = async (predicate, description, ms = 20_000) => {
    const deadline = Date.now() + ms;
    while (Date.now() < deadline) { const value = await predicate(); if (value) return value; await delay(25); }
    assert.fail(`${description}: ${JSON.stringify(hand.slice(-15))}`);
  };
  async function publish(label, timeoutMs) {
    const endpoint = new URL("/v1/account/tool-host", base); endpoint.protocol = "ws:";
    const child = fork(join(root, "test/fixtures/managed-curl-hand-publisher.mjs"),
      [JSON.stringify({ endpoint: endpoint.href, workspace, machine, label, timeoutMs })], { stdio: ["ignore", "pipe", "pipe", "ipc"] });
    child.stdout.on("data", bytes => runtime.push(`[hand ${label}] ${bytes}`));
    child.stderr.on("data", bytes => runtime.push(`[hand ${label}] ${bytes}`));
    child.on("message", event => hand.push(event));
    child.send({ token });
    publishers.set(label, child);
    await waitFor(() => hand.some(event => event.label === label && event.kind === "ready"), `${label} ready`);
    return child;
  }
  const control = (label, op, extra = {}) => publishers.get(label).send({ op, ...extra });
  const opens = label => hand.filter(event => event.label === label && event.kind === "socket" && event.event === "open").length;
  const callFrames = marker => hand.filter(event => event.kind === "frame" && event.direction === "broker"
    && event.frame.type === "call" && String(event.frame.cmd ?? "").includes(marker));
  const brokerFrames = (label, type) => hand.filter(event => event.label === label && event.kind === "frame"
    && event.direction === "broker" && event.frame.type === type);
  const handResult = response => {
    const results = execResults(response);
    assert.equal(results.length, 1, `${response.name}: one Code Mode cell`);
    const payload = results[0].payload ?? results[0];
    return { payload, text: JSON.stringify(payload) };
  };

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
    const enrolled = await (await mf.getWorker("managed")).fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user: crypto.randomUUID() }) });
    assert.equal(enrolled.status, 200, await enrolled.clone().text());
    ({ token } = await enrolled.json());
    result.wasm = assets.map(({ source, sha256 }) => ({ source, sha256 }));

    await publish("A", 8_000);
    const created = await curl("create-agent", "/v1/agents", { method: "POST", body: { settings }, expected: 201 });
    const agent = created.json().agent_id;
    result.agent = agent;
    await curl("hands-online", "/v1/account/hands");

    // 1. Known receipt reconciles: transport loss while the command runs; the
    // command finishes offline; the same runtime reconnects and replays it.
    {
      const pending = turn("reconcile-known-receipt", agent, { cmd: "printf R >> reconcile.log; while [ ! -f release-reconcile ]; do sleep 0.05; done; printf RECONCILED_OK" });
      await waitFor(async () => await file("reconcile.log") === "R", "reconcile command started");
      control("A", "hold"); control("A", "drop");
      await waitFor(() => hand.some(event => event.label === "A" && event.kind === "reconnect_held"), "A offline");
      await writeFile(join(workspace, "release-reconcile"), "");
      await waitFor(() => hand.some(event => event.label === "A" && event.kind === "attachment" && event.event === "result_retained"), "receipt retained offline");
      const opened = opens("A"); control("A", "release");
      const response = await pending;
      const end = terminal(response), { text } = handResult(response);
      assert.match(text, /RECONCILED_OK/);
      assert.equal(await file("reconcile.log"), "R", "exactly one side effect");
      assert.equal(callFrames("release-reconcile").length, 1, "call dispatched exactly once");
      assert.ok(opens("A") > opened, "publisher reconnected");
      assert.ok(brokerFrames("A", "recover").length >= 1, "broker requested recovery of the dispatched call");
      result.scenarios.reconcile = { terminal: end.type, final_message: end.final_message, call_frames: 1, recover_frames: brokerFrames("A", "recover").length, effect: await file("reconcile.log") };
    }

    // 2. Dispatched call lost in transit; the same living runtime reconnects
    // without proof: explicit unknown outcome, never resent, no side effect.
    {
      control("A", "lose", { marker: "lost.log" });
      const recovers = brokerFrames("A", "recover").length;
      const response = await turn("no-retained-proof", agent, { cmd: "printf X >> lost.log; printf LOST_RAN" });
      const end = terminal(response), { text } = handResult(response);
      assert.ok(hand.some(event => event.kind === "lost_in_transit"), "call frame was lost after dispatch");
      assert.match(text, /no retained proof of this dispatched call; it was not resent/);
      assert.doesNotMatch(text, /LOST_RAN/);
      assert.equal(await file("lost.log"), undefined, "no side effect");
      assert.equal(callFrames("lost.log").length, 1, "never redispatched");
      assert.ok(brokerFrames("A", "recover").length > recovers);
      assert.ok(hand.some(event => event.kind === "frame" && event.direction === "publisher" && event.frame.type === "status" && event.frame.state === "missing"));
      result.scenarios.no_retained_proof = { terminal: end.type, final_message: end.final_message, call_frames: callFrames("lost.log").length, effect: null };
    }

    // 3. Durable call deadline elapses while the Hand is offline.
    {
      const pending = turn("deadline-after-dispatch", agent, { cmd: "printf D >> deadline.log; sleep 1; printf DEADLINE_RAN" });
      await waitFor(async () => await file("deadline.log") === "D", "deadline command started");
      control("A", "hold"); control("A", "drop");
      const response = await pending;
      const end = terminal(response), { text } = handResult(response);
      control("A", "release");
      assert.match(text, /deadline expired after dispatch/);
      assert.equal(await file("deadline.log"), "D", "started once, never resent");
      assert.equal(callFrames("deadline.log").length, 1);
      result.scenarios.deadline = { terminal: end.type, final_message: end.final_message, call_frames: 1, effect: await file("deadline.log") };
    }

    // 4. Publisher replacement: the Hand process dies mid-command and a new
    // runtime publishes the same machine. The old call is explicitly unknown.
    {
      await waitFor(() => hand.filter(event => event.label === "A" && event.kind === "socket" && event.event === "open").length >= 3, "A back online");
      const pending = turn("publisher-replaced", agent, { cmd: "printf P >> replaced.log; sleep 30; printf REPLACED_RAN" });
      await waitFor(async () => await file("replaced.log") === "P", "replaced command started");
      publishers.get("A").kill("SIGKILL");
      await publish("B", 8_000);
      const response = await pending;
      const end = terminal(response), { text } = handResult(response);
      assert.doesNotMatch(text, /REPLACED_RAN/);
      assert.match(text, /ambiguous|unknown|not resent|replaced|runtime identity/i);
      assert.equal(callFrames("replaced.log").length, 1, "replacement runtime never receives the old call");
      assert.equal(brokerFrames("B", "call").length, 0);
      result.scenarios.replaced = { terminal: end.type, final_message: end.final_message, call_frames: 1, effect: await file("replaced.log") };
    }

    // 5. Recovery follow-up on the replacement Hand works on the same agent.
    {
      const response = await turn("follow-up", agent, { cmd: "printf F >> follow.log; printf FOLLOW_UP_OK" });
      const end = terminal(response), { text } = handResult(response);
      assert.match(text, /FOLLOW_UP_OK/);
      assert.equal(await file("follow.log"), "F");
      assert.equal(brokerFrames("B", "call").length, 1);
      result.scenarios.follow_up = { terminal: end.type, final_message: end.final_message, effect: await file("follow.log") };
    }
    // 6. Managed->account response lost while the command runs (fault-injected
    // network loss): the command is not cancelled, its original identity's
    // receipt is reconciled on the same runtime, and it runs exactly once.
    {
      const cancels = brokerFrames("B", "cancel").length;
      const response = await turn("account-response-lost", agent, { cmd: "printf L >> response-lost.log; sleep 1; printf RESPONSE_LOSS_RECOVERED # __LOSE_ACCOUNT_RESPONSE__" });
      const end = terminal(response), { text } = handResult(response);
      assert.match(text, /RESPONSE_LOSS_RECOVERED/);
      assert.equal(await file("response-lost.log"), "L", "exactly one side effect");
      assert.equal(callFrames("response-lost.log").length, 1, "never redispatched");
      assert.equal(brokerFrames("B", "cancel").length, cancels, "transport loss never cancels the command");
      assert.ok(runtime.some(line => line.includes("fixture.network_fault") && line.includes("lose_response")), "fault injected");
      const reconciled = runtime.find(line => line.includes("hand.receipt.reconcile") && line.includes('"outcome":"recovered"'));
      assert.ok(reconciled, "receipt recovery observed with its sanitized cause");
      assert.match(reconciled, /"error_class":"Error\/network_lost: Network connection lost\."/);
      result.scenarios.response_lost = { terminal: end.type, final_message: end.final_message, call_frames: 1, cancel_frames: 0, effect: await file("response-lost.log"), telemetry: JSON.parse(reconciled.slice(reconciled.indexOf("{"))) };
    }

    // 7. The account answered but the body was truncated in transit.
    {
      const response = await turn("account-response-truncated", agent, { cmd: "printf T >> truncated.log; printf TRUNCATED_RECOVERED # __TRUNCATE_ACCOUNT_RESPONSE__" });
      const end = terminal(response), { text } = handResult(response);
      assert.match(text, /TRUNCATED_RECOVERED/);
      assert.equal(await file("truncated.log"), "T", "exactly one side effect");
      assert.equal(callFrames("truncated.log").length, 1, "never redispatched");
      result.scenarios.response_truncated = { terminal: end.type, final_message: end.final_message, call_frames: 1, effect: await file("truncated.log") };
    }

    // 7b. write_stdin response lost while the process is still writing: the
    // poll is reconciled receipt-only, stdin is written exactly once and its
    // output is not lost.
    {
      const response = await turn("stdin-response-lost", agent, { yield: 500, stdin: "__LOSE_ACCOUNT_RESPONSE__\n",
        cmd: "while read line; do printf 'GOT:%s\\n' \"$line\" >> stdin.log; sleep 1; printf 'ECHO_%s' \"$line\"; done" });
      const end = terminal(response), { text } = handResult(response);
      assert.match(text, /ECHO___LOSE_ACCOUNT_RESPONSE__/, "stdin output survives the lost response");
      assert.equal(await file("stdin.log"), "GOT:__LOSE_ACCOUNT_RESPONSE__\n", "stdin delivered exactly once");
      const stdinCalls = hand.filter(event => event.kind === "frame" && event.direction === "broker" && event.frame.type === "call" && event.frame.name === "write_stdin");
      assert.equal(stdinCalls.length, 1, "write_stdin never redispatched");
      result.scenarios.stdin_response_lost = { terminal: end.type, final_message: end.final_message, stdin_calls: 1, effect: await file("stdin.log") };
    }

    // The durable public history retains every explicit outcome after recovery.
    const history = await curl("events-history", `/v1/agents/${agent}/events/history?limit=256`);
    const retained = history.json().data.filter(row => row.type === "event" && row.event?.type === "tool.result" && row.event.payload.tool === "exec");
    const outcomes = ["RECONCILED_OK", "no retained proof of this dispatched call; it was not resent",
      "deadline expired after dispatch", "became ambiguous when its host was replaced", "FOLLOW_UP_OK",
      "RESPONSE_LOSS_RECOVERED", "TRUNCATED_RECOVERED", "ECHO___LOSE_ACCOUNT_RESPONSE__"];
    assert.equal(retained.length, outcomes.length, "one retained Code Mode result per turn");
    outcomes.forEach((outcome, index) => assert.ok(JSON.stringify(retained[index].event.payload).includes(outcome), `history retains ${outcome}`));
    result.history = retained.map(row => ({ cursor: row.cursor, turn_id: row.event.payload.turn_id, status: row.event.payload.status }));
    // 8. Explicit public turn cancellation still reaches the running command
    // now that HTTP transport loss is no longer treated as cancellation.
    {
      const cancels = brokerFrames("B", "cancel").length;
      const admitted = (await curl("explicit-cancel-admit", `/v1/agents/${agent}/turns`, { method: "POST", expected: 202,
        headers: { "Idempotency-Key": `explicit-cancel-${crypto.randomUUID()}` },
        body: { input: `Run this on the Hand. HAND_STEP ${JSON.stringify({ workdir: `/${machine}`, yield: 30_000, cmd: "printf C >> cancel.log; sleep 3; printf X >> cancel.log" })}` } })).json();
      await waitFor(async () => await file("cancel.log") === "C", "cancel command started");
      await curl("explicit-cancel", `/v1/agents/${agent}/turns/${admitted.turn_id}/cancel`, { method: "POST", expected: 202, headers: { "Idempotency-Key": crypto.randomUUID() } });
      await waitFor(() => brokerFrames("B", "cancel").length > cancels, "explicit cancel frame delivered to the Hand");
      let state;
      await waitFor(async () => { state = (await curl("explicit-cancel-state", `/v1/agents/${agent}/turns/${admitted.turn_id}`)).json().state;
        return ["completed", "failed", "cancelled"].includes(state); }, "cancelled turn settled", 30_000);
      assert.equal(state, "cancelled");
      // Wait well past the command's own 3s marker: it must have been terminated.
      await delay(4_500);
      assert.equal(await file("cancel.log"), "C", "cancelled command was terminated before its delayed marker");
      assert.equal(callFrames("cancel.log").length, 1);
      result.scenarios.explicit_cancel = { turn_state: state, cancel_frames: brokerFrames("B", "cancel").length - cancels, effect: await file("cancel.log") };
    }
    // Hand processes and workerd never print the credential.
    for (const [name, text] of [["hand-wire", JSON.stringify(hand)], ["runtime", runtime.join("\n")]]) {
      assert.ok(!text.includes(token), `${name} log must not contain the API key`);
    }
    console.log(JSON.stringify({ evidence: output, scenarios: result.scenarios }));
  } catch (error) { failure = error; result.error = error.stack; throw error; }
  finally {
    for (const [label, child] of publishers) { if (child.exitCode === null && !child.killed) { try { child.send({ op: "close" }); } catch {} await Promise.race([new Promise(done => child.once("exit", done)), delay(3_000)]); child.kill("SIGKILL"); } void label; }
    try { await mf?.dispose(); } catch {}
    // Defense in depth only; the try block asserts the logs never contain it.
    const scrub = text => token ? text.replaceAll(token, "<redacted>") : text;
    await writeFile(join(output, "hand-wire.json"), scrub(JSON.stringify(hand, null, 2)) + "\n");
    await writeFile(join(output, "runtime.log"), scrub(runtime.join("\n")));
    await writeFile(join(output, "checks.json"), JSON.stringify(checks, null, 2));
    await writeFile(join(output, "result.json"), scrub(JSON.stringify(result, null, 2)) + "\n");
    await writeFile(join(output, "README.md"), `Run: \`${command}\`\n\nStatus: ${failure ? "FAIL: " + failure.message : "PASS"}\n\nEvidence: curl/*.{request.json,headers,response,receipt.json} (every managed API request), hand-wire.json (publisher frames/observations per Hand process), hand/*.log (native side effects), runtime.log, result.json.\n`);
  }
});
