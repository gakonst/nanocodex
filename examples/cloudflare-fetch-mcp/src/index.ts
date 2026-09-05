import { DurableObject } from "cloudflare:workers";
import asyncVariant from "@jitl/quickjs-wasmfile-release-asyncify";
import { createJsonChannelStore, tempo } from "mppx/client";
import { Agent, createQuickJsEvaluator, createTempoProvider, Transport } from "nanocodex/host";
import {
  newQuickJSAsyncWASMModuleFromVariant,
  newVariant,
} from "quickjs-emscripten-core";
import { createClient, formatUnits, http } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { tempo as tempoChain } from "viem/chains";

import nanocodexWasm from "./nanocodex.wasm";
import quickJsWasm from "./quickjs.wasm";
import { RequestError, authorized, parseFetchPrompt } from "./request";

const PATH_USD = "0x20c0000000000000000000000000000000000000" as const;
const quickJsEvaluator = newQuickJSAsyncWASMModuleFromVariant(
  newVariant(asyncVariant, { wasmModule: quickJsWasm }),
).then((module) => createQuickJsEvaluator(module, {
  memoryLimitBytes: 64 * 1024 * 1024,
  stackLimitBytes: 512 * 1024,
}));

export interface Env {
  HOSTED_AGENT: DurableObjectNamespace<HostedNanocodex>;
  API_TOKEN: string;
  TEMPO_PRIVATE_KEY: string;
}

const json = (body: unknown, init: ResponseInit = {}) => Response.json(body, {
  ...init,
  headers: { "cache-control": "no-store", ...init.headers },
});

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/health") {
      await quickJsEvaluator;
      return json({ service: "nanocodex-fetch-mcp", code_mode: "quickjs-wasm", status: "ok" });
    }
    if (request.method !== "POST" || url.pathname !== "/v1/fetch") {
      return json({ error: "not_found" }, { status: 404 });
    }
    if (!env.API_TOKEN || !env.TEMPO_PRIVATE_KEY) {
      return json({ error: "server_not_configured" }, { status: 503 });
    }
    if (!authorized(request.headers.get("authorization"), env.API_TOKEN)) {
      return json({ error: "unauthorized" }, { status: 401 });
    }
    const body = await request.text();
    if (body.length > 64 * 1024) {
      return json({ error: "request body exceeds 64 KiB" }, { status: 413 });
    }
    return env.HOSTED_AGENT.getByName("tempo-wallet").fetch("https://agent.internal/prompt", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body,
    });
  },
};

export class HostedNanocodex extends DurableObject<Env> {
  #queue = Promise.resolve();

  fetch(request: Request): Promise<Response> {
    const response = this.#queue.then(() => this.#prompt(request));
    this.#queue = response.then(() => {}, () => {});
    return response;
  }

  async #prompt(request: Request): Promise<Response> {
    if (request.method !== "POST" || new URL(request.url).pathname !== "/prompt") {
      return json({ error: "not_found" }, { status: 404 });
    }
    let input;
    try {
      input = parseFetchPrompt(await request.text());
    } catch (error) {
      if (error instanceof RequestError) return json({ error: error.message }, { status: error.status });
      throw error;
    }
    let privateKey: `0x${string}`;
    try {
      privateKey = parsePrivateKey(this.env.TEMPO_PRIVATE_KEY);
    } catch (error) {
      return json({ error: errorMessage(error) }, { status: 503 });
    }

    const account = privateKeyToAccount(privateKey);
    const client = createClient({ chain: tempoChain, transport: http() });
    const storage = this.ctx.storage;
    const channelStore = createJsonChannelStore({
      get: (key) => storage.get<string>(`mpp:${key}`),
      async set(key, value) {
        await storage.put(`mpp:${key}`, value);
      },
      async delete(key) {
        await storage.delete(`mpp:${key}`);
      },
    });
    const mcpChannels = new Map<string, bigint>();
    const mcpMethod = tempo({
      account,
      autoSwap: { tokenIn: [PATH_USD], slippage: 1 },
      channelStore,
      getClient: () => client,
      maxDeposit: "0.10",
      topUpAmount: "0.05",
      onChannelUpdate(entry) {
        mcpChannels.set(entry.channelId, entry.cumulativeAmount);
      },
    });
    const modelMpp = tempo.session.manager({
      account,
      autoSwap: { tokenIn: [PATH_USD], slippage: 1 },
      bootstrap: true,
      channelStore,
      client,
      maxDeposit: "0.10",
      topUpAmount: "0.05",
    });

    let agent: Awaited<ReturnType<typeof Agent.create>> | undefined;
    try {
      agent = await Agent.create({
        module: nanocodexWasm,
        transport: Transport.mpp({
          session: createTempoProvider({
            session: modelMpp,
            payment: { methods: [mcpMethod] },
          }),
        }),
        codeEvaluator: await quickJsEvaluator,
        thinking: input.thinking,
        instructions: "You are Nanocodex hosted in a Cloudflare Worker. Remote MCP tools are deferred: use tool_search first, then call discovered mcp__mercator__* tools only from Code Mode.",
        workspace: "/workspace",
      });
      const turn = agent.turn.prompt({ input: input.prompt });
      try {
        const result = await turn.result();
        try {
          const mercatorCumulative = [...mcpChannels.values()]
            .reduce((total, amount) => total + amount, 0n);
          return json({
            final_message: result.finalMessage,
            usage: await result.usage(),
            payments: {
              model_cumulative: formatUnits(modelMpp.cumulative, 6),
              mercator_cumulative: formatUnits(mercatorCumulative, 6),
            },
          });
        } finally {
          result.dispose();
        }
      } finally {
        turn.dispose();
      }
    } catch (error) {
      return json({ error: errorMessage(error) }, { status: 502 });
    } finally {
      await agent?.session.shutdown().catch(() => {});
      await modelMpp.close().catch(() => {});
    }
  }
}

function parsePrivateKey(value: string): `0x${string}` {
  if (!/^0x[0-9a-fA-F]{64}$/.test(value)) {
    throw new Error("TEMPO_PRIVATE_KEY must be a 32-byte 0x-prefixed private key");
  }
  return value as `0x${string}`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
