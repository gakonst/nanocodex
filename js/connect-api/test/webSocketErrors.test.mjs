import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";

const execFileAsync = promisify(execFile);
const grantId = `0x${"0".repeat(64)}`;

test("WebSocket routes project rejected async handlers as typed API errors", async (t) => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-connect-api-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));

  await execFileAsync(
    process.platform === "win32" ? "npx.cmd" : "npx",
    [
      "wrangler",
      "deploy",
      "--dry-run",
      "--config",
      "./wrangler.jsonc",
      "--outdir",
      outdir,
    ],
    { cwd: new URL("..", import.meta.url) },
  );

  const worker = (await import(new URL(`file://${path.join(outdir, "index.js")}`))).default;
  const env = {
    CONNECT_STATE: {
      idFromName: (name) => name,
      get: () => ({ fetch: async () => Response.json({ value: undefined }) }),
    },
  };
  const context = { waitUntil() {} };
  const routes = [
    `/v1/grants/${grantId}/model?app_id=atlas-workspace`,
    `/v1/grants/${grantId}/agents/agent/realtime/sideband`,
    `/v1/grants/${grantId}/agents/agent/tool-host`,
  ];

  for (const route of routes) {
    const response = await worker.fetch(new Request(`https://nanocodex.gakonst.workers.dev${route}`, {
      headers: { origin: "https://nanocodex-connect-playground.gakonst.workers.dev" },
    }), env, context);
    assert.equal(response.status, 426, route);
    assert.deepEqual(await response.json(), {
      error: {
        code: "websocket_required",
        message: route.includes("realtime")
          ? "The voice sideband requires a WebSocket upgrade."
          : route.includes("tool-host")
            ? "The tool host requires a WebSocket upgrade."
            : "The model endpoint requires a WebSocket upgrade.",
      },
    });
  }
});
