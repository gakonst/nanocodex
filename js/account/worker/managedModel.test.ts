import assert from "node:assert/strict";
import test from "node:test";

import {
  managedModelStatus,
  resetManagedSponsoredTrial,
  type ManagedModelAccess,
} from "./managedModel.ts";

test("preserves the sponsored source while disabling shared-account voice", async () => {
  const sponsoredAccess = access({
    ready: true,
    active: "chatgpt",
    free_prompts_remaining: 3,
    source: "sponsored",
    astra_entitled: true,
  });
  const sponsored = await managedModelStatus(sponsoredAccess.access);
  assert.equal(sponsoredAccess.requestedUrl(), "https://broker.internal/.well-known/nanocodex/model-status");
  assert.deepEqual(sponsored, {
    astraEntitled: false,
    freePromptsRemaining: 3,
    ready: true,
    source: "sponsored",
    voiceEnabled: false,
  });

  const user = await managedModelStatus(access({
    ready: true,
    active: "chatgpt",
    source: "user",
    astra_entitled: true,
  }).access);
  assert.deepEqual(user, {
    astraEntitled: true,
    freePromptsRemaining: null,
    ready: true,
    source: "brokered",
    voiceEnabled: true,
  });
});

test("fails closed when the credential source is absent or malformed", async () => {
  for (const value of [
    { ready: true, active: "chatgpt" },
    { ready: true, active: "openai", source: "sponsored-owner-id" },
  ]) {
    assert.deepEqual(await managedModelStatus(access(value).access), {
      astraEntitled: false,
      freePromptsRemaining: null,
      ready: false,
      source: null,
      voiceEnabled: false,
    });
  }
});

test("uses the private local-only trial reset boundary", async () => {
  const target = access({ free_prompts_remaining: 3 });
  const response = await resetManagedSponsoredTrial(target.access);
  assert.equal(response.status, 200);
  assert.equal(target.requestedUrl(), "https://broker.internal/.well-known/nanocodex/sponsored-trial-reset");
  assert.equal(target.requestedMethod(), "POST");
});

function access(body: unknown): Readonly<{
  access: ManagedModelAccess;
  requestedMethod(): string | undefined;
  requestedUrl(): string | undefined;
}> {
  let requestedMethod: string | undefined;
  let requestedUrl: string | undefined;
  return {
    access: {
    binding: {
      fetch: async (request: RequestInfo | URL) => {
        const received = new Request(request);
        requestedMethod = received.method;
        requestedUrl = received.url;
        return Response.json(body, {
          headers: { "cache-control": "no-store" },
        });
      },
    } as unknown as Fetcher,
    },
    requestedMethod: () => requestedMethod,
    requestedUrl: () => requestedUrl,
  };
}
