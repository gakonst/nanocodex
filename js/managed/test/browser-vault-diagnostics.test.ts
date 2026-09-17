import { describe, expect, it } from "vitest";
import { actBrowserVault } from "../src/browser-vault";
const identity = { vault_id: "a".repeat(22), expected_origin: "https://example.test", target_id: "target" };
const action = { action: "click" as const, snapshot_id: "a".repeat(36), ref: "e1" };
function cdp(value: unknown) {
  return { async send(method: string) {
    if (method === "Target.getTargetInfo") return { targetInfo: { type: "page", url: identity.expected_origin } };
    if (method === "Target.attachToTarget") return { sessionId: "attached" };
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame", loaderId: "loader", url: identity.expected_origin } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    return { result: { value } };
  } };
}
describe("private click diagnostic projection", () => {
  it.each(["snapshot_missing", "stale_ref", "document_changed", "challenge_detected", "element_not_visible", "changed_element", "outside_viewport", "occluded", "unsafe_destination", "unsupported_element"])("projects only the fixed %s category", async reason => {
    await expect(actBrowserVault(cdp(reason), identity, action)).rejects.toThrow(`Private browser action could not be completed safely (${reason})`);
  });
  it.each(["https://secret.test/private", "changed_element password=secret", { reason: "occluded", html: "SECRET" }, false, null])("suppresses all non-allowlisted response data", async value => {
    await expect(actBrowserVault(cdp(value), identity, action)).rejects.toThrow(/^Private browser action could not be completed safely$/);
  });
});
