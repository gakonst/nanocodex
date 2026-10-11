import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire, builtinModules } from "node:module";
import { fetch } from "./support/miniflare-fetch.mjs";

const require = createRequire(import.meta.url);
const { build } = require("esbuild");
const { Miniflare } = require("miniflare");
const { WebSocket } = require("ws");
const managed = fileURLToPath(new URL("..", import.meta.url));
const repo = resolve(managed, "../..");
const binary = resolve(process.env.NATIVE_APP_BINARY ?? resolve(repo, "apple/NanocodexApps/.build/debug/native-app-journey"));
const sha256 = source => createHash("sha256").update(source).digest("hex");

async function executeSwift(requestPath) {
  const command = [binary, "--validate-json"];
  const child = spawn(command[0], command.slice(1), { stdio: ["pipe", "pipe", "pipe"] });
  child.stdin.end(await readFile(requestPath));
  const stdout = [], stderr = [];
  let bytes = 0;
  const timer = setTimeout(() => child.kill("SIGKILL"), 15_000);
  for (const [stream, chunks] of [[child.stdout, stdout], [child.stderr, stderr]]) {
    stream.on("data", chunk => {
      bytes += chunk.length;
      if (bytes > 2 * 1024 * 1024) child.kill("SIGKILL");
      chunks.push(chunk);
    });
  }
  try {
    const [code, signal] = await once(child, "close");
    return { command, code, signal, stdout: Buffer.concat(stdout).toString(), stderr: Buffer.concat(stderr).toString() };
  } finally {
    clearTimeout(timer);
  }
}

