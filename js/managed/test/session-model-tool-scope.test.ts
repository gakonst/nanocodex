import { describe, expect, it, vi } from "vitest";
import { managedImageFetch, managedWebFetch } from "../src/managed-tool-fetch";
import { managedCredentialSubject, scopedManagedModelEgress, sessionCredentialOwner } from "../src/session-credential-ownership";

const storageId = "a".repeat(64);
const ownerId = "11111111-1111-4111-8111-111111111111";
const otherOwner = "22222222-2222-4222-8222-222222222222";
const sessionId = "018f25e8-7b51-7a32-8c4d-0123456789ab";
const subject = managedCredentialSubject(storageId);
const ownerHeader = "x-nanocodex-session-model-owner";
const coordinates = (owner_id: string) => ({ owner_id, session_id: sessionId, runtime_profile: "managed" });
const state = (owner = ownerId) => ({
  subject, storageId,
  binding: { ...coordinates(owner), subject: storageId, state: "active", strategy: "session_v1" },
  session: coordinates(owner), initialization: { ...coordinates(owner), state: "active" },
  deleting: false, deleted: false, exported: false, importPending: false,
});

function bindings() {
  const general: Request[] = [];
  const model: Request[] = [];
  return {
    general, model,
    generalBinding: { fetch: vi.fn(async (request: Request) => { general.push(request); return Response.json({ output: "general", data: [{ b64_json: "AAAA" }] }); }) } as unknown as Fetcher,
    modelBinding: { fetch: vi.fn(async (request: Request) => { model.push(request); return Response.json({ output: "private", data: [{ b64_json: "AAAA" }] }); }) } as unknown as Fetcher,
  };
}
const webCall = (fetcher: typeof fetch) => fetcher("https://managed-tools.internal/web-search", { method: "POST",
  body: JSON.stringify({ session_id: "s", commands: { search_query: [{ q: "fixture" }] } }) });
const imageCall = (fetcher: typeof fetch, images?: string[]) => fetcher("https://managed-tools.internal/image-generation", { method: "POST",
  body: JSON.stringify({ prompt: "draw", ...(images ? { images } : {}) }) });

