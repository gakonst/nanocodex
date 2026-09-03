import assert from "node:assert/strict";
import test from "node:test";
import type { ToolActivity } from "nanocodex-react/agent";
import {
  connectRequestedAccount,
  requestedAccountConnection,
} from "./accountConnectionRequest.ts";

function tool(output: unknown, overrides: Partial<ToolActivity> = {}): ToolActivity {
  return {
    callId: "connect",
    name: "requestAccountConnection",
    arguments: JSON.stringify({ connector: "gmail" }),
    output: JSON.stringify(output),
    status: "completed",
    children: [],
    ...overrides,
  };
}

test("projects a canonical account connection action from the structured tool result", () => {
  assert.deepEqual(requestedAccountConnection(tool({
    status: "user_action_required",
    action: "connect_account",
    connector: "gmail",
    label: "model-controlled label",
  })), { connector: "gmail", label: "Gmail" });
});

test("rejects incomplete, unsupported, failed, or unrelated tool results", () => {
  assert.equal(requestedAccountConnection(tool({
    status: "user_action_required",
    action: "connect_account",
    connector: "chatgpt",
    label: "ChatGPT",
  })), undefined);
  assert.equal(requestedAccountConnection(tool({
    status: "user_action_required",
    action: "connect_account",
    connector: "gmail",
    label: "Gmail",
    authorization_url: "https://provider.test/secret",
  })), undefined);
  assert.equal(requestedAccountConnection(tool({}, { status: "failed" })), undefined);
  assert.equal(requestedAccountConnection(tool({}, { name: "other" })), undefined);
});

test("starts OAuth from the click, accepts only its popup callback, and verifies the connector", async (t) => {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const originalFetch = globalThis.fetch;
  t.after(() => {
    if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
    else Reflect.deleteProperty(globalThis, "window");
    globalThis.fetch = originalFetch;
  });

  const messageListeners = new Set<(event: MessageEvent<unknown>) => void>();
  let authorizationHref = "";
  const popup = {
    closed: false,
    close() { this.closed = true; },
    location: {},
  };
  Object.defineProperty(popup.location, "href", {
    set(value: string) {
      authorizationHref = value;
      queueMicrotask(() => {
        const event = {
          data: {
            type: "nanocodex:connector-complete",
            connector: "google",
            result: "success",
          },
          origin: "https://nanocodex.test",
          source: popup,
        } as MessageEvent<unknown>;
        for (const listener of messageListeners) listener(event);
      });
    },
  });
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      location: { origin: "https://nanocodex.test" },
      open() { return popup; },
      addEventListener(type: string, listener: (event: MessageEvent<unknown>) => void) {
        if (type === "message") messageListeners.add(listener);
      },
      removeEventListener(type: string, listener: (event: MessageEvent<unknown>) => void) {
        if (type === "message") messageListeners.delete(listener);
      },
      setInterval: globalThis.setInterval,
      clearInterval: globalThis.clearInterval,
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
    },
  });

  const requests: Array<{ path: string; init?: RequestInit }> = [];
  globalThis.fetch = async (input, init) => {
    const path = String(input);
    requests.push({ path, init });
    return path === "/v1/connectors/google"
      ? Response.json({ authorization_url: "https://accounts.google.test/oauth" })
      : Response.json({ connectors: { gmail: { connected: true, connections: [] } } });
  };

  await connectRequestedAccount(
    { connector: "gmail", label: "Gmail" },
    new AbortController().signal,
  );

  assert.equal(authorizationHref, "https://accounts.google.test/oauth");
  assert.equal(popup.closed, true);
  assert.equal(messageListeners.size, 0);
  assert.deepEqual(requests.map(({ path }) => path), [
    "/v1/connectors/google",
    "/v1/connectors",
  ]);
  assert.deepEqual(JSON.parse(String(requests[0]?.init?.body)), { return_to: "/connect" });
  assert.equal(requests[0]?.init?.credentials, "same-origin");
  assert.equal(requests[1]?.init?.cache, "no-store");
});

test("fences concurrent account connection attempts before reusing the OAuth popup", async (t) => {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const originalFetch = globalThis.fetch;
  t.after(() => {
    if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
    else Reflect.deleteProperty(globalThis, "window");
    globalThis.fetch = originalFetch;
  });

  let popupOpens = 0;
  const popup = {
    closed: false,
    close() { this.closed = true; },
    location: { href: "" },
  };
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      location: { origin: "https://nanocodex.test" },
      open() {
        popupOpens += 1;
        return popup;
      },
      addEventListener() {},
      removeEventListener() {},
      setInterval: globalThis.setInterval,
      clearInterval: globalThis.clearInterval,
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
    },
  });
  globalThis.fetch = async () => Response.json({
    authorization_url: "https://accounts.google.test/oauth",
  });

  const controller = new AbortController();
  const first = connectRequestedAccount(
    { connector: "gmail", label: "Gmail" },
    controller.signal,
  );
  await assert.rejects(
    connectRequestedAccount(
      { connector: "gcalendar", label: "Google Calendar" },
      new AbortController().signal,
    ),
    /already in progress/,
  );
  assert.equal(popupOpens, 1);

  controller.abort();
  await assert.rejects(first, { name: "AbortError" });
  assert.equal(popup.closed, true);
});
