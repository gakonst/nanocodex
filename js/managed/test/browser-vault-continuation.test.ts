import { describe, expect, it, vi } from "vitest";
import { actBrowserVault, captureBrowserVaultBinding, captureBrowserVaultDocumentBinding, fillBrowserVaultOtp, inspectBrowserVault, sanitizeBrowserVaultText, snapshotBrowserVault } from "../src/browser-vault";
const identity = { vault_id: "a".repeat(22), expected_origin: "https://login.example", target_id: "tab1" };
function fixture(value: unknown | ((params: any) => unknown)) {
  const send = vi.fn(async (method: string, params: any) => {
    if (method === "Target.getTargetInfo") return { targetInfo: { type: "page", url: identity.expected_origin } };
    if (method === "Target.attachToTarget") return { sessionId: "attached" };
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "top", loaderId: "loader", url: identity.expected_origin } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 7 };
    return { result: { value: typeof value === "function" ? value(params) : value } };
  });
  return { send };
}
describe("private Vault continuation host boundary", () => {
  it("never infers authentication from absent inputs", async () => {
    expect(await inspectBrowserVault(fixture({ status: "unknown", flags: [false, false, false] }), identity)).toEqual({ status: "unknown" });
    await expect(inspectBrowserVault(fixture({ status: "authenticated", flags: [false, false, false] }), identity)).rejects.toThrow("Private login status is unavailable");
  });
  it("sanitizes all readable output including encoded and split credentials", async () => {
    const secret = "fake+user@example.test";
    const cdp = fixture((params: any) => ({ snapshot_id: params.arguments[2].value, status: "unknown", flags: [false, false, false], title: secret, text: `hello ${encodeURIComponent(secret)} ${encodeURIComponent(encodeURIComponent(secret))}`, elements: [{ ref: "e1", role: "link", text: secret }] }));
    const result = await snapshotBrowserVault(cdp, identity, [secret]);
    expect(result.title).toBe("[redacted]"); expect(result.text).toBe("hello [redacted] [redacted]"); expect(result.elements[0]!.text).toBe("[redacted]");
    expect(sanitizeBrowserVaultText("abc abc abc".split("").join(" "), ["abc"], 100)).toBe("[redacted]   [redacted]   [redacted]");
  });
  it("masks prior numeric OTP echoes after rehydration without exact code and omits URLs", () => {
    const echoes = ['826519', '826 519', '%38%32%36%35%31%39', '%2538%2532%2536%2535%2531%2539', '&#56;&#50;&#54;&#53;&#49;&#57;', '&#x38;&#x32;&#x36;&#x35;&#x31;&#x39;', btoa('826519')];
    for (const echo of echoes) expect(sanitizeBrowserVaultText(`Code: ${echo}`, [], 100)).toBe('Code: [redacted]');
    expect(sanitizeBrowserVaultText('Visit https://login.example/path?secret=826519', [], 100)).toBe('Visit [url omitted]');
  });
  it("rejects oversized/invalid snapshots and suppresses provider errors", async () => {
    const cdp = fixture((params: any) => ({ snapshot_id: params.arguments[2].value, status: "unknown", flags: [false, false, false], title: "secret", text: "x".repeat(65537), elements: [] }));
    await expect(snapshotBrowserVault(cdp, identity, ["secret"])).rejects.toThrow(/^Private page snapshot is unavailable$/);
    await expect(inspectBrowserVault({ send: async () => { throw new Error("SECRET provider details"); } }, identity)).rejects.toThrow(/^Private login status is unavailable$/);
  });
  it("rejects unsafe destinations before dispatch and validates reference shape", async () => {
    const cdp = fixture(true);
    for (const url of ["https://evil.test", "javascript:alert(1)", "https://user:pass@login.example"]) await expect(actBrowserVault(cdp, identity, { action: "navigate", url })).rejects.toThrow(/safely/);
    await expect(actBrowserVault(cdp, identity, { action: "click", snapshot_id: "x", ref: "body" })).rejects.toThrow(/safely/);
    expect(cdp.send).not.toHaveBeenCalled();
  });
  it("binds OTP to current document before resolving and suppresses secret responses", async () => {
    const cdp = fixture(true), resolve = vi.fn(async () => "826519");
    expect(await captureBrowserVaultBinding(fixture({ status: "otp_form", flags: [false, false, true] }), identity)).toMatchObject({ loaderId: "loader", otp_selector: expect.any(String) });
    await expect(fillBrowserVaultOtp({ cdp, request: { ...identity, otp_selector: "#otp", expected_loader_id: "stale" }, resolve, submit: true })).rejects.toThrow(/safely/);
    expect(resolve).not.toHaveBeenCalled();
    expect(await fillBrowserVaultOtp({ cdp, request: { ...identity, otp_selector: "#otp", expected_loader_id: "loader" }, resolve, submit: true })).toEqual({ status: "submitted" });
  });
  it("rejects navigation between loader discovery and isolated-world creation before resolving OTP", async () => {
    const cdp = fixture(true), originalSend = cdp.send;
    let frames = 0;
    const send = vi.fn(async (method: string, params: any) => {
      const result = await originalSend(method, params);
      if (method === 'Page.getFrameTree' && ++frames === 2) return { frameTree: { frame: { id: 'top', loaderId: 'new-loader', url: identity.expected_origin } } };
      return result;
    });
    const resolve = vi.fn(async () => '826519');
    await expect(fillBrowserVaultOtp({ cdp: { send }, request: { ...identity, otp_selector: '#otp', expected_loader_id: 'loader' }, resolve, submit: true })).rejects.toThrow(/safely/);
    expect(resolve).not.toHaveBeenCalled();
    expect(send.mock.calls.some(([method]) => method === 'Runtime.callFunctionOn')).toBe(false);
  });
  it("permits host takeover binding on challenge/unknown documents without enabling OTP", async () => {
    for (const status of ['challenge', 'unknown']) {
      const cdp = fixture({ status, flags: [false, false, false] });
      expect(await captureBrowserVaultDocumentBinding(cdp, identity)).toEqual({ loaderId: 'loader' });
      await expect(captureBrowserVaultBinding(cdp, identity)).rejects.toThrow('Private verification document is unavailable');
    }
  });
  it("blocks cross-origin reads without sending an isolated function", async () => {
    const cdp = { send: vi.fn(async () => ({ targetInfo: { type: "page", url: "https://other.example/secret" } })) };
    expect(await inspectBrowserVault(cdp, identity)).toEqual({ status: "destination_changed" });
    expect(cdp.send).toHaveBeenCalledTimes(1);
  });
});
