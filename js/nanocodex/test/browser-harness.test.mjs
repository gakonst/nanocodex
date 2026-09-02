import assert from "node:assert/strict";
import { test } from "node:test";

import { bindBrowser } from "../tools/browser/index.mjs";
import * as datasets from "../tools/dataset.mjs";
import { namedTool } from "../tools/namedTool.mjs";
import * as standard from "../tools/standard.mjs";

const context = Object.freeze({
  callId: "browser-harness-call",
  parentCallId: "",
  sessionId: "browser-harness-session",
  signal: new AbortController().signal,
});

const shellDescriptor = Object.freeze({
  shell: "nanocodex-just-bash",
  commands: Object.freeze(["curl", "gh", "git", "python3"]),
  customCommands: Object.freeze(["gh", "git", "python3"]),
  cwd: "/workspace",
  limits: Object.freeze({ maxFileSystemBytes: 256 * 1024 * 1024 }),
  network: Object.freeze({ enabled: true, mode: "connector-http-gateway" }),
  pty: false,
  sessions: false,
  sandboxEscalation: false,
});

test("the default browser harness exposes one exact model-visible tool set", async () => {
  const requests = [];
  const workspace = {
    async readFile(path) {
      assert.equal(path, "/workspace/pixel.png");
      return Uint8Array.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    },
  };
  const runtime = bindBrowser({
    datasets,
    fetch: async (input) => {
      requests.push(String(input));
      return Response.json({
        connectors: {
          github: { connected: true, connections: [{
            id: "a".repeat(43),
            label: "Nano Cat (nanocat)",
            account_id: "github-account",
            capabilities: ["github"],
            access_token: "hidden",
          }] },
          gmail: { connected: false },
          gdrive: { connected: true, connections: [{
            id: "b".repeat(43),
            label: "Drive User",
            account_id: "google-account",
            capabilities: ["gmail", "gdrive", "gcalendar"],
            access_token: "hidden",
          }] },
          gcalendar: { connected: true, connections: [{
            id: "b".repeat(43),
            label: "Drive User",
            account_id: "google-account",
            capabilities: ["gmail", "gdrive", "gcalendar"],
          }] },
          slack: { connected: true, connections: [{
            id: "c".repeat(43),
            label: "Acme (U123)",
            account_id: "T123:U123",
            capabilities: ["slack"],
          }] },
          x: { connected: true, label: "Nano Cat (@nanocat)", account_id: "hidden" },
        },
      });
    },
    origin: "https://demo.test",
    standard,
    threadId: "browser-harness-thread",
    shell: {
      descriptor: shellDescriptor,
      artifactTool: namedTool("render_artifact", {
        description: "Render an artifact.",
        handler: async () => ({ artifactId: "ui" }),
      }),
      execTool: {
        description: "Run a command.",
        handler: async ({ cmd }) => ({ exit_code: 0, output: `${cmd}\n` }),
      },
      instructions: "browser harness",
      projectInstructions: "project instructions",
      workspace,
    },
  }, {
    dataset: {
      fetch: async () => new Response('{"id":1}\n'),
    },
    images: {
      fetch: async (input, init) => {
        requests.push(new Request(input, init).url);
        return Response.json({ image_url: "data:image/png;base64,Z2VuZXJhdGVk" });
      },
    },
    web: {
      fetch: async (input, init) => {
        requests.push(new Request(input, init).url);
        return Response.json({ output: "searched" });
      },
    },
  });

  assert.equal(runtime.filesystem, workspace);
  assert.equal(runtime.instructions, "browser harness");
  assert.equal(runtime.projectInstructions, "project instructions");
  assert.deepEqual(runtime.tools.map(({ name }) => name), [
    "exec_command",
    "runtimeInfo",
    "accountInfo",
    "web__run",
    "image_gen__imagegen",
    "view_image",
    "update_plan",
    "dataset",
    "render_artifact",
  ]);
  assert(runtime.tools.every((tool) => Object.isFrozen(tool)));

  const byName = Object.fromEntries(runtime.tools.map((tool) => [tool.name, tool]));
  assert.deepEqual(await byName.exec_command.handler({ cmd: "pwd" }, context), {
    exit_code: 0,
    output: "pwd\n",
  });
  const accountInfo = await byName.accountInfo.handler({}, context);
  assert.deepEqual(accountInfo, {
    status: "ready",
    authenticated: ["github", "gdrive", "gcalendar", "slack", "x"],
    accounts: {
      github: "Nano Cat (nanocat)",
      gdrive: "Drive User",
      gcalendar: "Drive User",
      slack: "Acme (U123)",
      x: "Nano Cat (@nanocat)",
    },
    connectorAccounts: {
      github: [{ id: "a".repeat(43), label: "Nano Cat (nanocat)", accountId: "github-account", capabilities: ["github"] }],
      gdrive: [{ id: "b".repeat(43), label: "Drive User", accountId: "google-account", capabilities: ["gmail", "gdrive", "gcalendar"] }],
      gcalendar: [{ id: "b".repeat(43), label: "Drive User", accountId: "google-account", capabilities: ["gmail", "gdrive", "gcalendar"] }],
      slack: [{ id: "c".repeat(43), label: "Acme (U123)", accountId: "T123:U123", capabilities: ["slack"] }],
    },
    identity: {},
    stablecoins: [],
    authorizations: [],
  });
  const runtimeInfo = await byName.runtimeInfo.handler({}, context);
  assert.deepEqual(runtimeInfo.account, accountInfo);
  assert.equal(runtimeInfo.shell, shellDescriptor.shell);
  assert.equal(runtimeInfo.shell_network, shellDescriptor.network.mode);
  assert.equal(runtimeInfo.workspace, shellDescriptor.cwd);
  assert.deepEqual(runtimeInfo.commands, shellDescriptor.commands);
  assert.deepEqual(runtimeInfo.custom_commands, shellDescriptor.customCommands);
  assert.deepEqual(runtimeInfo.limits, shellDescriptor.limits);
  assert.equal(runtimeInfo.pty, false);
  assert.equal(runtimeInfo.sessions, false);
  assert.equal(runtimeInfo.sandbox_escalation, false);
  assert.equal(await byName.web__run.handler({ time: [{ utc_offset: "+03:00" }] }, context), "searched");
  assert.deepEqual(await byName.image_gen__imagegen.handler({ prompt: "draw" }, context), {
    image_url: "data:image/png;base64,Z2VuZXJhdGVk",
  });
  assert.deepEqual(requests, [
    "https://demo.test/v1/connectors",
    "https://demo.test/v1/connectors",
    "https://demo.test/api/tools/web-search",
    "https://demo.test/api/tools/image-generation",
  ]);
  const viewed = await byName.view_image.handler({ path: "/workspace/pixel.png" }, context);
  assert.deepEqual(viewed.output, [{
    type: "input_image",
    image_url: "data:image/png;base64,iVBORw0KGgo=",
    detail: "high",
  }]);
  assert.equal(viewed.structuredResult.image_url, "data:image/png;base64,iVBORw0KGgo=");
  assert.deepEqual(await byName.update_plan.handler({ plan: [] }, context), { updated: true });
  const opened = await byName.dataset.handler({
    operation: "open",
    source: {
      kind: "url",
      url: "https://data.example/browser-harness.jsonl",
      format: "jsonl",
    },
  }, context);
  assert.deepEqual(opened.previewRows, [{ id: 1 }]);
  assert.deepEqual(await byName.render_artifact.handler({}, context), { artifactId: "ui" });
});

