import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { WebSocketServer } from "ws";

const binary = process.env.NANOCODEX_DEVICE_TEST_BINARY;
// These native journeys use a synthetic account endpoint and private HOME.
// Linux exercises the shared managed-provider discovery without any macOS
// downloader, LaunchAgent installation, or real desktop permissions.
for (const scenario of ["missing", "managed-failure", "managed-slow", "explicit-failure"]) {
  test(`real native Hand remains usable with ${scenario} CUA`, {
    skip: !binary || process.platform !== "linux", timeout: 20_000,
  }, async t => {
    const home = await mkdtemp(join(tmpdir(), "ncx-hand-cua-"));
    const workspace = join(home, "workspace");
    await mkdir(workspace);
    const server = createServer((_request, response) => { response.writeHead(404); response.end(); });
    const sockets = new WebSocketServer({ noServer: true });
    let catalogs = 0, resolveResult;
    const result = new Promise(resolve => { resolveResult = resolve; });
    server.on("upgrade", (request, socket, head) => {
      if (request.url !== "/v1/account/tool-host") { socket.destroy(); return; }
      sockets.handleUpgrade(request, socket, head, ws => sockets.emit("connection", ws));
    });
    sockets.on("connection", socket => socket.on("message", data => {
      const frame = JSON.parse(String(data));
      if (frame.type === "catalog") {
        catalogs++;
        socket.send('{"type":"ready"}');
        socket.send(JSON.stringify({ type: "call", session_id: "synthetic-cua", call_id: "shell", model: "gpt-6-astra", name: "exec_command",
          input: { cmd: "printf native_hand_ready" }, output_token_budget: 1024, output_byte_budget: 131072, deadline_at: Date.now() + 10_000 }));
      }
      if (frame.type === "result") {
        socket.send(JSON.stringify({ type: "ack", call_id: frame.call_id }));
        resolveResult(frame);
      }
      if (frame.type === "drain") socket.send('{"type":"draining"}');
    }));
    server.listen(0, "127.0.0.1"); await once(server, "listening");
    const provider = join(home, "failed-provider");
    const checkManagedConfig = scenario.startsWith("managed-")
      ? '[ "$1" = "fixture-arg" ] && [ "$HAND_CUA_SENTINEL" = "fixture-env" ] || exit 8\n' : "";
    await writeFile(provider, "#!/bin/sh\n" + checkManagedConfig + 'printf invoked > "$HOME/provider-invoked"\n' + (scenario === "managed-slow" ? "exec sleep 60\n" : "exit 7\n"), { mode: 0o700 });
    const env = { PATH: process.env.PATH, HOME: home, USERPROFILE: home, NANOCODEX_DIR: home,
      NANOCODEX_API_KEY: `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`,
      NANOCODEX_MANAGED_URL: `http://127.0.0.1:${server.address().port}` };
    if (scenario.startsWith("managed-")) {
      await mkdir(join(home, "runtimes/openai-cua"), { recursive: true });
      await writeFile(join(home, "runtimes/openai-cua/provider.json"), JSON.stringify({
        status: "installed", executable: provider, transport: "mcp", args: ["fixture-arg"], environment: { HAND_CUA_SENTINEL: "fixture-env" },
        dependency_contract: "nanocodex-native-no-codex-v1",
      }));
    }
    if (scenario === "explicit-failure") env.NANOCODEX_COMPUTER = provider;
    const started = Date.now();
    const child = spawn(binary, ["hand", "--workspace", workspace, "--state-dir", join(home, "state")], { env, stdio: ["ignore", "ignore", "pipe"] });
    const exited = once(child, "exit");
    let diagnostics = "";
    child.stderr.on("data", data => { diagnostics = (diagnostics + data).slice(-8192); });
    const timeout = setTimeout(() => child.kill("SIGKILL"), 15_000);
    t.after(async () => {
      clearTimeout(timeout);
      if (child.exitCode === null && child.signalCode === null) child.kill("SIGINT");
      await exited;
      for (const socket of sockets.clients) socket.terminate();
      sockets.close(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
      await rm(home, { recursive: true, force: true });
    });
    if (scenario === "explicit-failure") {
      assert.deepEqual(await exited, [1, null], diagnostics);
      assert.equal(catalogs, 0);
    } else {
      const frame = await Promise.race([result, exited.then(exit => { throw new Error(`Hand exited ${exit}: ${diagnostics}`); })]);
      assert.equal(frame.outcome.status, "completed", JSON.stringify(frame));
      assert.match(frame.outcome.output.output, /native_hand_ready/);
      assert.equal(catalogs, 1);
      if (scenario === "managed-slow") assert(Date.now() - started < 5000, "Optional provider startup must not hold shell access for its 120-second deadline");
    }
    if (scenario !== "missing") assert.equal(await readFile(join(home, "provider-invoked"), "utf8"), "invoked");
  });
}
