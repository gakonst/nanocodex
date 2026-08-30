import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { withWebMcp } from "../next/index.mjs";
import { nanocodex } from "../vite/index.mjs";
import { createProvider, publish } from "../webmcp/WebMcp.mjs";

test("Vite and Next.js generated tools reach authenticated website endpoints through WebMCP", async () => {
  const root = await mkdtemp(join(tmpdir(), "nanocodex-framework-webmcp-"));
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, "http://localhost");
    if (request.headers.cookie !== "session=website-user") {
      response.writeHead(401, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: "unauthorized" }));
      return;
    }
    if (request.method === "GET" && url.pathname === "/api/balance") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ balance: "125.50", currency: url.searchParams.get("currency") }));
      return;
    }
    const transfer = url.pathname.match(/^\/api\/transfers\/([^/]+)$/);
    if (request.method === "POST" && transfer) {
      const body = JSON.parse(await readBody(request));
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ id: decodeURIComponent(transfer[1]), status: "sent", ...body }));
      return;
    }
    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: "not_found" }));
  });
  await listen(server);
  const origin = `http://127.0.0.1:${server.address().port}`;

  try {
    const viteRoot = join(root, "vite");
    await mkdir(viteRoot, { recursive: true });
    await writeFile(join(viteRoot, "account.ts"), `
      export const loadBalance = () => fetch("/api/balance");
    `);
    const vitePlugin = nanocodex({ chatGpt: false, webMcp: true });
    vitePlugin.configResolved({ root: viteRoot, logger: { info() {} } });
    await vitePlugin.buildStart();
    await vitePlugin.closeBundle();

    const nextRoot = join(root, "next");
    const routeRoot = join(nextRoot, "app", "api", "transfers", "[id]");
    await mkdir(routeRoot, { recursive: true });
    await writeFile(join(routeRoot, "route.ts"), `
      export async function POST(request: Request) {
        return Response.json(await request.json());
      }
    `);
    await withWebMcp({}, { root: nextRoot })("phase-production-build", {});

    const viteManifest = approved(await manifest(join(viteRoot, "webmcp.manifest.json")));
    const nextManifest = approved(await manifest(join(nextRoot, "webmcp.manifest.json")));
    assert.deepEqual(viteManifest.tools.map(({ name }) => name), ["get_api_balance"]);
    assert.deepEqual(nextManifest.tools.map(({ name }) => name), ["post_api_transfers_id"]);

    const registry = modelContext(origin);
    const document = {
      location: { href: `${origin}/dashboard`, origin },
      modelContext: registry,
      title: "Example Bank",
    };
    const sessionFetch = (input, options = {}) => {
      assert.equal(options.credentials, "same-origin");
      const headers = new Headers(options.headers);
      headers.set("cookie", "session=website-user");
      return fetch(input, { ...options, headers });
    };
    const publications = await Promise.all([
      publish(viteManifest, { baseUrl: origin, document, fetch: sessionFetch }),
      publish(nextManifest, { baseUrl: origin, document, fetch: sessionFetch }),
    ]);
    const approvals = [];
    const provider = await createProvider({
      confirm(action) {
        approvals.push(action);
        return true;
      },
      document,
      fallback: "never",
    });

    try {
      assert.deepEqual(provider.definitions().map(({ name }) => name), [
        "web_get_api_balance",
        "web_post_api_transfers_id",
      ]);
      const balance = await provider.resolve("web_get_api_balance").handler({
        query: { currency: "USD" },
      });
      const transfer = await provider.resolve("web_post_api_transfers_id").handler({
        path: { id: "tx-7" },
        body: { amount: "25.00", recipient: "Ada" },
      });

      assert.deepEqual(balance, { balance: "125.50", currency: "USD" });
      assert.deepEqual(transfer, {
        amount: "25.00",
        id: "tx-7",
        recipient: "Ada",
        status: "sent",
      });
      assert.deepEqual(approvals.map(({ name }) => name), ["post_api_transfers_id"]);
    } finally {
      provider.close();
      for (const publication of publications) publication.close();
    }
  } finally {
    await close(server);
    await rm(root, { recursive: true, force: true });
  }
});

function approved(value) {
  return { ...value, tools: value.tools.map((tool) => ({ ...tool, approved: true })) };
}

async function manifest(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

function modelContext(origin) {
  const tools = new Map();
  return {
    async registerTool(tool, options) {
      tools.set(tool.name, tool);
      options.signal.addEventListener("abort", () => tools.delete(tool.name), { once: true });
    },
    async getTools() {
      return [...tools.values()].map((tool) => ({ ...tool, origin }));
    },
    async executeTool(tool, input, options) {
      return tools.get(tool.name).execute(JSON.parse(input), options);
    },
  };
}

function readBody(request) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    request.on("data", (chunk) => chunks.push(chunk));
    request.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    request.on("error", reject);
  });
}

function listen(server) {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
}

function close(server) {
  return new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
}
