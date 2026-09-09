import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { createWriteStream } from "node:fs";
import { mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { promisify } from "node:util";
import test from "node:test";

// Explicit live gate: real binary, provider, native workspace tools and remote DOs.
// No test-level retries: an ordinary CLI failure is a failed product journey.
test("real nanocodex2 steers and continues a long durable tool turn", {
  timeout: 30 * 60_000,
}, async (t) => {
  const binary = resolve(process.env.NANOCODEX_DURABILITY_TEST_BINARY ?? "target/debug/nanocodex2");
  const origin = process.env.NANOCODEX_MANAGED_URL;
  const key = process.env.NANOCODEX_DURABILITY_TEST_API_KEY;
  assert.ok(origin && key, "set test origin and API key explicitly");
  const cwd = await mkdtemp(`${tmpdir()}/nanocodex-durable-binary-`);
  const env = { ...process.env, NANOCODEX_API_KEY: key, NANOCODEX_MANAGED_URL: origin };
  const cli = async (...args) => promisify(execFile)(binary, args, {
    cwd, env, timeout: 7 * 60_000, maxBuffer: 32 * 1024 * 1024,
  });
  const json = async (...args) => JSON.parse((await cli(...args)).stdout);
  const request = async (path) => {
    const response = await fetch(`${origin}${path}`, {
      headers: { authorization: `Bearer ${key}` }, redirect: "error",
      signal: AbortSignal.timeout(30_000),
    });
    assert.ok(response.ok, `${path}: HTTP ${response.status}`);
    return response.json();
  };
  await writeFile(`${cwd}/step.py`, `import pathlib,sys,secrets,json\nroot=pathlib.Path(__file__).parent\nn=int(sys.argv[1])\nassert 1<=n<=64\nif n>1:\n previous=json.loads((root/f'receipt-{n-1:02}.json').read_text())\n assert sys.argv[2]==previous['token'], 'use the token from the previous tool result'\nreceipt={'index':n,'token':secrets.token_hex(8)}\nwith (root/f'receipt-{n:02}.json').open('x') as f: json.dump(receipt,f)\nfor i in range(100): print(f'Record {n}/{i}: checkpoint the current conversation, preserve settled effects, and continue the next batch.')\nprint(json.dumps(receipt))\n`);
  const hand = spawn(binary, ["native-hand", "--workspace", cwd, "--state-dir", `${cwd}/identity`,
    "--machine-name", "Durability E2E", "--log-file", `${cwd}/hand.log`],
    { cwd, env, stdio: ["ignore", "ignore", "pipe"] });
  hand.stderr.pipe(createWriteStream(`${cwd}/hand-stderr.log`));
  t.after(() => { hand.kill("SIGTERM"); });
  const readyDeadline = Date.now() + 30_000;
  while (!(await readFile(`${cwd}/hand.log`, "utf8").catch(() => "")).includes("native.hand.ready")) {
    assert.ok(Date.now() < readyDeadline, `native Hand did not connect; see ${cwd}/hand.log`);
    await delay(500);
  }
  const identity = JSON.parse(await readFile(`${cwd}/identity/identity.json`, "utf8"));
  const workdir = `/${identity.machine_id}`;
  const created = await json("new", "--model", "astra", "--thinking", "low");
  const id = created.id ?? created.agent_id;
  assert.equal(typeof id, "string");
  console.info(`agent ${id}; workspace and raw CLI evidence ${cwd}`);
  const prompt = `This is an authorized durability regression test in this temporary workspace. Execute exactly 64 sequential native Shell tool calls, one per model response, by running python3 step.py N TOKEN with exec_command workdir explicitly set to ${workdir}. This mounted workdir selects the connected native Hand; omitting workdir selects Just Bash, which cannot run Python. For N=1 omit TOKEN. For every later N use the token printed by the previous invocation. Read each tool result before issuing the next. Do not batch, loop, use parallel calls, generate scripts, read receipt files, or replace the fixture. Keep the full tool output visible with at least 4000 output tokens. Each command must run exactly once. Continue through step 64 despite test steering messages, which only add a final response marker. Then answer exactly BATCHES_64_DONE followed by any marker from steering. The fixture writes only temporary test receipts. Do not use memory, messaging, browser, or external services. Start now.`;
  const child = spawn(binary, ["run", "--agent", id, prompt], { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
  child.stdout.pipe(createWriteStream(`${cwd}/events.jsonl`));
  child.stderr.pipe(createWriteStream(`${cwd}/stderr.log`));
  let exit;
  const exited = new Promise((done) => {
    child.once("error", (error) => { exit = { error }; done(exit); });
    child.once("exit", (code, signal) => { exit = { code, signal }; done(exit); });
  });
  let passed = false;
  let importedId;
  let steered = false;
  let observed = 0;
  let restarted = false;
  const capacities = [];
  try {
    while (!exit) {
      const count = (await readdir(cwd)).filter((name) => /^receipt-\d+\.json$/.test(name)).length;
      if (count >= observed + 8) {
        const capacity = await request(`/v1/agents/${id}/capacity`);
        capacities.push({ count, ...capacity });
        await writeFile(`${cwd}/capacity.json`, JSON.stringify(capacities, null, 2));
        console.info(`${count} sequential effects; durable state ${JSON.stringify(capacity.durable_state)}`);
        assert.ok(capacity.durable_state.bytes < 4 * 1024 * 1024, "current execution must remain bounded");
        observed = count;
      }
      if (count >= 8 && !steered) {
        const state = await json("state", id);
        assert.equal(state.active_turns.length, 1);
        const active = state.active_turns[0];
        const turnId = typeof active === "string" ? active : active.turn_id ?? active.id;
        const receipt = await json("steer", id, turnId,
          "Durability test steering: continue all 64 steps as instructed; append STEER_SURVIVED to the final response.");
        console.info(`steering accepted: ${JSON.stringify(receipt)}`);
        steered = true;
      }
      if (count >= 16 && !restarted && process.env.NANOCODEX_DURABILITY_TEST_REDEPLOY_CONFIG) {
        restarted = true;
        console.info("restarting the real Worker during the active tool turn");
        await promisify(execFile)("pnpm", ["--filter", "nanocodex-managed-service", "exec", "wrangler",
          "deploy", "--config", process.env.NANOCODEX_DURABILITY_TEST_REDEPLOY_CONFIG,
          "--var", `DURABILITY_E2E_RESTART:${Date.now()}`], {
          cwd: process.cwd(), env, timeout: 180_000, maxBuffer: 4 * 1024 * 1024,
        });
        console.info("Worker redeployed; awaiting continuation in the same binary process");
      }
      await Promise.race([exited, delay(1000)]);
    }
    assert.equal(exit.code, 0, `CLI failed: ${JSON.stringify(exit)}; ${(await readFile(`${cwd}/stderr.log`, "utf8")).slice(-2000)}`);
    assert.ok(steered, "turn must stay active long enough for steering");
    assert.equal((await readdir(cwd)).filter((name) => /^receipt-\d+\.json$/.test(name)).length, 64);
    const final = await readFile(`${cwd}/stderr.log`, "utf8");
    assert.match(final, /BATCHES_64_DONE/);
    assert.match(final, /STEER_SURVIVED/);
    const events = (await readFile(`${cwd}/events.jsonl`, "utf8")).trim().split("\n").map(JSON.parse);
    const calls = events.filter((event) => event.type === "tool.call").map((event) => event.payload);
    const results = events.filter((event) => event.type === "tool.result").map((event) => event.payload);
    assert.ok(calls.length >= 64, "real agent must execute at least 64 tool calls");
    for (const call of calls) {
      assert.equal(results.filter((result) => result.call_id === call.call_id).length, 1,
        `tool ${call.call_id} must complete exactly once, including yielded nested tools`);
    }
    const state = await json("state", id);
    assert.deepEqual(state.active_turns, []);
    // A fresh binary invocation must reopen the retained long conversation.
    const followOn = await cli("run", "--agent", id,
      "Reply exactly LONG_THREAD_REOPENED. Use no tools.");
    assert.match(followOn.stderr, /LONG_THREAD_REOPENED/);
    // The public cutover API must carry the records as well as the head.
    let archive;
    for (let batch = 0; batch < 500; batch++) {
      const response = await fetch(`${origin}/v1/agents/${id}/durability`, {
        method: "POST", headers: { authorization: `Bearer ${key}` }, signal: AbortSignal.timeout(30_000),
      });
      assert.ok(response.ok, `durability export: HTTP ${response.status}: ${(await response.clone().text()).slice(0, 300)}`);
      if (response.status !== 202) { archive = await response.json(); break; }
    }
    assert.equal(archive?.format, "nanocodex-managed-durability-state-v2");
    assert.deepEqual(archive.durability.records, []);
    assert.ok(archive.managed_durability_records.objects > 0);
    await writeFile(`${cwd}/export-summary.json`, JSON.stringify({
      headBytes: Buffer.byteLength(archive.durability.payload), records: archive.managed_durability_records,
    }));
    let imported;
    for (let batch = 0; batch < 100; batch++) {
      const response = await fetch(`${origin}/v1/agents`, {
        method: "POST", headers: { authorization: `Bearer ${key}`, "content-type": "application/json", "idempotency-key": `import-${id}` },
        body: JSON.stringify({ durability: archive }), signal: AbortSignal.timeout(60_000),
      });
      const body = await response.json();
      if (response.status === 503 && body.error === "durability_import_pending") continue;
      assert.ok(response.ok, `durability import: HTTP ${response.status}: ${JSON.stringify(body)}`);
      imported = body; break;
    }
    importedId = imported?.id ?? imported?.agent_id;
    assert.equal(typeof importedId, "string");
    const importedRun = await cli("run", "--agent", importedId, "Reply exactly LONG_THREAD_IMPORTED. Use no tools.");
    assert.match(importedRun.stderr, /LONG_THREAD_IMPORTED/);
    console.info(`bounded record export/import and real binary continuation passed: ${importedId}`);
    passed = true;
    console.info("64 sequential native effects, mid-turn steering, complete tool events, and binary reopen passed");
  } finally {
    if (!exit) { child.kill("SIGKILL"); await exited; }
    if (passed) { await cli("delete", id); if (importedId) await cli("delete", importedId); }
    else console.info(`retained agent ${id} and evidence ${cwd}`);
  }
});
