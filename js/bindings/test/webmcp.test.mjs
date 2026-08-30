import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { createProvider, publish } from "../webmcp/WebMcp.mjs";
import { generate, generateFile, validate } from "../webmcp/generator.mjs";

test("WebMCP provider mirrors native tools, preserves session execution, and refreshes", async () => {
  const listeners = new Set();
  const calls = [];
  const confirmations = [];
  let tools = [registeredTool("search.products", true), registeredTool("checkout", false)];
  const document = {
    modelContext: {
      addEventListener(type, listener) { if (type === "toolchange") listeners.add(listener); },
      removeEventListener(type, listener) { if (type === "toolchange") listeners.delete(listener); },
      async getTools(options) {
        assert.deepEqual(options, { fromOrigins: ["https://shop.example"] });
        return tools;
      },
      async executeTool(tool, input, options) {
        const parsed = JSON.parse(input);
        calls.push({ tool: tool.name, input: parsed, signal: options.signal });
        return JSON.stringify({ tool: tool.name, input: parsed, authenticated: true });
      },
    },
  };
  const provider = await createProvider({
    document,
    fallback: false,
    fromOrigins: ["https://shop.example"],
    confirm: async (request) => { confirmations.push(request); return true; },
  });

  assert.deepEqual(provider.definitions().map(({ name }) => name), ["web_checkout", "web_search_products"]);
  const signal = new AbortController().signal;
  assert.deepEqual(
    await provider.resolve("web_search_products").handler({ query: "boots" }, { signal }),
    { tool: "search.products", input: { query: "boots" }, authenticated: true },
  );
  assert.equal(confirmations.length, 0);
  assert.deepEqual(
    await provider.resolve("web_checkout").handler({ cart: "current" }, { signal }),
    { tool: "checkout", input: { cart: "current" }, authenticated: true },
  );
  assert.equal(confirmations.length, 1);
  assert.equal(confirmations[0].origin, "https://shop.example");
  assert.equal(calls.every((call) => call.signal === signal), true);

  const snapshots = [];
  provider.subscribe((definitions) => snapshots.push(definitions.map(({ name }) => name)));
  tools = [registeredTool("account.profile", true)];
  for (const listener of listeners) listener();
  await waitFor(() => snapshots.length === 1);
  assert.deepEqual(snapshots[0], ["web_account_profile"]);
  assert.equal(provider.resolve("web_checkout"), undefined);
  provider.close();
  assert.equal(listeners.size, 0);
});

test("mutating WebMCP tools execute only inside the one-time dialog approval", async () => {
  let pageCalls = 0;
  let request;
  let execution;
  const document = {
    title: "Sodium Bank",
    location: { href: "https://sodium.example/accounts", origin: "https://sodium.example" },
    modelContext: {
      async getTools() { return [registeredTool("send_transfer", false)]; },
      async executeTool(_tool, input) {
        pageCalls += 1;
        return JSON.stringify({ sent: JSON.parse(input).amount });
      },
    },
  };
  const provider = await createProvider({
    document,
    fallback: false,
    dialog: {
      async open(nextRequest, nextExecution) {
        request = nextRequest;
        execution = nextExecution;
        assert.equal(pageCalls, 0);
        return nextExecution.execute();
      },
    },
  });

  assert.deepEqual(
    await provider.resolve("web_send_transfer").handler({ amount: "25.00", to: "Ada" }, {}),
    { sent: "25.00" },
  );
  assert.equal(pageCalls, 1);
  assert.equal(request.type, "webMcpApproval");
  assert.equal(request.app.name, "Sodium Bank");
  assert.equal(request.app.origin, "https://sodium.example");
  assert.equal(request.action.name, "send_transfer");
  assert.deepEqual(request.action.input, { amount: "25.00", to: "Ada" });
  assert.equal(typeof execution.execute, "function");
  provider.close();
});

test("a rejected WebMCP approval never reaches the website handler", async () => {
  let pageCalls = 0;
  const provider = await createProvider({
    document: {
      title: "Sodium Bank",
      location: { href: "https://sodium.example", origin: "https://sodium.example" },
      modelContext: {
        async getTools() { return [registeredTool("send_transfer", false)]; },
        async executeTool() { pageCalls += 1; },
      },
    },
    fallback: false,
    dialog: { async open() { throw new Error("The request was not approved."); } },
  });
  await assert.rejects(
    provider.resolve("web_send_transfer").handler({ amount: "25.00" }, {}),
    /not approved/,
  );
  assert.equal(pageCalls, 0);
  provider.close();
});

