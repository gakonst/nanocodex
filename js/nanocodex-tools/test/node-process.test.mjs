import assert from "node:assert/strict";
import { test } from "node:test";
import { access, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { setTimeout as delay } from "node:timers/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createNodeProcessTools } from "../tools/nodeProcess.mjs";

test("native pipe sessions preserve ownership and never inherit provider credentials", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "nanocodex-process-"));
  const original = process.env.NC_API_KEY;
  process.env.NC_API_KEY = "secret-sentinel";
  const runtime = await createNodeProcessTools({ workspace });
  const [exec, stdin] = runtime.tools;
  const context = {
    sessionId: "owner-a",
    signal: new AbortController().signal,
  };
  try {
    const result = await exec.handler(
      {
        cmd: 'printf "key=%s\\n" "$NC_API_KEY"; read value; printf "result=%s\\n" "$value"',
        yield_time_ms: 10,
      },
      context,
    );
    assert.equal(typeof result.session_id, "number");
    assert.doesNotMatch(result.output, /secret-sentinel/);
    await assert.rejects(
      stdin.handler(
        { session_id: result.session_id, chars: "wrong\n" },
        { ...context, sessionId: "owner-b" },
      ),
      /unavailable/,
    );
    const completed = await stdin.handler(
      {
        session_id: result.session_id,
        chars: "correct\n",
        yield_time_ms: 1000,
      },
      context,
    );
    assert.equal(completed.exit_code, 0);
    assert.match(completed.output, /result=correct/);
    await assert.rejects(
      exec.handler({ cmd: "pwd", workdir: "/" }, context),
      /outside/,
    );
    await assert.rejects(
      exec.handler({ cmd: "pwd", tty: true }, context),
      /PTY/,
    );
  } finally {
    if (original === undefined) delete process.env.NC_API_KEY;
    else process.env.NC_API_KEY = original;
    await runtime.close();
    await rm(workspace, { recursive: true });
  }
});

test(
  "a stopped Hand also reaps descendants of an already completed shell",
  { skip: process.platform === "win32" },
  async () => {
    const workspace = await mkdtemp(join(tmpdir(), "nanocodex-descendants-"));
    const runtime = await createNodeProcessTools({ workspace });
    let pid;
    try {
      const result = await runtime.tools[0].handler(
        { cmd: 'sleep 60 </dev/null >/dev/null 2>&1 & printf "%s" "$!"' },
        { sessionId: "owner" },
      );
      assert.equal(result.exit_code, 0);
      pid = Number(result.output);
      assert.ok(pid > 0);
      process.kill(pid, 0);
      await runtime.close();
      let alive = true;
      for (let attempt = 0; attempt < 30; attempt++) {
        try {
          process.kill(pid, 0);
          await delay(100);
        } catch (error) {
          assert.equal(error.code, "ESRCH");
          alive = false;
          break;
        }
      }
      assert.equal(alive, false, "a background process outlived its Hand");
    } finally {
      if (pid) {
        try {
          process.kill(pid, "SIGKILL");
        } catch {}
      }
      await runtime.close();
      await rm(workspace, { recursive: true });
    }
  },
);

test("invalid execution limits never start a command", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "nanocodex-invalid-"));
  const runtime = await createNodeProcessTools({ workspace });
  try {
    await assert.rejects(
      runtime.tools[0].handler(
        { cmd: "touch should-not-exist", max_output_tokens: -1 },
        { sessionId: "owner" },
      ),
      /integer/,
    );
    await assert.rejects(access(join(workspace, "should-not-exist")), {
      code: "ENOENT",
    });
    const invalidShell = await runtime.tools[0].handler(
      { cmd: "echo ignored", shell: "/does-not-exist" },
      { sessionId: "owner" },
    );
    assert.equal(invalidShell.exit_code, 1);
  } finally {
    await runtime.close();
    await rm(workspace, { recursive: true });
  }
});

test("stopping a Hand terminates retained processes and fences new commands", async () => {
  const workspace = await mkdtemp(join(tmpdir(), "nanocodex-stop-"));
  const runtime = await createNodeProcessTools({ workspace });
  const context = { sessionId: "owner", signal: new AbortController().signal };
  const result = await runtime.tools[0].handler(
    { cmd: "sleep 60", yield_time_ms: 0 },
    context,
  );
  assert.equal(typeof result.session_id, "number");
  await runtime.close();
  await assert.rejects(
    runtime.tools[1].handler({ session_id: result.session_id }, context),
    /unavailable/,
  );
  await assert.rejects(
    runtime.tools[0].handler({ cmd: "echo forbidden" }, context),
    /stopped/,
  );
  await rm(workspace, { recursive: true });
});