test("the browser harness preserves explicit tool URLs", async () => {
  const urls = [];
  const runtime = bindBrowser(preparedBrowser(), {
    web: {
      url: "https://tools.test/search",
      fetch: async (input) => {
        urls.push(String(input));
        return Response.json({ output: "searched" });
      },
    },
    images: {
      url: "https://tools.test/images",
      fetch: async (input) => {
        urls.push(String(input));
        return Response.json({ image_url: "data:image/png;base64,Z2VuZXJhdGVk" });
      },
    },
  });
  const byName = Object.fromEntries(runtime.tools.map((tool) => [tool.name, tool]));
  await byName.web__run.handler({ search_query: [{ q: "override" }] }, context);
  await byName.image_gen__imagegen.handler({ prompt: "override" }, context);
  assert.deepEqual(urls, ["https://tools.test/search", "https://tools.test/images"]);
});

test("accountInfo adds app authorization without forwarding unknown control-plane fields", async () => {
  const runtime = bindBrowser({
    ...preparedBrowser(),
    fetch: async () => Response.json({
      connectors: { chatgpt: { connected: true, label: "Subscription" } },
      identity: { tempoAddress: "0xabc", brokerUserId: "secret" },
      stablecoins: [{
        token: "0x01",
        symbol: "MACH",
        balance: "5000000",
        decimals: 6,
        providerCredential: "secret",
      }],
      authorizations: [{
        appId: "atlas-workspace",
        permission: "agent.run",
        status: "active",
        expiresAt: 2_000_000_000,
        capabilities: ["nanocodex.agent", "x", "chatgpt"],
        connectors: ["x", "chatgpt"],
        accessKey: {
          id: "0x02",
          expiry: 2_000_000_000,
          limits: [{ token: "0x01", symbol: "MACH", limit: "10000000", period: 86_400 }],
          scopes: [{ address: "0x03", selector: "0x12345678", recipients: ["0x04"] }],
          witness: "secret",
        },
        spend: {
          token: "0x01",
          symbol: "MACH",
          spent: "250000",
          limit: "10000000",
          period: 86_400,
          maxPerRequest: "250000",
          credential: "secret",
        },
        grantToken: "secret",
      }],
    }),
  }, { accountInfo: { requireAuthorization: true } });
  const accountInfo = runtime.tools.find(({ name }) => name === "accountInfo");

  assert.deepEqual(await accountInfo.handler({}, context), {
    status: "ready",
    authenticated: ["chatgpt"],
    accounts: { chatgpt: "Subscription" },
    connectorAccounts: {},
    identity: { tempoAddress: "0xabc" },
    stablecoins: [{ token: "0x01", symbol: "MACH", balance: "5000000", decimals: 6 }],
    authorizations: [{
      appId: "atlas-workspace",
      permission: "agent.run",
      status: "active",
      expiresAt: 2_000_000_000,
      capabilities: ["nanocodex.agent", "x", "chatgpt"],
      connectors: ["x", "chatgpt"],
      accessKey: {
        id: "0x02",
        expiry: 2_000_000_000,
        limits: [{ token: "0x01", symbol: "MACH", limit: "10000000", period: 86_400 }],
        scopes: [{ address: "0x03", selector: "0x12345678", recipients: ["0x04"] }],
      },
      spend: {
        token: "0x01",
        symbol: "MACH",
        spent: "250000",
        limit: "10000000",
        period: 86_400,
        maxPerRequest: "250000",
      },
    }],
  });
});

function preparedBrowser() {
  const workspace = { async readFile() { return new Uint8Array(); } };
  return {
    datasets,
    fetch: async () => Response.json({ connectors: {} }),
    origin: "https://demo.test",
    standard,
    threadId: "browser-harness-overrides",
    shell: {
      descriptor: shellDescriptor,
      artifactTool: namedTool("render_artifact", {
        description: "Render an artifact.",
        handler: async () => ({ artifactId: "ui" }),
      }),
      execTool: { description: "Run a command.", handler: async () => ({}) },
      instructions: "browser harness",
      projectInstructions: "project instructions",
      workspace,
    },
  };
}