test("semantic fallback observes, fills, activates, and submits only retained visible elements", async () => {
  const form = element("form", { name: "support" });
  form.requestSubmit = () => { form.submitted = true; };
  const input = element("input", { name: "email", type: "email" });
  input.form = form;
  const button = element("button", { text: "Send" });
  button.form = form;
  button.click = () => { button.clicked = true; };
  const document = fakeDocument([button, input], [form]);
  const approvals = [];
  const provider = await createProvider({
    document,
    native: false,
    fallback: "always",
    confirm: (request) => { approvals.push(request.name); return true; },
  });
  const context = { signal: new AbortController().signal };
  const observed = await provider.resolve("web_page_observe").handler({}, context);
  assert.equal(observed.title, "Fixture");
  assert.equal(observed.elements.length, 2);
  assert.equal(observed.forms.length, 1);
  const inputDescription = observed.elements.find(({ name }) => name === "email");
  const buttonDescription = observed.elements.find(({ role }) => role === "button");

  assert.deepEqual(
    await provider.resolve("web_page_fill").handler({
      values: [{ id: inputDescription.id, value: "agent@example.com" }],
    }, context),
    { filled: [inputDescription.id] },
  );
  assert.equal(input.value, "agent@example.com");
  await provider.resolve("web_page_activate").handler({ id: buttonDescription.id }, context);
  await provider.resolve("web_page_submit").handler({ id: observed.forms[0].id }, context);
  assert.equal(button.clicked, true);
  assert.equal(form.submitted, true);
  assert.deepEqual(approvals, ["web_page_fill", "web_page_activate", "web_page_submit"]);

  button.isConnected = false;
  await assert.rejects(
    provider.resolve("web_page_activate").handler({ id: buttonDescription.id }, context),
    /no longer available/,
  );
  provider.close();
});

test("publisher registers only approved tools and aborts registrations on close", async () => {
  const registrations = [];
  const document = {
    modelContext: {
      async registerTool(tool, options) { registrations.push({ tool, options }); },
    },
  };
  const publication = await publish({
    version: 1,
    tools: [
      {
        name: "get_profile",
        description: "Read the signed-in profile.",
        approved: true,
        inputSchema: { type: "object", additionalProperties: false },
        annotations: { readOnlyHint: true },
        implementation: { kind: "custom" },
      },
      {
        name: "delete_profile",
        description: "Delete the signed-in profile.",
        approved: false,
        implementation: { kind: "custom" },
      },
    ],
  }, {
    document,
    exposedTo: ["https://embed.example"],
    handlers: { get_profile: () => ({ id: "current-user" }) },
  });
  assert.deepEqual(publication.tools, ["get_profile"]);
  assert.equal(registrations.length, 1);
  assert.deepEqual(registrations[0].options.exposedTo, ["https://embed.example"]);
  assert.deepEqual(await registrations[0].tool.execute({}), { id: "current-user" });
  assert.equal(registrations[0].options.signal.aborted, false);
  publication.close();
  assert.equal(registrations[0].options.signal.aborted, true);
});

test("publisher keeps generated fetches same-origin and preserves cancellation and session credentials", async () => {
  let registration;
  const requests = [];
  const document = {
    location: { href: "https://shop.example/account" },
    modelContext: {
      async registerTool(tool, options) { registration = { tool, options }; },
    },
  };
  const publication = await publish({
    version: 1,
    tools: [{
      name: "update_order",
      description: "Update one signed-in order.",
      approved: true,
      annotations: { readOnlyHint: false },
      implementation: { kind: "fetch", method: "POST", path: "/api/orders/[id]" },
    }],
  }, {
    document,
    confirm: () => true,
    async fetch(url, options) {
      requests.push({ url: String(url), options });
      return Response.json({ updated: true });
    },
  });
  const controller = new AbortController();
  assert.deepEqual(await registration.tool.execute({
    path: { id: "order 7" },
    query: { view: "full" },
    body: { status: "ready" },
  }, { signal: controller.signal }), { updated: true });
  assert.equal(requests[0].url, "https://shop.example/api/orders/order%207?view=full");
  assert.equal(requests[0].options.credentials, "same-origin");
  assert.equal(requests[0].options.signal, controller.signal);
  assert.equal(requests[0].options.body, JSON.stringify({ status: "ready" }));
  publication.close();

  registration = undefined;
  const unsafe = await publish({
    version: 1,
    tools: [{
      name: "unsafe_fetch",
      description: "An intentionally invalid cross-origin fixture.",
      approved: true,
      annotations: { readOnlyHint: true },
      implementation: { kind: "fetch", method: "GET", path: "https://other.example/data" },
    }],
  }, { document, fetch: () => { throw new Error("must not fetch"); } });
  await assert.rejects(registration.tool.execute({}), /must stay on https:\/\/shop\.example/);
  unsafe.close();
});

test("publisher aborts every attempted registration when publication fails", async () => {
  const signals = [];
  const document = { modelContext: {
    async registerTool(_tool, options) {
      signals.push(options.signal);
      if (signals.length === 2) throw new Error("registry rejected second tool");
    },
  } };
  await assert.rejects(publish({ version: 1, tools: [
    {
      name: "first",
      description: "First fixture.",
      approved: true,
      implementation: { kind: "custom" },
    },
    {
      name: "second",
      description: "Second fixture.",
      approved: true,
      implementation: { kind: "custom" },
    },
  ] }, {
    document,
    handlers: { first: () => null, second: () => null },
  }), /registry rejected second tool/);
  assert.equal(signals.length, 2);
  assert.equal(signals.every((signal) => signal.aborted), true);
});