describe("managed web search and image tools use the private Session model path", () => {
  it("a burst of search, generation, and edit calls reaches only the private binding with a live owner (no callback route)", async () => {
    const b = bindings();
    const owner = vi.fn(() => sessionCredentialOwner(state()));
    const egress = scopedManagedModelEgress(b.generalBinding, storageId, subject,
      { binding: b.modelBinding, owner }, "acct-pinned");
    const web = managedWebFetch(egress, storageId);
    const image = managedImageFetch(egress, storageId);
    for (let i = 0; i < 6; i++) {
      expect((await webCall(web)).status).toBe(200);
      expect((await imageCall(image)).status).toBe(200);
      expect((await imageCall(image, ["data:image/png;base64,AAAA"])).status).toBe(200);
    }
    expect(b.general).toHaveLength(0);
    expect(b.model).toHaveLength(18);
    expect(owner).toHaveBeenCalledTimes(18); // re-evaluated per request, never cached
    expect(b.model.map(request => new URL(request.url).pathname).slice(0, 3))
      .toEqual(["/v1/search", "/v1/images/generations", "/v1/images/edits"]);
    for (const request of b.model) {
      expect(request.method).toBe("POST");
      expect(request.headers.get("x-nanocodex-subject")).toBe(subject);
      expect(request.headers.get(ownerHeader)).toBe(ownerId);
      expect(request.headers.get("authorization")).toBe("Bearer NANOCODEX_PROVIDER_CREDENTIAL");
      // The retained ChatGPT account selection is preserved; tool calls carry no placement.
      expect(request.headers.get("x-nanocodex-chatgpt-account-id")).toBe("acct-pinned");
      expect(request.headers.has("x-nanocodex-model-region")).toBe(false);
    }
    expect(await b.model[0]!.json()).toMatchObject({ id: "s", commands: { search_query: [{ q: "fixture" }] },
      settings: { allowed_callers: ["direct"], external_web_access: true } });
  });

  it("re-reads ownership per request: owner rotation applies immediately and deletion fails closed without fallback", async () => {
    const b = bindings();
    const states = [state(), state(otherOwner), { ...state(otherOwner), deleting: true }, { ...state(otherOwner), deleted: true },
      { ...state(otherOwner), exported: true }, { ...state(otherOwner), importPending: true }];
    let index = 0;
    const egress = scopedManagedModelEgress(b.generalBinding, storageId, subject,
      { binding: b.modelBinding, owner: () => sessionCredentialOwner(states[index]!) });
    const web = managedWebFetch(egress, storageId);
    const image = managedImageFetch(egress, storageId);
    await webCall(web); index = 1; await webCall(web); await imageCall(image);
    expect(b.model.map(request => request.headers.get(ownerHeader))).toEqual([ownerId, otherOwner, otherOwner]);
    for (index = 2; index < states.length; index++) {
      await expect(webCall(web)).rejects.toThrow("managed model ownership is unavailable");
      const failed = await imageCall(image);
      expect(failed.status).toBe(502);
      expect(await failed.json()).toEqual({ error: "image generation request failed" });
    }
    expect(b.model).toHaveLength(3);
    expect(b.general).toHaveLength(0); // never the callback-prone general broker for the Session's own subject
  });

  it("replaces injected owner and account headers and rejects a caller-chosen subject", async () => {
    const b = bindings();
    for (const pin of [undefined, "acct-pinned"]) {
      const egress = scopedManagedModelEgress(b.generalBinding, storageId, subject,
        { binding: b.modelBinding, owner: () => ownerId }, pin);
      await egress.fetch("https://nanocodex.internal/v1/images/generations", { method: "POST", headers: {
        "x-nanocodex-subject": storageId, [ownerHeader]: otherOwner,
        "x-nanocodex-chatgpt-account-id": "spoofed", "content-type": "application/json",
      }, body: "{}" });
      const forwarded = b.model.at(-1)!;
      expect(forwarded.headers.get(ownerHeader)).toBe(ownerId);
      expect(forwarded.headers.get("x-nanocodex-chatgpt-account-id")).toBe(pin ?? null);
      expect(forwarded.headers.get("x-nanocodex-subject")).toBe(subject);
      // Only the storage identity is accepted; the wrapper selects the credential subject.
      expect(() => egress.fetch("https://nanocodex.internal/v1/search", { method: "POST",
        headers: { "x-nanocodex-subject": managedCredentialSubject("b".repeat(64)) } })).toThrow("managed model subject mismatch");
    }
    expect(b.model).toHaveLength(2);
    expect(b.general).toHaveLength(0);
  });

  it("other methods, queries, and routes never receive a Session owner assertion", async () => {
    const b = bindings();
    const owner = vi.fn(() => ownerId);
    const egress = scopedManagedModelEgress(b.generalBinding, storageId, subject, { binding: b.modelBinding, owner });
    for (const [method, path] of [["GET", "/v1/search"], ["PUT", "/v1/images/edits"], ["POST", "/v1/search?x=1"],
      ["POST", "/v1/images/variations"], ["POST", "/v1/images/edits/"], ["POST", "/v1/realtime/calls"]] as const) {
      await egress.fetch(`https://nanocodex.internal${path}`, { method, headers: { "x-nanocodex-subject": storageId },
        ...(method === "GET" ? {} : { body: "{}" }) });
    }
    await egress.fetch("https://example.com/v1/search", { method: "POST", headers: { "x-nanocodex-subject": storageId }, body: "{}" });
    expect(b.model).toHaveLength(0);
    expect(owner).not.toHaveBeenCalled();
    expect(b.general).toHaveLength(7);
    for (const request of b.general) expect(request.headers.has(ownerHeader)).toBe(false);
  });

  it("legacy directory subjects and deployments without the private binding keep the general broker", async () => {
    const b = bindings();
    const owner = vi.fn(() => ownerId);
    const legacy = scopedManagedModelEgress(b.generalBinding, storageId, storageId, undefined, "acct-pinned");
    const unbound = scopedManagedModelEgress(b.generalBinding, storageId, subject, undefined);
    expect((await webCall(managedWebFetch(legacy, storageId))).status).toBe(200);
    expect((await imageCall(managedImageFetch(unbound, storageId))).status).toBe(200);
    expect(b.model).toHaveLength(0);
    expect(owner).not.toHaveBeenCalled();
    expect(b.general.map(request => request.headers.get("x-nanocodex-subject"))).toEqual([storageId, subject]);
    expect(b.general[0]!.headers.get("x-nanocodex-chatgpt-account-id")).toBe("acct-pinned");
    for (const request of b.general) expect(request.headers.has(ownerHeader)).toBe(false);
  });

  it("invalid tool input never reaches any binding", async () => {
    const b = bindings();
    const egress = scopedManagedModelEgress(b.generalBinding, storageId, subject, { binding: b.modelBinding, owner: () => ownerId });
    const web = managedWebFetch(egress, storageId);
    for (const body of [{}, { session_id: "s" }, { session_id: "", commands: {} }, { session_id: "s", commands: [] },
      { session_id: "s", commands: {}, model: "not-a-model" }]) {
      expect((await web("https://managed-tools.internal/web-search", { method: "POST", body: JSON.stringify(body) })).status).toBe(400);
    }
    expect((await managedImageFetch(egress, storageId)("https://managed-tools.internal/image-generation", { method: "POST", body: "{" })).status).toBe(400);
    expect(b.model).toHaveLength(0);
    expect(b.general).toHaveLength(0);
  });
});

describe("managed web search and image tool wiring", () => {
  it("the Session builds both tools on its scoped model egress, never the general binding", async () => {
    const rawIndex = "../src/index.ts?raw";
    const { default: source } = await import(/* @vite-ignore */ rawIndex) as { default: string };
    expect(source).toMatch(/web\(\{\s*url: "https:\/\/managed-tools\.internal\/web-search",\s*fetch: managedWebFetch\(this\.#modelEgress\(\), this\.ctx\.id\.toString\(\)\)/);
    expect(source).toMatch(/imageGeneration\(\{\s*url: "https:\/\/managed-tools\.internal\/image-generation",\s*fetch: managedImageFetch\(this\.#modelEgress\(\), this\.ctx\.id\.toString\(\)\)/);
    expect(source).not.toMatch(/managed(?:Web|Image)Fetch\(this\.env\b/);
    expect(source).not.toMatch(/function fetchManagedTool\(/);
    const rawTool = "../src/managed-tool-fetch.ts?raw";
    const { default: tool } = await import(/* @vite-ignore */ rawTool) as { default: string };
    expect(tool).not.toMatch(/NANOCODEX\b|env\./);
  });
});
