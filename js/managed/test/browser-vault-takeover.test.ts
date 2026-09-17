import { describe, expect, it, vi } from "vitest";
import { privateVaultTakeover, type BrowserVaultTakeoverAction } from "../src/browser-vault-takeover";
const identity = { vault_id: "a".repeat(22), target_id: "tab1", expected_origin: "https://login.example" };
const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a1X8AAAAASUVORK5CYII=";
function fixture(override?: (method: string) => any) {
  const calls: { method: string; params: any; sid?: string }[] = [];
  const send = vi.fn(async (method: string, params: any, sid?: string) => {
    calls.push({ method, params, sid });
    const replaced = override?.(method);
    if (replaced !== undefined) return replaced;
    switch (method) {
      case "Target.getTargetInfo": return { targetInfo: { type: "page", targetId: "tab1", url: identity.expected_origin } };
      case "Target.attachToTarget": return { sessionId: "private" };
      case "Page.getFrameTree": return { frameTree: { frame: { id: "top", url: identity.expected_origin } } };
      case "Page.getLayoutMetrics": return { cssLayoutViewport: { clientWidth: 800, clientHeight: 600 }, layoutViewport: { clientWidth: 1600, clientHeight: 1200 } };
      case "Page.captureScreenshot": return { data: png };
      default: return {};
    }
  });
  return { send, calls };
}
const failure = /^Private browser takeover could not be completed safely$/;
describe("human-only private browser takeover", () => {
  it("returns only PNG pixels and dimensions through private CDP", async () => {
    const cdp = fixture();
    expect(await privateVaultTakeover(cdp, identity, { action: "observe" })).toEqual({ status: "active", image: `data:image/png;base64,${png}`, width: 1, height: 1 });
    expect(cdp.calls.map(c => c.method)).toEqual(["Target.getTargetInfo", "Target.attachToTarget", "Target.getTargetInfo", "Page.getFrameTree", "Page.getLayoutMetrics", "Target.getTargetInfo", "Page.getFrameTree", "Page.captureScreenshot", "Target.getTargetInfo", "Page.getFrameTree", "Target.detachFromTarget"]);
    expect(cdp.calls.find(c => c.method === "Page.captureScreenshot")).toEqual({ method: "Page.captureScreenshot", params: { format: "png", fromSurface: true, captureBeyondViewport: false }, sid: "private" });
  });
  it.each([
    [{ action: "click", x: 0.5, y: 0.5 }, "Input.dispatchMouseEvent", { type: "mousePressed", x: 400, y: 300, button: "left", clickCount: 1 }],
    [{ action: "type", text: "human test input" }, "Input.insertText", { text: "human test input" }],
    [{ action: "key", key: "Enter" }, "Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 }],
    [{ action: "scroll", delta_y: -2000 }, "Input.dispatchMouseEvent", { type: "mouseWheel", x: 400, y: 300, deltaX: 0, deltaY: -2000 }],
  ] as const)("dispatches fixed input with before/after checks (%j)", async (action, method, params) => {
    const cdp = fixture();
    await privateVaultTakeover(cdp, identity, action);
    expect(cdp.calls.find(c => c.method === method)).toEqual({ method, params, sid: "private" });
    for (const [index, call] of cdp.calls.entries()) {
      if (!call.method.startsWith("Input.")) continue;
      expect(cdp.calls[index - 1]?.method).toBe("Page.getFrameTree");
      expect(cdp.calls[index + 1]?.method).toBe("Target.getTargetInfo");
      expect(cdp.calls[index + 2]?.method).toBe("Page.getFrameTree");
    }
    expect(cdp.calls.some(c => c.method.startsWith("Runtime."))).toBe(false);
  });
  it.each([null, {}, { action: "finish" }, { action: "observe", url: "https://provider.invalid" }, { action: "click", x: NaN, y: 0 }, { action: "click", x: 0, y: 1.1 }, { action: "type", text: "x".repeat(513) }, { action: "type", text: "" }, { action: "key", key: "F12" }, { action: "key", key: "toString" }, { action: "scroll", delta_y: Infinity }, { action: "scroll", delta_y: 2001 }])("rejects invalid actions before CDP (%j)", async action => {
    const cdp = fixture();
    await expect(privateVaultTakeover(cdp, identity, action as BrowserVaultTakeoverAction)).rejects.toThrow(failure);
    expect(cdp.calls).toEqual([]);
  });
  it.each(["http://login.example", "https://login.example/path", "https://other.example"])("rejects invalid/unapproved origins (%s)", async origin => {
    const cdp = fixture();
    await expect(privateVaultTakeover(cdp, { ...identity, expected_origin: origin }, { action: "observe" })).rejects.toThrow(failure);
    expect(cdp.calls.some(c => c.method === "Page.captureScreenshot")).toBe(false);
  });
  it.each(["target", "frame", "after-input", "after-screenshot"])("suppresses output on origin changes (%s)", async phase => {
    let changed = false;
    const cdp = fixture(method => {
      if ((phase === "after-input" && method === "Input.insertText") || (phase === "after-screenshot" && method === "Page.captureScreenshot")) changed = true;
      if (method === "Target.getTargetInfo" && (phase === "target" || changed)) return { targetInfo: { type: "page", url: "https://other.example/SECRET" } };
      if (method === "Page.getFrameTree" && phase === "frame") return { frameTree: { frame: { id: "top", url: "https://other.example/SECRET" } } };
    });
    await expect(privateVaultTakeover(cdp, identity, { action: "type", text: "test" })).rejects.toThrow(failure);
    if (phase !== "after-screenshot") expect(cdp.calls.some(c => c.method === "Page.captureScreenshot")).toBe(false);
    if (phase !== "target") expect(cdp.calls.at(-1)?.method).toBe("Target.detachFromTarget");
  });
  it.each(["viewport", "image", "provider", "subframe", "target-type", "oversized-image"])("fails closed for malformed/provider responses (%s)", async kind => {
    const cdp = fixture(method => {
      if (kind === "provider") throw new Error("SECRET https://provider.invalid/session");
      if (kind === "viewport" && method === "Page.getLayoutMetrics") return { cssLayoutViewport: { clientWidth: 9000, clientHeight: 600 } };
      if (kind === "image" && method === "Page.captureScreenshot") return { data: btoa("not PNG data".repeat(10)) };
      if (kind === "oversized-image" && method === "Page.captureScreenshot") return { data: "A".repeat(8 * 1024 * 1024 + 4) };
      if (kind === "subframe" && method === "Page.getFrameTree") return { frameTree: { frame: { id: "sub", parentId: "top", url: identity.expected_origin } } };
      if (kind === "target-type" && method === "Target.getTargetInfo") return { targetInfo: { type: "worker", url: identity.expected_origin } };
    });
    await expect(privateVaultTakeover(cdp, identity, { action: "observe" })).rejects.toThrow(failure);
  });
});