test("repository generator finds routes, forms, GraphQL, tRPC, and server actions without executing code", async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-webmcp-"));
  try {
    await mkdir(join(directory, "app", "api", "orders", "[id]"), { recursive: true });
    await writeFile(join(directory, "app", "api", "orders", "[id]", "route.ts"), `
      export async function GET() { return Response.json({ ok: true }); }
      export async function DELETE() { throw new Error("must never execute"); }
    `);
    await writeFile(join(directory, "page.html"), `
      <form id="support" action="/api/support" method="post">
        <input name="email" type="email" required>
        <textarea name="message"></textarea>
      </form>
    `);
    await writeFile(join(directory, "client.ts"), `
      "use server";
      export async function inviteMember(input: unknown) { return input; }
      export const archiveMember = async (input: unknown) => input;
      const GetViewer = gql\`query GetViewer($id: ID!) { viewer { id } }\`;
      const router = { account: procedure
        .query(() => ({ id: 1 })) };
      fetch("/api/search", { method: "POST", body: "{}" });
    `);

    const manifest = await generate({ root: directory });
    assert.equal(validate(manifest), true);
    assert.equal(manifest.tools.every(({ approved }) => approved === false), true);
    const names = manifest.tools.map(({ name }) => name);
    assert.equal(names.includes("get_api_orders_id"), true);
    assert.equal(names.includes("delete_api_orders_id"), true);
    assert.equal(names.includes("form_support"), true);
    assert.equal(names.includes("graphql_GetViewer"), true);
    assert.equal(names.includes("action_inviteMember"), true);
    assert.equal(names.includes("action_archiveMember"), true);
    assert.equal(names.includes("trpc_account"), true);
    assert.equal(names.includes("post_api_search"), true);
    const encoded = JSON.stringify(manifest);
    assert.equal(encoded.includes("must never execute"), false);

    const path = join(directory, "webmcp.manifest.json");
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
    assert.equal(validate(JSON.parse(await readFile(path, "utf8"))), true);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("manifest generation preserves only approvals for unchanged tool contracts", async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-webmcp-manifest-"));
  const route = join(directory, "app", "api", "orders", "route.ts");
  const output = join(directory, "webmcp.manifest.json");
  try {
    await mkdir(join(directory, "app", "api", "orders"), { recursive: true });
    await writeFile(route, "export async function GET() { return Response.json([]); }\n");
    const first = await generateFile({ root: directory });
    assert.equal(first.changed, true);
    assert.equal(first.path, output);
    const reviewed = JSON.parse(await readFile(output, "utf8"));
    reviewed.tools[0].approved = true;
    await writeFile(output, `${JSON.stringify(reviewed, null, 2)}\n`);

    const unchanged = await generateFile({ root: directory });
    assert.equal(unchanged.changed, false);
    assert.equal(unchanged.manifest.tools[0].approved, true);

    await writeFile(route, "export async function GET() { return Response.json([1]); }\n");
    const implementationChanged = await generateFile({ root: directory });
    assert.equal(implementationChanged.changed, true);
    assert.equal(implementationChanged.manifest.tools[0].name, "get_api_orders");
    assert.equal(implementationChanged.manifest.tools[0].approved, false);

    await writeFile(route, "export async function POST() { return Response.json({ ok: true }); }\n");
    const changed = await generateFile({ root: directory });
    assert.equal(changed.changed, true);
    assert.equal(changed.manifest.tools[0].name, "post_api_orders");
    assert.equal(changed.manifest.tools[0].approved, false);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

function registeredTool(name, readOnlyHint) {
  return {
    name,
    title: name,
    description: `Use ${name}.`,
    inputSchema: { type: "object", additionalProperties: true },
    origin: "https://shop.example",
    annotations: { readOnlyHint, untrustedContentHint: readOnlyHint },
    window: {},
  };
}

function element(tagName, attributes = {}) {
  const attrs = new Map(Object.entries(attributes)
    .filter(([name]) => name !== "text")
    .map(([name, value]) => [name, String(value)]));
  return {
    tagName: tagName.toUpperCase(),
    type: attributes.type ?? "",
    value: "",
    checked: false,
    hidden: false,
    disabled: false,
    isConnected: true,
    innerText: attributes.text ?? "",
    getAttribute: (name) => attrs.get(name) ?? null,
    getClientRects: () => [{}],
    dispatchEvent() {},
  };
}

function fakeDocument(actionable, forms) {
  return {
    title: "Fixture",
    location: { href: "https://app.example/settings" },
    body: { innerText: "Account settings Send" },
    defaultView: { Event: class { constructor(type) { this.type = type; } } },
    querySelectorAll(selector) { return selector === "form" ? forms : actionable; },
  };
}

async function waitFor(predicate) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail("timed out waiting for WebMCP update");
}
