import { describe, expect, it, vi } from "vitest";
import { fillBrowserVault, parseBrowserVaultRequest, PrivateBrowserCdp } from "../src/browser-vault";

const base = { vault_id: "a".repeat(22), expected_origin: "https://login.example", target_id: "tab1", submit: true };
function fixture() {
  const calls: { method: string; params: any }[] = [];
  const send = vi.fn(async (method: string, params: any) => {
    calls.push({ method, params });
    if (method === "Target.getTargetInfo") return { targetInfo: { type: "page", url: base.expected_origin } };
    if (method === "Target.attachToTarget") return { sessionId: "attached" };
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "top", loaderId: "loader", url: base.expected_origin } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 7 };
    return { result: { value: true } };
  });
  return { calls, send };
}
describe("private browser Vault boundary", () => {
  it("requires an exact HTTPS origin, explicit submit and at least one unique selector", () => {
    expect(parseBrowserVaultRequest({ ...base, username_selector: "#email" })).toMatchObject(base);
    expect(parseBrowserVaultRequest({ ...base, password_selector: "#pass" })).toMatchObject(base);
    for (const bad of [{ ...base }, { ...base, username_selector: "" }, { ...base, username_selector: "#x", password_selector: "#x" }, { ...base, username_selector: "#x", submit: undefined }, { ...base, username_selector: "#x", expected_origin: "https://login.example/path" }, { ...base, username_selector: "#x", unexpected: true }]) {
      expect(() => parseBrowserVaultRequest(bad)).toThrow("Invalid vault login request");
    }
  });
  it.each(["username_selector", "password_selector"])("sends only the requested step's credential, after durable quarantine (%s)", async selector => {
    const cdp = fixture();
    const quarantine = vi.fn(async () => { expect(cdp.calls.some(c => c.method === "Runtime.callFunctionOn")).toBe(false); });
    const result = await fillBrowserVault({ cdp, sessionId: "browser", request: parseBrowserVaultRequest({ ...base, [selector]: "#field" }), resolve: async () => ({ username: "fake-user", password: "fake-password" }), quarantine });
    expect(result).toEqual({ status: "submitted" });
    expect(quarantine).toHaveBeenCalledWith(expect.objectContaining({ origin: base.expected_origin, vaultId: base.vault_id }));
    const args = cdp.calls.at(-1)!.params.arguments;
    expect(args[selector === "username_selector" ? 4 : 3].value).toBeNull();
  });
  it("never injects after cancellation or quarantine failure and suppresses raw errors", async () => {
    for (const cancelled of [false, true]) {
      const cdp = fixture();
      const controller = new AbortController();
      const promise = fillBrowserVault({ cdp, sessionId: "browser", request: parseBrowserVaultRequest({ ...base, password_selector: "#field" }), signal: controller.signal, resolve: async () => { if (cancelled) controller.abort(); return { username: "fake", password: "secret" }; }, quarantine: async () => { throw new Error("SECRET raw provider error"); } });
      await expect(promise).rejects.toThrow(/^Vault login could not be filled safely$/);
      expect(cdp.calls.some(c => c.method === "Runtime.callFunctionOn")).toBe(false);
    }
  });
  it("uses the agents 0.22 reconnect URL without accepting a supplied endpoint", async () => {
    const pair = new WebSocketPair();
    const fetch = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => new Response(null, { status: 101, webSocket: pair[0] }));
    const cdp = await PrivateBrowserCdp.connect({ fetch }, "session/a?b");
    expect(fetch.mock.calls[0]?.[0]).toBe("https://localhost/v1/devtools/browser/session%2Fa%3Fb");
    cdp.close();
    await expect(cdp.send("Target.getTargets")).rejects.toThrow("Private browser disconnected");
  });
});
