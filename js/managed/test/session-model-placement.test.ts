import { describe, expect, it, vi } from "vitest";
import { managedCredentialSubject, scopedManagedModelEgress } from "../src/session-credential-ownership";

const storageId = "a".repeat(64);
const regionHeader = "x-nanocodex-model-region";
const owner = "11111111-1111-4111-8111-111111111111";

describe("trusted initial Session ingress placement", () => {
  it.each(["GET", "POST"])("overwrites runtime headers and checks live authority on every %s reconnect", async (method) => {
    const general = { fetch: vi.fn(async (_request: Request) => new Response(null, { status: 204 })) };
    const model = { fetch: vi.fn(async (_request: Request) => new Response(null, { status: 204 })) };
    let colo: string | null = "LAX";
    let currentOwner: string | undefined = owner;
    const ownerLookup = vi.fn(() => currentOwner);
    const scoped = scopedManagedModelEgress(general as unknown as Fetcher, storageId, managedCredentialSubject(storageId), {
      binding: model as unknown as Fetcher, owner: ownerLookup, clientIngressColo: () => colo,
    });
    const headers = { "x-nanocodex-subject": storageId, [regionHeader]: "weur" };
    await scoped.fetch("https://nanocodex.internal/v1/responses", { method, headers });
    expect(model.fetch.mock.calls[0]![0].headers.get(regionHeader)).toBe("wnam");
    colo = "ZZZ";
    await scoped.fetch("https://nanocodex.internal/v1/responses", { method, headers });
    expect(model.fetch.mock.calls[1]![0].headers.has(regionHeader)).toBe(false);
    currentOwner = undefined;
    expect(() => scoped.fetch("https://nanocodex.internal/v1/responses", { method, headers })).toThrow(/ownership is unavailable/);
    expect(model.fetch).toHaveBeenCalledTimes(2);
    expect(ownerLookup).toHaveBeenCalledTimes(3);
    await scoped.fetch("https://nanocodex.internal/v1/search", { method: "POST", headers });
    expect(general.fetch.mock.calls[0]![0].headers.has(regionHeader)).toBe(false);
  });
  it("strips runtime placement on both legacy subjects and private bindings without a trusted callback", async () => {
    for (const privateBinding of [false, true]) {
      const binding = { fetch: vi.fn(async (_request: Request) => new Response(null, { status: 204 })) };
      const fetcher = binding as unknown as Fetcher;
      const scoped = scopedManagedModelEgress(fetcher, storageId,
        privateBinding ? managedCredentialSubject(storageId) : storageId,
        privateBinding ? { binding: fetcher, owner: () => owner } : undefined);
      await scoped.fetch("https://nanocodex.internal/v1/responses", { headers: {
        "x-nanocodex-subject": storageId, [regionHeader]: "wnam",
      } });
      expect(binding.fetch.mock.calls[0]![0].headers.has(regionHeader)).toBe(false);
    }
  });
});