test("native Hands retain more than 32 parallel processes until completion", { skip: process.platform === "win32" }, async () => {
  const workspace = await mkdtemp(join(tmpdir(), "nanocodex-capacity-"));
  const runtime = await createNodeProcessTools({ workspace });
  const [exec, stdin] = runtime.tools;
  const context = { sessionId: "owner" };
  const sessions = [];
  try {
    for (let batch = 0; batch < 2; batch++) {
      const results = await Promise.allSettled(Array.from({ length: 40 }, () => exec.handler(
        { cmd: 'read value; printf "%s\\n" "$value"', yield_time_ms: 0 }, context,
      )));
      for (const result of results) {
        assert.equal(result.status, "fulfilled", result.reason?.message);
        assert.equal(typeof result.value.session_id, "number");
        sessions.push(result.value.session_id);
      }
    }
    assert.equal(new Set(sessions).size, 80);
    const completed = await Promise.all(sessions.map((session_id, index) => stdin.handler(
      { session_id, chars: `process-${index}\n`, yield_time_ms: 1000 }, context,
    )));
    for (const [index, result] of completed.entries()) {
      assert.equal(result.exit_code, 0);
      assert.equal(result.output, `process-${index}\n`);
      assert.equal(result.session_id, undefined);
    }
  } finally { await runtime.close(); await rm(workspace, { recursive: true }); }
});

test("large unread output survives completion and drains without truncation or broken UTF-8", { timeout: 10_000 }, async () => {
  const workspace = await mkdtemp(join(tmpdir(), "nanocodex-output-test-"));
  let finished;
  const completed = new Promise(resolve => { finished = resolve; });
  const runtime = await createNodeProcessTools({ workspace, onActivity(event) { if (event.type === "completed") finished(); } });
  const expected = "a😀β\n".repeat(400_000) + "final-output\n";
  await writeFile(join(workspace, "large.txt"), expected);
  try {
    const context = { sessionId: "owner" };
    let result = await runtime.tools[0].handler({ cmd: "cat large.txt", yield_time_ms: 0, max_output_tokens: 7 }, context);
    let output = result.output;
    await completed;
    while (result.session_id !== undefined) {
      result = await runtime.tools[1].handler({ session_id: result.session_id, yield_time_ms: 0, max_output_tokens: 32_000 }, context);
      output += result.output;
    }
    assert.equal(result.exit_code, 0);
    assert.equal(output, expected);
  } finally { await runtime.close(); await rm(workspace, { recursive: true }); }
});

test("Finder PATH discovers installed Node while preserving inherited executable priority", { skip: process.platform !== "darwin" }, async t => {
  const installations = await Promise.all(["/opt/homebrew/bin/node", "/usr/local/bin/node"].map(path => access(path).then(() => path, () => undefined)));
  if (!installations.some(Boolean)) { t.skip("No Homebrew or /usr/local Node installation is present."); return; }
  const workspace = await mkdtemp(join(tmpdir(), "nanocodex-path-"));
  const originalPath = process.env.PATH;
  const runtimes = [];
  try {
    process.env.PATH = "/usr/bin:/bin:/usr/sbin:/sbin";
    const finder = await createNodeProcessTools({ workspace }); runtimes.push(finder);
    const version = await finder.tools[0].handler({ cmd: "node --version" }, { sessionId: "finder" });
    assert.equal(version.exit_code, 0);
    assert.match(version.output, /^v\d+\.\d+\.\d+/);
    const preferred = join(workspace, "bin"); await mkdir(preferred);
    await writeFile(join(preferred, "node"), "#!/bin/sh\nprintf 'preferred-node\\n'\n", { mode: 0o700 });
    process.env.PATH = `${preferred}:/usr/bin:/bin`;
    const explicit = await createNodeProcessTools({ workspace }); runtimes.push(explicit);
    const chosen = await explicit.tools[0].handler({ cmd: "node --version" }, { sessionId: "explicit" });
    assert.equal(chosen.output.trim(), "preferred-node");
  } finally {
    if (originalPath === undefined) delete process.env.PATH; else process.env.PATH = originalPath;
    await Promise.all(runtimes.map(runtime => runtime.close()));
    await rm(workspace, { recursive: true });
  }
});