test("cloud apps tool validates real Swift before saving and preserves source/state on failure", { timeout: 120_000 }, async () => {
  await access(binary);
  const output = resolve(process.env.NATIVE_APP_VALIDATION_OUTPUT ?? resolve(repo, "output/native-app-validation"), new Date().toISOString().replaceAll(":", "-"));
  await mkdir(output, { recursive: true });
  const trace = [], validatorCalls = [];
  let available = true, mf, handSocket;
  const handFrames = [];
  async function nativeValidate(input) {
    const index = validatorCalls.length + 1;
    validatorCalls.push({ input });
    const inputPath = resolve(output, `${index}-request.json`);
    await writeFile(inputPath, JSON.stringify(input, null, 2));
    const execution = await executeSwift(inputPath);
    await writeFile(resolve(output, `${index}-execution.json`), JSON.stringify(execution, null, 2));
    assert.equal(execution.signal, null, execution.stderr);
    const result = JSON.parse(execution.stdout);
    assert.equal(typeof result.valid, "boolean", execution.stdout);
    assert.ok(execution.code === 0 || (execution.code === 1 && !result.valid), execution.stderr);
    validatorCalls[index - 1].result = result;
    return result;
  }
  const bridge = createServer(async (request, response) => {
    try {
      if (request.method !== "POST" || request.url !== "/validate") {
        response.writeHead(404).end(); return;
      }
      if (!available) {
        response.writeHead(503, { "content-type": "application/json" }).end(JSON.stringify({ error: "fixture_validator_offline" })); return;
      }
      const chunks = [];
      let size = 0;
      for await (const chunk of request) {
        size += chunk.length;
        if (size > 2 * 1024 * 1024) throw new Error("Validation request exceeded limit");
        chunks.push(chunk);
      }
      const input = JSON.parse(Buffer.concat(chunks).toString());
      const result = await nativeValidate(input);
      response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
    } catch (error) {
      trace.push({ transport_error: String(error) });
      response.writeHead(502, { "content-type": "application/json" }).end(JSON.stringify({ error: String(error) }));
    }
  });
  bridge.listen(0, "127.0.0.1");
  await once(bridge, "listening");
  const validatorURL = `http://127.0.0.1:${bridge.address().port}/validate`;
  try {
    const bundle = await build({
      entryPoints: [resolve(managed, "test/native-app-validation-worker.ts")],
      bundle: true, write: false, format: "esm", target: "es2022", platform: "browser", external: ["node:*", "cloudflare:workers", ...builtinModules],
      alias: { "node-rsa": resolve(managed, "node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    });
    await writeFile(resolve(output, "worker.mjs"), bundle.outputFiles[0].text);
    mf = new Miniflare({
      host: "127.0.0.1", port: 0, modules: true, script: bundle.outputFiles[0].text,
      compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      d1Databases: { NANOCODEX_CRM: "native-app-validation" },
      durableObjects: { NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true } },
      bindings: { SWIFT_VALIDATOR_URL: validatorURL },
    });
    const base = await mf.ready;
    const db = await mf.getD1Database("NANOCODEX_CRM");
    const migration = await readFile(resolve(managed, "migrations/0011_prompt_apps.sql"), "utf8");
    await db.exec(migration.replace(/^--.*$/gm, "").replaceAll("\n", " "));
    const snapshot = async () => (await db.prepare("SELECT * FROM prompt_apps ORDER BY owner_id,id").all()).results;
    async function call(scenario, input, headers = {}) {
      const response = await fetch(new URL("/tools/apps", base), {
        method: "POST", headers: { authorization: "Bearer owner", "content-type": "application/json", "x-fixture-call-id": scenario, ...headers },
        body: JSON.stringify(input),
      });
      const text = await response.text();
      let value;
      try { value = JSON.parse(text); } catch { assert.fail(`${scenario}: HTTP ${response.status}: ${text}`); }
      trace.push({ scenario, input, status: response.status, value });
      return { status: response.status, value };
    }
    const source = await readFile(resolve(repo, "apple/Tests/NativeAppsUI/Fixtures/cigarettes.swift"), "utf8");
    // The generated counter failure: record fields were emitted with defaults.
    // The shared Swift lowering pass rejects these, even though ordinary Swift
    // syntax permits them. This must never become a D1 app document.
    const invalid = `import SwiftUI\nstruct CounterEntry {\n    var id = UUID().uuidString\n    var timestamp = Date().timeIntervalSince1970\n}\nstruct Counter: View {\n    @Persisted("entries") var entries: [CounterEntry] = []\n    var body: some View {\n        Form {\n            Text("Cigarettes: \\(entries.count)")\n            Button("Log cigarette") { entries.append(CounterEntry()) }\n        }\n    }\n}\n`;
    const document = { title: "Synthetic cigarette counter", description: "Native validation journey", runtime: "swift-v1", source };
    await writeFile(resolve(output, "valid-counter.swift"), source);
    await writeFile(resolve(output, "invalid-counter.swift"), invalid);
    assert.deepEqual(await snapshot(), []);
    const rejected = await call("invalid-create", { operation: "save", ...document, source: invalid });
    assert.equal(rejected.value.error, "app_validation_failed", JSON.stringify(rejected));
    assert.deepEqual(await snapshot(), [], "invalid Swift must not create an app");
    const badEvidence = validatorCalls.at(-1).result;
    assert.equal(badEvidence.valid, false);
    assert.equal(badEvidence.source_sha256, sha256(invalid));
    assert.match(JSON.stringify(badEvidence.diagnostic), /Record fields|initializer/i);

    const steps = [
      { action: "expect", text: "Cigarettes: 0" },
      { action: "tap", title: "Log cigarette" },
      { action: "tap", title: "Log cigarette" },
      { action: "expect", text: "Cigarettes: 2" },
      { action: "tap", title: "Undo last cigarette" },
      { action: "expect", text: "Cigarettes: 1" },
      { action: "reopen" },
      { action: "expect", text: "Cigarettes: 1" },
    ];
    const validation = await call("validate-log-undo-reopen", { operation: "validate", runtime: "swift-v1", source, steps });
    assert.equal(validation.status, 200, JSON.stringify(validation));
    assert.equal(validatorCalls.at(-1).result.valid, true);
    assert.equal(validatorCalls.at(-1).result.source_sha256, sha256(source));
    assert.equal(validatorCalls.at(-1).result.persisted_test_state.count, 1);
    assert.ok(validatorCalls.at(-1).result.checks.some(check => check.stage === "reopen" && check.passed));
    assert.deepEqual(validatorCalls.at(-1).input.steps, steps, "cloud tool must forward the actual requested actions");
    assert.deepEqual(await snapshot(), [], "validation must not save an app or its test data");

    const created = await call("valid-create", { operation: "save", ...document });
    assert.equal(created.status, 200, JSON.stringify(created));
    const id = created.value.id;
    assert.equal(typeof id, "string");
    assert.equal(created.value.revision, 1);
    assert.equal(created.value.source, source);
    assert.equal(validatorCalls.at(-1).result.valid, true);
    assert.equal(validatorCalls.at(-1).input.source, source);
    const seeded = await call("set-real-app-data", { operation: "data_set", id, revision: 0, value: { count: 7, coach: "", future: { keep: true } } });
    assert.equal(seeded.status, 200, JSON.stringify(seeded));
    const before = await snapshot();
    const edit = await call("invalid-edit", { operation: "save", ...document, id, revision: 1, source: invalid });
    assert.equal(edit.value.error, "app_validation_failed", JSON.stringify(edit));
    assert.deepEqual(await snapshot(), before, "invalid edits must preserve source, revisions, recovery source and persisted data");
    const saved = await call("read-after-failed-edit", { operation: "get", id });
    assert.equal(saved.value.source, source);
    assert.equal(saved.value.revision, 1);
    const data = await call("data-after-failed-edit", { operation: "data_get", id });
    assert.deepEqual(data.value.value, { count: 7, coach: "", future: { keep: true } });
    const reopen = await call("validate-saved-state", { operation: "validate", runtime: "swift-v1", source: saved.value.source, state: data.value.value,
      steps: [{ action: "expect", text: "Cigarettes: 7" }, { action: "tap", title: "Log cigarette" }, { action: "reopen" }, { action: "expect", text: "Cigarettes: 8" }] });
    assert.equal(reopen.status, 200, JSON.stringify(reopen));
    assert.equal(validatorCalls.at(-1).result.valid, true);
    assert.deepEqual(await snapshot(), before, "validation actions must not mutate actual account state");

    async function http(scenario, path = "", method = "GET", body, account = "owner", headers = {}) {
      const response = await fetch(new URL("/v1/apps" + path, base), {
        method, headers: { authorization: "Bearer " + account, "content-type": "application/json", ...headers },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
      const value = await response.json();
      trace.push({ scenario, surface: "http", path, method, account, status: response.status, value });
      assert.equal(response.headers.get("cache-control"), "no-store");
      assert.equal(response.headers.get("x-content-type-options"), "nosniff");
      return { status: response.status, value };
    }
    // The public HTTP route uses the same native validation callback and actual
    // D1 database. Only account authentication is a fixture in this worker.
    for (const [account, method, expected] of [["anonymous", "GET", 401], ["connect", "GET", 403], ["no-tools", "GET", 403], ["read", "POST", 403], ["write", "GET", 403]]) {
      assert.equal((await http("http-authorization-" + account, "", method, method === "POST" ? document : undefined, account)).status, expected);
    }
    assert.equal((await http("http-cookie-no-origin", "", "POST", document, "cookie")).status, 403);
    assert.equal((await http("http-cookie-wrong-origin", "", "POST", document, "cookie", { origin: "https://evil.test" })).status, 403);
    const rejectedHTTP = await http("http-invalid-create", "", "POST", { ...document, source: invalid });
    assert.equal(rejectedHTTP.status, 422);
    assert.equal(rejectedHTTP.value.validation.valid, false);
    assert.deepEqual(await snapshot(), before);
    const createdHTTP = await http("http-valid-create", "", "POST", document, "cookie", { origin: base.origin });
    assert.equal(createdHTTP.status, 201);
    assert.equal(createdHTTP.value.validation.valid, true);
    assert.equal(createdHTTP.value.validation.source_sha256, sha256(source));
    const path = "/" + createdHTTP.value.id;
    assert.equal((await http("http-read-created", path)).value.source, source);
    assert.equal((await http("http-validate-created", "/validate", "POST", { id: createdHTTP.value.id })).value.valid, true);
    assert.deepEqual((await http("http-other-owner-list", "", "GET", undefined, "other")).value.apps, []);
    for (const [suffix, method, body] of [["", "GET"], ["", "PUT", { ...document, revision: 1 }], ["/data", "GET"], ["/data", "PUT", { value: {}, revision: 0 }], ["?revision=1", "DELETE"], ["/restore", "POST", { revision: 1 }]]) {
      assert.equal((await http("http-other-owner-" + method + suffix, path + suffix, method, body, "other")).status, 404);
    }
    const page = (await http("http-page-one", "?limit=1")).value;
    assert.equal(page.apps.length, 1);
    assert.equal(page.apps[0].source, undefined);
    assert.equal(typeof page.next_cursor, "string");
    const next = (await http("http-page-two", "?limit=1&cursor=" + page.next_cursor)).value;
    assert.notEqual(next.apps[0].id, page.apps[0].id);
    assert.equal((await http("http-no-previous-revision", path + "/restore", "POST", { revision: 1 })).value.error, "no_previous_revision");
    assert.deepEqual((await http("http-initial-data", path + "/data")).value, { value: null, revision: 0, updated_at: null });
    const racing = await Promise.all([1, 2].map(count => http("http-state-race-" + count, path + "/data", "PUT", { value: { count }, revision: 0 })));
    assert.deepEqual(racing.map(result => result.status).sort(), [200, 409]);
    assert.equal((await http("http-state-update", path + "/data", "PUT", { value: { count: 3 }, revision: 1 })).value.revision, 2);
    const revised = { ...document, title: "Revised counter", source: source.replace("Cigarettes:", "Logged:") };
    const updated = await http("http-source-update", path, "PUT", { ...revised, revision: 1 });
    assert.equal(updated.status, 200);
    assert.equal(updated.value.revision, 2);
    assert.equal(updated.value.validation.valid, true);
    assert.equal((await http("http-source-conflict", path, "PUT", { ...document, revision: 1 })).value.error, "revision_conflict");
    assert.equal((await http("http-delete-conflict", path + "?revision=1", "DELETE")).status, 409);
    const storedHTTP = await snapshot();
    assert.equal((await http("http-invalid-edit", path, "PUT", { ...document, revision: 2, source: invalid })).status, 422);
    assert.deepEqual(await snapshot(), storedHTTP);
    const restored = await http("http-restore", path + "/restore", "POST", { revision: 2 });
    assert.equal(restored.value.source, source);
    assert.equal(restored.value.revision, 3);
    assert.equal(restored.value.validation.valid, true);
    assert.deepEqual((await http("http-data-after-restore", path + "/data")).value.value, { count: 3 });
    assert.equal((await http("http-restore-conflict", path + "/restore", "POST", { revision: 2 })).status, 409);
    const swapped = await http("http-restore-swap", path + "/restore", "POST", { revision: 3 });
    assert.equal(swapped.value.source, revised.source);
    assert.equal(swapped.value.revision, 4);
    for (const [name, input, expected] of [
      ["oversized-source", { ...document, source: "🪴".repeat(65537) }, "invalid_input"],
      ["unknown-field", { ...document, owner_id: "other" }, "invalid_input"],
      ["chosen-id", { ...document, id: "chosen" }, "invalid_input"],
      ["legacy-html", { title: "Legacy", html: "<h1>Legacy</h1>" }, "invalid_input"],
      ["mixed-html", { ...document, html: "<h1>Mixed</h1>" }, "invalid_input"],
      ...[undefined, null, "html-v1", "javascript-v1", "swift-v2", "Swift-v1"].map(runtime => ["runtime-" + runtime, { ...document, runtime }, "unsupported_runtime"]),
    ]) assert.equal((await http("http-" + name, "", "POST", input)).value.error, expected);
    assert.equal((await http("http-duplicate-query", "?limit=1&limit=2")).status, 400);
    assert.equal((await http("http-limit-too-large", "?limit=101")).status, 400);
    assert.equal((await http("http-update-no-revision", path, "PUT", document)).status, 400);
    assert.equal((await http("http-delete-no-revision", path, "DELETE")).status, 400);
    assert.equal((await http("http-data-no-value", path + "/data", "PUT", { revision: 2 })).status, 400);
    assert.equal((await http("http-data-too-large", path + "/data", "PUT", { value: "x".repeat(262144), revision: 2 })).status, 413);
    assert.equal((await http("http-data-negative-revision", path + "/data", "PUT", { value: null, revision: -1 })).status, 400);
    assert.equal((await http("http-delete", path, "DELETE", { revision: 4 })).value.deleted, true);
    assert.equal((await http("http-get-deleted", path)).status, 404);
    assert.equal((await http("http-data-deleted", path + "/data")).status, 404);
    assert.equal((await http("http-cannot-revive-data", path + "/data", "PUT", { value: "revive", revision: 2 })).status, 404);
    assert.deepEqual(await snapshot(), before, "HTTP journey leaves only the original tool app");

    // Exercise production discovery, canonical machineTool routing and receipt
    // unwrapping through a real account DO and WebSocket publisher. The fixture
    // publisher executes Swift rather than manufacturing a validation result.
    const handURL = new URL("/__fixture/hand", base);
    handURL.protocol = "ws:";
    handSocket = new WebSocket(handURL, { headers: { authorization: "Bearer owner" } });
    let readyResolve, readyReject;
    const handReady = new Promise((resolve, reject) => { readyResolve = resolve; readyReject = reject; });
    const readyTimer = setTimeout(() => readyReject(new Error("Hand catalog was not admitted")), 10_000);
    handSocket.on("message", async data => {
      const frame = JSON.parse(data.toString());
      handFrames.push(frame);
      if (frame.type === "ready") { clearTimeout(readyTimer); readyResolve(); }
      if (frame.type !== "call") return;
      try {
        assert.equal(frame.name, "validate_app");
        const result = await nativeValidate(frame.input);
        handSocket.send(JSON.stringify({ type: "result", call_id: frame.call_id, outcome: {
          status: "completed", output: { output: JSON.stringify(result), success: true, structured_result: result, metadata: null, process_trace: null },
        } }));
      } catch (error) {
        handSocket.send(JSON.stringify({ type: "result", call_id: frame.call_id, outcome: {
          status: "completed", output: { output: String(error), success: false, structured_result: null, metadata: null, process_trace: null },
        } }));
      }
    });
    await once(handSocket, "open");
    const machine = "native-validation-fixture";
    handSocket.send(JSON.stringify({
      type: "catalog", capabilities: ["turn_metadata"], attachment_id: machine,
      machines: [{ id: machine, name: "Synthetic Swift validation Hand", workspace: "/workspace", capabilities: ["native", "background_limited"] }],
      tools: [{ provider: "native", remote_name: "validate_app", parallel_safe: false, timeout_ms: 15_000,
        definition: { type: "function", name: "validate_app", description: "Runs the real swift-v1 parser and interpreter in isolation.", strict: false,
          parameters: { type: "object", additionalProperties: false, required: ["runtime", "source"], properties: {
            runtime: { type: "string", enum: ["swift-v1"] }, source: { type: "string" }, state: { type: "object" }, steps: { type: "array" }, agent_response: { type: "string" },
          } },
        },
      }],
    }));
    await handReady;
    const native = await call("native-hand-log-undo-reopen", { operation: "validate", runtime: "swift-v1", source, steps }, { "x-fixture-validator": "hand" });
    assert.equal(native.status, 200);
    assert.equal(native.value.valid, true, JSON.stringify(native));
    assert.equal(native.value.validator_machine, machine);
    assert.equal(native.value.source_sha256, sha256(source));
    assert.equal(native.value.persisted_test_state.count, 1);
    const nativeInvalid = await call("native-hand-reject-save", { operation: "save", ...document, source: invalid }, { "x-fixture-validator": "hand" });
    assert.equal(nativeInvalid.value.error, "app_validation_failed");
    assert.equal(nativeInvalid.value.validation.validator_machine, machine);
    assert.equal(handFrames.filter(frame => frame.type === "call").length, 2);
    assert.deepEqual(await snapshot(), before);
    const closed = once(handSocket, "close");
    handSocket.close(1000, "validation fixture complete");
    await closed;
    const nativeOffline = await call("native-hand-offline-save", { operation: "save", ...document }, { "x-fixture-validator": "hand" });
    assert.equal(nativeOffline.value.error, "app_validation_unavailable");
    assert.deepEqual(await snapshot(), before);

    available = false;
    for (const [scenario, input] of [
      ["validator-offline-validate", { operation: "validate", runtime: "swift-v1", source }],
      ["validator-offline-create", { operation: "save", ...document }],
      ["validator-offline-edit", { operation: "save", ...document, id, revision: 1 }],
    ]) {
      const offline = await call(scenario, input);
      assert.equal(offline.value.error, "app_validation_unavailable", JSON.stringify(offline));
      assert.match(JSON.stringify(offline.value), /validation_unavailable|validator_unavailable/i);
      assert.deepEqual(await snapshot(), before, `${scenario} must fail before any D1 mutation`);
    }
    for (const [name, path, method, body] of [
      ["create", "", "POST", document],
      ["validate", "/validate", "POST", { runtime: "swift-v1", source }],
      ["edit", "/" + id, "PUT", { ...document, revision: 1 }],
    ]) {
      const offline = await http("http-validator-offline-" + name, path, method, body);
      assert.equal(offline.status, 503);
      assert.equal(offline.value.error, "app_validation_unavailable");
      assert.deepEqual(await snapshot(), before);
    }
    await writeFile(resolve(output, "database.json"), JSON.stringify(await snapshot(), null, 2));
    console.log(`PASS cloud apps -> real Swift parser/runtime: rejection, log/undo/reopen, isolated validation, invalid edit preservation and offline fail-closed. Evidence: ${output}`);
  } finally {
    await writeFile(resolve(output, "trace.json"), JSON.stringify({ binary, validatorURL, trace, validatorCalls, handFrames }, null, 2));
    handSocket?.terminate();
    await mf?.dispose();
    await new Promise((done, reject) => bridge.close(error => error ? reject(error) : done()));
  }
});
