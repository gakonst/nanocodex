import { env } from "cloudflare:workers";
import {
  runDurableObjectAlarm,
  runInDurableObject,
  SELF,
} from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  AgentSubjectDirectory,
  finishRateLimitedRefresh,
  retryAfterDelayMs,
  type UserCredentialBroker,
} from "../src/broker";
import type { UserConnectorBroker } from "../src/connector-broker";
import { CredentialVault, type EncryptedEnvelope } from "../src/credential-vault";
import { handleEgress, type EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;
const subjectA = "A".repeat(43);
const subjectB = "B".repeat(43);
const localBootstrapExpiry = 4_102_444_800_000;

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("per-user credential broker", () => {
  it("bounds provider retry-after values for durable refresh scheduling", () => {
    const now = Date.parse("2026-08-24T00:00:00Z");
    expect(retryAfterDelayMs("120", now)).toBe(120_000);
    expect(retryAfterDelayMs("Mon, 24 Aug 2026 00:03:00 GMT", now)).toBe(180_000);
    expect(retryAfterDelayMs("invalid", now, 1, 0)).toBe(30_000);
    expect(retryAfterDelayMs("-5", now, 1, 0)).toBe(30_000);
    expect(retryAfterDelayMs(null, now, 2, 0.5)).toBe(90_000);
    expect(retryAfterDelayMs(null, now, 5, 1)).toBe(15 * 60_000);
    expect(retryAfterDelayMs("999999", now)).toBe(15 * 60_000);
    expect(retryAfterDelayMs("0", now)).toBe(1_000);
  });

  it("persists provider backoff before normal body cancellation and retries from its alarm", async () => {
    const startedAt = localBootstrapExpiry - 4 * 60_000;
    let now = startedAt;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    vi.spyOn(Math, "random").mockReturnValue(0.5);
    let cancellations = 0;
    const provider = vi.spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(rateLimitedResponse("120", () => { cancellations += 1; }))
      .mockResolvedValueOnce(rateLimitedResponse(null, () => { cancellations += 1; }))
      .mockImplementationOnce(async () => refreshedTokens(startedAt + 60 * 60_000));
    const stub = workerEnv.USER_CREDENTIALS.getByName("user-refresh-normal-cancel");

    expect((await claimLocalCredential(stub)).status).toBe(200);
    expect(await credential(stub)).toMatchObject({ status: 200, body: {
      kind: "chatgpt",
      revision: 0,
    } });
    expect(cancellations).toBe(1);
    expect(provider).toHaveBeenCalledTimes(1);
    const firstDeadline = startedAt + 120_000;
    expect(await durableAlarm(stub)).toBe(firstDeadline);
    expect(await durableRefreshState(stub)).toMatchObject({
      refreshAfter: firstDeadline,
      refreshAttempts: 1,
      refreshState: "ready",
    });

    expect(await credential(stub)).toMatchObject({ status: 200, body: {
      kind: "chatgpt",
      revision: 0,
    } });
    expect(await credential(stub, true, 0)).toMatchObject({ status: 503, body: {
      error: "chatgpt_refresh_rate_limited",
    } });
    expect(provider).toHaveBeenCalledTimes(1);
    expect(await durableAlarm(stub)).toBe(firstDeadline);

    now = firstDeadline;
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(cancellations).toBe(2);
    expect(provider).toHaveBeenCalledTimes(2);
    const secondDeadline = firstDeadline + 90_000;
    expect(await durableAlarm(stub)).toBe(secondDeadline);
    expect(await durableRefreshState(stub)).toMatchObject({
      refreshAfter: secondDeadline,
      refreshAttempts: 2,
    });

    now = secondDeadline - 1;
    expect(await credential(stub)).toMatchObject({ status: 200, body: { revision: 0 } });
    expect(await credential(stub, true, 0)).toMatchObject({ status: 503 });
    expect(provider).toHaveBeenCalledTimes(2);

    now = secondDeadline;
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(provider).toHaveBeenCalledTimes(3);
    expect(await credential(stub)).toMatchObject({ status: 200, body: { revision: 1 } });
    expect((await durableAlarm(stub))!).toBeGreaterThan(secondDeadline);
  });

  it("keeps rejecting body cancellation from restoring the near-expiry alarm", async () => {
    const startedAt = localBootstrapExpiry - 4 * 60_000;
    let now = startedAt;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    let cancellations = 0;
    const provider = vi.spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(rateLimitedResponse("300", () => {
        cancellations += 1;
        throw new Error("cancel rejected");
      }))
      .mockImplementationOnce(async () => refreshedTokens(startedAt + 60 * 60_000));
    const stub = workerEnv.USER_CREDENTIALS.getByName("user-refresh-rejecting-cancel");

    expect((await claimLocalCredential(stub)).status).toBe(200);
    expect(await durableAlarm(stub)).toBe(startedAt + 1_000);
    expect(await credential(stub)).toMatchObject({ status: 200, body: {
      kind: "chatgpt",
      revision: 0,
    } });
    expect(cancellations).toBe(1);
    const deadline = startedAt + 300_000;
    expect(await durableAlarm(stub)).toBe(deadline);
    expect(await durableRefreshState(stub)).toMatchObject({
      refreshAfter: deadline,
      refreshAttempts: 1,
      refreshState: "ready",
    });

    expect(await durableAlarm(stub)).toBe(deadline);
    expect(await credential(stub)).toMatchObject({ status: 200, body: { revision: 0 } });
    expect(await credential(stub, true, 0)).toMatchObject({ status: 503, body: {
      error: "chatgpt_refresh_rate_limited",
    } });
    expect(provider).toHaveBeenCalledTimes(1);

    now = localBootstrapExpiry;
    expect(await credential(stub)).toMatchObject({ status: 503, body: {
      error: "chatgpt_refresh_rate_limited",
    } });
    expect(provider).toHaveBeenCalledTimes(1);

    now = deadline;
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    expect(provider).toHaveBeenCalledTimes(2);
    expect(await credential(stub)).toMatchObject({ status: 200, body: { revision: 1 } });
  });

  it("quarantines an in-flight refresh restored after persistence fails in the same isolate", async () => {
    const startedAt = localBootstrapExpiry - 4 * 60_000;
    vi.spyOn(Date, "now").mockReturnValue(startedAt);
    const provider = vi.spyOn(globalThis, "fetch")
      .mockImplementation(async () => refreshedTokens(startedAt + 60 * 60_000));
    const stub = workerEnv.USER_CREDENTIALS.getByName("user-refresh-restore-quarantine");

    expect((await claimLocalCredential(stub)).status).toBe(200);
    const results = await runInDurableObject(
      stub,
      async (instance: UserCredentialBroker, state) => {
        const durablePut = state.storage.put.bind(state.storage);
        let writes = 0;
        const putSpy = vi.spyOn(state.storage, "put").mockImplementation((async (
          key: string,
          value: unknown,
        ) => {
          writes += 1;
          if (writes > 1) throw new Error("injected persistence failure");
          return durablePut(key, value);
        }) as typeof state.storage.put);
        try {
          const first = await credential(instance);
          const second = await credential(instance);
          return { first, second, writes };
        } finally {
          putSpy.mockRestore();
        }
      },
    );

    expect(results).toEqual({
      first: { status: 503, body: { error: "credential_broker_failed" } },
      second: { status: 422, body: { error: "chatgpt_credential_dead" } },
      writes: 3,
    });
    expect(await durableRefreshState(stub)).toMatchObject({ refreshState: "in_flight" });
    expect(provider).toHaveBeenCalledOnce();
  });

  it.each(["persistence", "alarm"] as const)(
    "finally cancels a 429 response body when %s fails",
    async (failure) => {
      let cancellations = 0;
      const response = rateLimitedResponse("60", () => { cancellations += 1; });
      const persist = vi.fn(async () => {
        if (failure === "persistence") throw new Error("injected persistence failure");
      });
      const schedule = vi.fn(async () => {
        if (failure === "alarm") throw new Error("injected alarm failure");
      });

      await expect(finishRateLimitedRefresh(response, persist, schedule)).rejects.toThrow(
        `injected ${failure} failure`,
      );
      expect(cancellations).toBe(1);
      expect(persist).toHaveBeenCalledOnce();
      expect(schedule).toHaveBeenCalledTimes(failure === "persistence" ? 0 : 1);
    },
  );

  it("disposes every device-login non-success body without skipping successful reads", async () => {
    let now = Date.parse("2026-08-24T00:00:00Z");
    vi.spyOn(Date, "now").mockImplementation(() => now);
    const startFailure = responseActivity();
    const startSuccess = responseActivity();
    const pendingPoll = responseActivity();
    const pollFailure = responseActivity();
    const firstCode = responseActivity();
    const exchangeFailure = responseActivity();
    const secondCode = responseActivity();
    const exchangeSuccess = responseActivity();
    const authorization = {
      authorization_code: "authorization-code",
      code_challenge: "authorization-challenge",
      code_verifier: "authorization-verifier",
    };
    const provider = vi.spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(streamedResponse("start failed", 500, startFailure, true))
      .mockResolvedValueOnce(streamedJsonResponse({
        device_auth_id: "device-id",
        user_code: "ABCD-EFGH",
        interval: 1,
      }, 200, startSuccess))
      .mockResolvedValueOnce(streamedResponse("authorization pending", 403, pendingPoll, true))
      .mockResolvedValueOnce(streamedResponse("poll failed", 500, pollFailure, true))
      .mockResolvedValueOnce(streamedJsonResponse(authorization, 200, firstCode))
      .mockResolvedValueOnce(streamedResponse("exchange failed", 500, exchangeFailure, true))
      .mockResolvedValueOnce(streamedJsonResponse(authorization, 200, secondCode))
      .mockResolvedValueOnce(streamedJsonResponse({
        access_token: testJwt({ exp: Math.floor(localBootstrapExpiry / 1_000) }),
        refresh_token: "login-refresh-token",
        id_token: testJwt({
          "https://api.openai.com/auth": {
            chatgpt_account_id: "login-account",
            chatgpt_account_is_fedramp: false,
          },
        }),
      }, 200, exchangeSuccess));
    const stub = workerEnv.USER_CREDENTIALS.getByName("user-device-response-lifecycle");

    expect(await brokerCall(stub, "/v1/chatgpt/login/start")).toMatchObject({
      status: 503,
      body: { error: "chatgpt_login_start_failed" },
    });
    expect(startFailure).toEqual({ pulls: 0, cancellations: 1 });

    expect(await brokerCall(stub, "/v1/chatgpt/login/start")).toMatchObject({
      status: 200,
      body: { state: "pending", user_code: "ABCD-EFGH" },
    });
    expect(startSuccess.pulls).toBeGreaterThan(0);
    expect(startSuccess.cancellations).toBe(0);

    expect(await brokerCall(stub, "/v1/chatgpt/login/status")).toMatchObject({
      status: 200,
      body: { state: "pending" },
    });
    expect(pendingPoll).toEqual({ pulls: 0, cancellations: 1 });

    now += 1_000;
    expect(await brokerCall(stub, "/v1/chatgpt/login/status")).toMatchObject({
      status: 503,
      body: { error: "chatgpt_login_poll_failed" },
    });
    expect(pollFailure).toEqual({ pulls: 0, cancellations: 1 });

    expect(await brokerCall(stub, "/v1/chatgpt/login/status")).toMatchObject({
      status: 503,
      body: { error: "chatgpt_token_exchange_failed" },
    });
    expect(firstCode.pulls).toBeGreaterThan(0);
    expect(firstCode.cancellations).toBe(0);
    expect(exchangeFailure).toEqual({ pulls: 0, cancellations: 1 });

    expect(await brokerCall(stub, "/v1/chatgpt/login/status")).toMatchObject({
      status: 200,
      body: { state: "authenticated", account_id: "login-account" },
    });
    expect(secondCode.pulls).toBeGreaterThan(0);
    expect(secondCode.cancellations).toBe(0);
    expect(exchangeSuccess.pulls).toBeGreaterThan(0);
    expect(exchangeSuccess.cancellations).toBe(0);
    expect(provider).toHaveBeenCalledTimes(8);
  });

  it("absorbs a rejecting 401 body cancellation and completes ChatGPT recovery", async () => {
    const subject = "Y".repeat(43);
    const credentialRequests: Array<{ recover: boolean; revision?: number }> = [];
    const first = responseActivity();
    const recovered = responseActivity();
    const upstream = vi.fn(async (request: RequestInfo | URL, init?: RequestInit) => {
      const outbound = request instanceof Request ? request : new Request(request, init);
      if (upstream.mock.calls.length === 1) {
        expect(outbound.headers.get("authorization")).toBe("Bearer stale-access");
        return streamedResponse("unauthorized", 401, first, true);
      }
      expect(outbound.headers.get("authorization")).toBe("Bearer recovered-access");
      return streamedJsonResponse({ recovered: true }, 200, recovered);
    });
    const response = await handleEgress(
      modelRequest(subject),
      responseLifecycleEnv(subject, credentialRequests, (recover) => ({
        kind: "chatgpt",
        secret: recover ? "recovered-access" : "stale-access",
        accountId: "chatgpt-account",
        fedramp: false,
        expiresAt: localBootstrapExpiry,
        revision: recover ? 1 : 0,
      })),
      undefined,
      upstream,
    );

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ recovered: true });
    expect(first).toEqual({ pulls: 0, cancellations: 1 });
    expect(recovered.pulls).toBeGreaterThan(0);
    expect(recovered.cancellations).toBe(0);
    expect(credentialRequests).toEqual([
      { recover: false },
      { recover: true, revision: 0 },
    ]);
    expect(upstream).toHaveBeenCalledTimes(2);
  });

  it("keeps 429 protocol classification when response cancellation rejects", async () => {
    const subject = "Z".repeat(43);
    const credentialRequests: Array<{ recover: boolean; revision?: number }> = [];
    const rejected = responseActivity();
    const upstreamException = vi.fn();
    const response = await handleEgress(
      modelRequest(subject),
      responseLifecycleEnv(subject, credentialRequests, () => ({
        kind: "chatgpt",
        secret: "rate-limited-access",
        accountId: "chatgpt-account",
        fedramp: false,
        expiresAt: localBootstrapExpiry,
        revision: 0,
      })),
      undefined,
      async () => streamedResponse("rate limited", 429, rejected, true),
      { upstreamException },
    );

    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({ error: "upstream_rejected" });
    expect(rejected).toEqual({ pulls: 0, cancellations: 1 });
    expect(credentialRequests).toEqual([{ recover: false }]);
    expect(upstreamException).not.toHaveBeenCalled();
  });

  it("default-denies anything except exact private control and model routes", async () => {
    expect((await SELF.fetch("https://example.test/")).status).toBe(403);
    expect((await SELF.fetch("https://nanocodex.internal/v1/responses")).status).toBe(403);
    expect((await SELF.fetch("https://nanocodex.internal/v1/responses?escape=true")).status).toBe(403);
    expect((await SELF.fetch("http://nanocodex.internal/v1/responses")).status).toBe(403);
    expect((await SELF.fetch("https://nanocodex.internal/v1/responses/other")).status).toBe(403);
    expect((await SELF.fetch("https://example.test/users/user-1/credentials/other")).status).toBe(404);
  });

  it("binds an opaque subject to exactly one user and unbinds only by owner", async () => {
    const subject = "C".repeat(43);
    const bound = await control(`/subjects/${subject}`, "PUT", { user_id: "user-bind-a" });
    expect(bound.status).toBe(200);
    expect(await bound.json()).toEqual({ status: "bound" });

    const idempotent = await control(`/subjects/${subject}`, "PUT", { user_id: "user-bind-a" });
    expect(await idempotent.json()).toEqual({ status: "unchanged" });
    expect((await control(`/subjects/${subject}`, "PUT", { user_id: "user-bind-b" })).status)
      .toBe(409);
    expect((await control(`/subjects/${subject}`, "DELETE", { user_id: "user-bind-b" })).status)
      .toBe(409);
    expect((await control(`/subjects/${subject}`, "DELETE", { user_id: "user-bind-a" })).status)
      .toBe(204);
    expect((await control(`/subjects/${subject}`, "PUT", { user_id: "user-bind-a" })).status)
      .toBe(410);
    expect((await control(`/subjects/${subject}`, "DELETE", { user_id: "user-bind-a" })).status)
      .toBe(204);
  });

  it("isolates subject authority across independently named objects", async () => {
    const subjectD = "Q".repeat(43);
    const subjectE = "V".repeat(43);
    const shardD = workerEnv.AGENT_SUBJECTS.getByName(`agent-subject-v1:${subjectD}`);
    const shardE = workerEnv.AGENT_SUBJECTS.getByName(`agent-subject-v1:${subjectE}`);

    expect((await control(`/subjects/${subjectD}`, "PUT", { user_id: "user-d" })).status)
      .toBe(200);
    expect((await control(`/subjects/${subjectE}`, "PUT", { user_id: "user-e" })).status)
      .toBe(200);
    expect(await subjectOwner(shardD, subjectD)).toBe("user-d");
    expect(await subjectOwner(shardE, subjectE)).toBe("user-e");

    expect((await control(`/subjects/${subjectD}`, "DELETE", { user_id: "user-d" })).status)
      .toBe(204);
    expect(await subjectOwner(shardD, subjectD)).toBeUndefined();
    expect(await subjectOwner(shardE, subjectE)).toBe("user-e");
    expect((await control(`/subjects/${subjectD}`, "PUT", { user_id: "user-d" })).status)
      .toBe(410);
  });

  it("rejects a body subject that does not match the named authority", async () => {
    const subject = "M".repeat(43);
    const other = "N".repeat(43);
    const shard = workerEnv.AGENT_SUBJECTS.getByName(`agent-subject-v1:${subject}`);
    const response = await shard.fetch("https://subjects.internal/v1/bind", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ subject: other, user_id: "user-mismatch" }),
    });
    expect(response.status).toBe(400);
    expect(await subjectOwner(shard, subject)).toBeUndefined();
    expect(await subjectOwner(shard, other)).toBeUndefined();
  });

  it("stores per-user OpenAI keys, exposes only status, and injects after subject resolution", async () => {
    await control(`/subjects/${subjectA}`, "PUT", { user_id: "user-openai-a" });
    const stored = await control("/users/user-openai-a/credentials/openai", "PUT", {
      api_key: "sk-user-a-secret",
    });
    expect(stored.status).toBe(204);

    const status = await SELF.fetch("https://broker.test/users/user-openai-a/credentials");
    expect(status.status).toBe(200);
    const publicStatus = await status.json<Record<string, unknown>>();
    expect(publicStatus).toMatchObject({
      ready: true,
      active: "openai",
      openai: { connected: true },
      chatgpt: { connected: false },
    });
    expect(JSON.stringify(publicStatus)).not.toContain("sk-user-a-secret");

    const response = await SELF.fetch(modelRequest(subjectA));
    expect(response.status).toBe(200);
    expect(response.headers.get("authorization")).toBeNull();
    expect(await response.json()).toEqual({
      url: "https://api.openai.com/v1/alpha/search",
      credential: "openai-a",
      account: null,
      subject: null,
      leaked: null,
    });
  });

  it("isolates two users that use the same fixed model URL", async () => {
    await control(`/subjects/${subjectB}`, "PUT", { user_id: "user-openai-b" });
    await control("/users/user-openai-b/credentials/openai", "PUT", {
      api_key: "sk-user-b-secret",
    });
    const a = await SELF.fetch(modelRequest(subjectA));
    const b = await SELF.fetch(modelRequest(subjectB));
    expect((await a.json() as { credential: string }).credential).toBe("openai-a");
    expect((await b.json() as { credential: string }).credential).toBe("openai-b");
  });

  it("rewrites the exact fixed Responses WebSocket endpoint after all checks", async () => {
    const subject = "W".repeat(64);
    await control(`/subjects/${subject}`, "PUT", { user_id: "user-websocket" });
    await control("/users/user-websocket/credentials/openai", "PUT", {
      api_key: "sk-websocket-secret",
    });
    let observed: Request | undefined;
    const response = await handleEgress(
      new Request("https://nanocodex.internal/v1/responses", {
        headers: {
          authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
          "openai-beta": "responses_websockets=2026-02-06",
          upgrade: "websocket",
          "x-nanocodex-subject": subject,
          "x-should-not-forward": "blocked",
        },
      }),
      workerEnv,
      undefined,
      async (input, init) => {
        observed = input instanceof Request ? input : new Request(input, init);
        return Response.json({ ok: true });
      },
    );
    expect(response.status).toBe(200);
    expect(observed?.url).toBe("https://api.openai.com/v1/responses");
    expect(observed?.headers.get("authorization")).toBe("Bearer sk-websocket-secret");
    expect(observed?.headers.get("x-nanocodex-subject")).toBeNull();
    expect(observed?.headers.get("x-should-not-forward")).toBeNull();
  });

  it("runs ChatGPT device login server-side without returning tokens", async () => {
    const subject = "D".repeat(43);
    await control(`/subjects/${subject}`, "PUT", { user_id: "user-chatgpt" });
    const started = await SELF.fetch(
      "https://broker.test/users/user-chatgpt/credentials/chatgpt/login",
      { method: "POST" },
    );
    expect(started.status).toBe(200);
    const pending = await started.json<Record<string, unknown>>();
    expect(pending).toMatchObject({ state: "pending", user_code: "ABCD-EFGH" });
    expect(JSON.stringify(pending)).not.toContain("device-secret");

    const completed = await SELF.fetch(
      "https://broker.test/users/user-chatgpt/credentials/chatgpt/login/status",
      { method: "POST" },
    );
    expect(completed.status).toBe(200);
    expect(await completed.json()).toEqual({
      state: "authenticated",
      account_id: "chatgpt-account",
    });

    const response = await SELF.fetch(modelRequest(subject));
    expect(response.status).toBe(200);
    const upstream = await response.json<Record<string, unknown>>();
    expect(upstream.url).toBe("https://chatgpt.com/backend-api/codex/alpha/search");
    expect(upstream.account).toBe("chatgpt-account");
    expect(upstream.credential).toBe("chatgpt");
    expect(upstream.subject).toBeNull();
  });

  it("uses only the transport relay for ChatGPT and keeps the credential server-side", async () => {
    const subject = "R".repeat(43);
    await control(`/subjects/${subject}`, "PUT", { user_id: "user-chatgpt-relay" });
    await SELF.fetch("https://broker.test/users/user-chatgpt-relay/credentials/chatgpt/login", {
      method: "POST",
    });
    await SELF.fetch(
      "https://broker.test/users/user-chatgpt-relay/credentials/chatgpt/login/status",
      { method: "POST" },
    );

    let localRelayRequest: Request | undefined;
    const relayed = await handleEgress(
      modelRequest(subject),
      {
        ...workerEnv,
        CODEX_RELAY_URL: "http://127.0.0.1:49152/",
        ALLOW_INSECURE_LOOPBACK_RELAY: "true",
      },
      undefined,
      async (input, init) => {
        localRelayRequest = input instanceof Request ? input : new Request(input, init);
        return Response.json({ ok: true });
      },
    );
    expect(relayed.status).toBe(200);
    expect(localRelayRequest?.url).toBe(
      "http://127.0.0.1:49152/backend-api/codex/alpha/search",
    );
    expect(localRelayRequest?.headers.get("authorization")).toMatch(/^Bearer [^.]+\.[^.]+\.[^.]+$/);
    expect(localRelayRequest?.headers.get("x-nanocodex-subject")).toBeNull();

    const capability = "C".repeat(43);
    let capabilityRequest: Request | undefined;
    const throughCapability = await handleEgress(
      modelRequest(subject),
      {
        ...workerEnv,
        CODEX_RELAY_URL: `http://127.0.0.1:49152/v1/${capability}`,
        ALLOW_INSECURE_LOOPBACK_RELAY: "true",
      },
      undefined,
      async (input, init) => {
        capabilityRequest = input instanceof Request ? input : new Request(input, init);
        return Response.json({ ok: true });
      },
    );
    expect(throughCapability.status).toBe(200);
    expect(capabilityRequest?.url).toBe(
      `http://127.0.0.1:49152/v1/${capability}/http/codex-web-search`,
    );

    let capabilitySocketRequest: Request | undefined;
    const throughCapabilitySocket = await handleEgress(
      new Request("https://nanocodex.internal/v1/responses", {
        headers: {
          authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
          "openai-beta": "responses_websockets=2026-02-06",
          upgrade: "websocket",
          "x-nanocodex-subject": subject,
        },
      }),
      {
        ...workerEnv,
        CODEX_RELAY_URL: `http://127.0.0.1:49152/v1/${capability}`,
        ALLOW_INSECURE_LOOPBACK_RELAY: "true",
      },
      undefined,
      async (input, init) => {
        capabilitySocketRequest = input instanceof Request ? input : new Request(input, init);
        return Response.json({ ok: true });
      },
    );
    expect(throughCapabilitySocket.status).toBe(200);
    expect(capabilitySocketRequest?.url).toBe(
      `http://127.0.0.1:49152/v1/${capability}`,
    );

    let containerRequest: Request | undefined;
    let containerName: string | undefined;
    const throughContainer = await handleEgress(
      modelRequest(subject),
      {
        ...workerEnv,
        ENVIRONMENT: "production",
        CHATGPT_EGRESS: {
          idFromName(name: string) {
            containerName = name;
            return {} as DurableObjectId;
          },
          get() {
            return {
              async fetch(request: Request) {
                containerRequest = request;
                return Response.json({ ok: true });
              },
            };
          },
        } as unknown as DurableObjectNamespace,
      },
      undefined,
      async () => { throw new Error("production ChatGPT must not use global fetch"); },
    );
    expect(throughContainer.status).toBe(200);
    expect(containerName).toBe("user-v1:user-chatgpt-relay");
    expect(containerRequest?.url).toBe(
      "https://chatgpt-egress.internal/backend-api/codex/alpha/search",
    );
    expect(containerRequest?.headers.get("authorization")).toBe(
      localRelayRequest?.headers.get("authorization"),
    );
    expect(containerRequest?.headers.get("x-nanocodex-subject")).toBeNull();

    let realtimeCall: Request | undefined;
    const call = await handleEgress(
      realtimeRequest(subject, "/v1/realtime/calls", {
        method: "POST",
        body: JSON.stringify({ sdp: "v=0", session: { delegation: { type: "client" } } }),
      }),
      {
        ...workerEnv,
        CODEX_RELAY_URL: "http://127.0.0.1:49152/",
        ALLOW_INSECURE_LOOPBACK_RELAY: "true",
      },
      undefined,
      async (input, init) => {
        realtimeCall = input instanceof Request ? input : new Request(input, init);
        return new Response("v=0", {
          status: 201,
          headers: { location: "/backend-api/codex/realtime/calls/rtc_test" },
        });
      },
    );
    expect(call.status).toBe(201);
    expect(realtimeCall?.url).toBe(
      "http://127.0.0.1:49152/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
    );
    expect(realtimeCall?.headers.get("openai-alpha")).toBe("quicksilver=v2");
    expect(realtimeCall?.headers.get("x-oai-attestation")).toBe('{"v":1,"s":1}');
    expect(realtimeCall?.headers.get("originator")).toBeNull();
    expect(realtimeCall?.headers.get("x-session-id")).toBe("realtime-session");
    expect(realtimeCall?.headers.get("session-id")).toBe("lifecycle-session");
    expect(realtimeCall?.headers.get("thread-id")).toBe("thread-session");
    expect(realtimeCall?.headers.get("chatgpt-account-id")).toBe("chatgpt-account");
    expect(realtimeCall?.headers.get("x-nanocodex-subject")).toBeNull();

    let realtimeSideband: Request | undefined;
    const sideband = await handleEgress(
      realtimeRequest(subject, "/v1/realtime/sideband", {
        callId: "rtc_test",
        method: "GET",
      }),
      {
        ...workerEnv,
        ENVIRONMENT: "production",
        CHATGPT_EGRESS: {
          idFromName() { throw new Error("Realtime sideband must use its fixed direct host"); },
        } as unknown as DurableObjectNamespace,
      },
      undefined,
      async (input, init) => {
        realtimeSideband = input instanceof Request ? input : new Request(input, init);
        return Response.json({ ok: true });
      },
    );
    expect(sideband.status).toBe(200);
    expect(realtimeSideband?.url).toBe("https://api.openai.com/v1/live/rtc_test");
    expect(realtimeSideband?.headers.get("upgrade")).toBe("websocket");
    expect(realtimeSideband?.headers.get("x-oai-attestation")).toBe('{"v":1,"s":1}');
    expect(realtimeSideband?.headers.get("originator")).toBeNull();
    expect(realtimeSideband?.headers.get("x-session-id")).toBe("realtime-session");
    expect(realtimeSideband?.headers.get("session-id")).toBe("lifecycle-session");
    expect(realtimeSideband?.headers.get("thread-id")).toBe("thread-session");
  });

  it("rejects Realtime before an OpenAI key can reach the ChatGPT-only endpoint", async () => {
    const response = await handleEgress(
      realtimeRequest(subjectA, "/v1/realtime/calls", {
        method: "POST",
        body: JSON.stringify({ sdp: "v=0" }),
      }),
      workerEnv,
      undefined,
      async () => { throw new Error("OpenAI credentials must not reach ChatGPT Realtime"); },
    );
    expect(response.status).toBe(409);
    expect(await response.json()).toEqual({ error: "chatgpt_credential_required" });
  });

  it("encrypts all provider and pending-login material in Durable Object storage", async () => {
    const stub = workerEnv.USER_CREDENTIALS.getByName("user-chatgpt");
    await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
      const row = await state.storage.get("credential-state");
      const encoded = JSON.stringify(row);
      for (const forbidden of [
        "chatgpt-access",
        "chatgpt-refresh-secret",
        "device-secret",
        "authorization-secret",
        "verifier-secret",
      ]) expect(encoded).not.toContain(forbidden);
      expect(encoded).toContain("ciphertext");
    });
  });

  it("provides the local bootstrap to every development account without exposing it", async () => {
    const failed = await handleEgress(
      new Request("https://broker.test/users/failed-local-user/credentials/chatgpt/local-claim", {
        method: "POST",
      }),
      {
        ...workerEnv,
        USER_CREDENTIALS: {
          getByName: () => ({
            fetch: async () => Response.json(
              { error: "local_chatgpt_bootstrap_unavailable" },
              { status: 503 },
            ),
          }),
        } as unknown as EgressEnv["USER_CREDENTIALS"],
      },
    );
    expect(failed.status).toBe(503);

    const claim = await SELF.fetch(
      "https://broker.test/users/local-user/credentials/chatgpt/local-claim",
      { method: "POST" },
    );
    expect(claim.status).toBe(200);
    const status = await claim.json<Record<string, unknown>>();
    expect(status).toMatchObject({ active: "chatgpt", chatgpt: { connected: true } });
    expect(JSON.stringify(status)).not.toContain("local-access");
    const otherClaim = await SELF.fetch(
      "https://broker.test/users/other-local-user/credentials/chatgpt/local-claim",
      { method: "POST" },
    );
    expect(otherClaim.status).toBe(200);
    const otherStatus = await otherClaim.json<Record<string, unknown>>();
    expect(otherStatus).toMatchObject({ active: "chatgpt", chatgpt: { connected: true } });
    expect(JSON.stringify(otherStatus)).not.toContain("local-access");
  });

  it("claims the local bootstrap atomically only when ChatGPT is missing or dead", async () => {
    const stub = workerEnv.USER_CREDENTIALS.getByName("user-local-claim-missing-only");
    const claims = await Promise.all([
      claimLocalCredential(stub),
      claimLocalCredential(stub),
    ]);
    const publicStatuses = await Promise.all(
      claims.map(async (response) => ({
        status: response.status,
        body: await response.json<Record<string, unknown>>(),
      })),
    );
    expect(publicStatuses).toEqual([
      expect.objectContaining({
        status: 200,
        body: expect.objectContaining({
          active: "chatgpt",
          chatgpt: expect.objectContaining({ connected: true, account_id: "local-account" }),
        }),
      }),
      expect.objectContaining({
        status: 200,
        body: expect.objectContaining({
          active: "chatgpt",
          chatgpt: expect.objectContaining({ connected: true, account_id: "local-account" }),
        }),
      }),
    ]);
    expect(JSON.stringify(publicStatuses)).not.toMatch(/local-access|local-refresh-secret/);
    expect(await credential(stub)).toMatchObject({
      status: 200,
      body: { kind: "chatgpt", revision: 0 },
    });

    let now = localBootstrapExpiry - 4 * 60_000;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    const provider = vi.spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(null, { status: 401 }));
    expect(await credential(stub)).toEqual({
      status: 422,
      body: { error: "chatgpt_credential_dead" },
    });
    expect(provider).toHaveBeenCalledOnce();

    const replacement = await claimLocalCredential(stub);
    expect(replacement.status).toBe(200);
    const replacementStatus = await replacement.json<Record<string, unknown>>();
    expect(replacementStatus).toMatchObject({
      active: "chatgpt",
      chatgpt: { connected: true, account_id: "local-account" },
    });
    expect(JSON.stringify(replacementStatus)).not.toMatch(/local-access|local-refresh-secret/);
    now = Date.parse("2026-08-26T00:00:00Z");
    expect(await credential(stub)).toMatchObject({
      status: 200,
      body: { kind: "chatgpt", revision: 1 },
    });
  });

  it("checks the local gate before retaining a healthy claim without parsing bootstrap", async () => {
    const stub = workerEnv.USER_CREDENTIALS.getByName("user-local-claim-retained");
    expect((await claimLocalCredential(stub)).status).toBe(200);

    const retained = await runInDurableObject(
      stub,
      async (instance: UserCredentialBroker) => {
        const brokerEnv = (instance as unknown as { env: EgressEnv }).env;
        const originalEnvironment = brokerEnv.ENVIRONMENT;
        const originalBootstrap = brokerEnv.LOCAL_CHATGPT_BOOTSTRAP;
        try {
          brokerEnv.LOCAL_CHATGPT_BOOTSTRAP = "{invalid-bootstrap";
          const repeated = await claimLocalCredential(instance);
          const repeatedBody = await repeated.json<Record<string, unknown>>();
          brokerEnv.ENVIRONMENT = "production";
          const hidden = await claimLocalCredential(instance);
          return {
            repeated: { status: repeated.status, body: repeatedBody },
            hidden: {
              status: hidden.status,
              body: await hidden.json<Record<string, unknown>>(),
            },
          };
        } finally {
          if (originalEnvironment === undefined) {
            delete brokerEnv.ENVIRONMENT;
          } else {
            brokerEnv.ENVIRONMENT = originalEnvironment;
          }
          if (originalBootstrap === undefined) {
            delete brokerEnv.LOCAL_CHATGPT_BOOTSTRAP;
          } else {
            brokerEnv.LOCAL_CHATGPT_BOOTSTRAP = originalBootstrap;
          }
        }
      },
    );
    expect(retained).toEqual({
      repeated: {
        status: 200,
        body: expect.objectContaining({
          active: "chatgpt",
          chatgpt: expect.objectContaining({ connected: true, account_id: "local-account" }),
        }),
      },
      hidden: { status: 404, body: { error: "not_found" } },
    });
    expect(JSON.stringify(retained)).not.toMatch(/local-access|local-refresh-secret/);
    expect(await credential(stub)).toMatchObject({
      status: 200,
      body: { kind: "chatgpt", revision: 0 },
    });

    const missingStub = workerEnv.USER_CREDENTIALS.getByName("user-local-claim-invalid-missing");
    const failures = await runInDurableObject(
      missingStub,
      async (instance: UserCredentialBroker) => {
        const brokerEnv = (instance as unknown as { env: EgressEnv }).env;
        const originalBootstrap = brokerEnv.LOCAL_CHATGPT_BOOTSTRAP;
        try {
          brokerEnv.LOCAL_CHATGPT_BOOTSTRAP = "{invalid-bootstrap";
          const invalid = await claimLocalCredential(instance);
          delete brokerEnv.LOCAL_CHATGPT_BOOTSTRAP;
          const unavailable = await claimLocalCredential(instance);
          return {
            invalid: {
              status: invalid.status,
              body: await invalid.json<Record<string, unknown>>(),
            },
            unavailable: {
              status: unavailable.status,
              body: await unavailable.json<Record<string, unknown>>(),
            },
          };
        } finally {
          if (originalBootstrap === undefined) {
            delete brokerEnv.LOCAL_CHATGPT_BOOTSTRAP;
          } else {
            brokerEnv.LOCAL_CHATGPT_BOOTSTRAP = originalBootstrap;
          }
        }
      },
    );
    expect(failures).toEqual({
      invalid: { status: 503, body: { error: "invalid_local_chatgpt_bootstrap" } },
      unavailable: { status: 503, body: { error: "local_chatgpt_bootstrap_unavailable" } },
    });
  });

  it("hides the local bootstrap claim route outside local development", async () => {
    const response = await handleEgress(
      new Request("https://broker.test/users/production-user/credentials/chatgpt/local-claim", {
        method: "POST",
      }),
      { ...workerEnv, ENVIRONMENT: "production" },
    );
    expect(response.status).toBe(404);
    expect(await response.json()).toEqual({ error: "not_found" });
  });

  it("deletes credentials and leaves bound subjects unable to invoke the model", async () => {
    expect((await SELF.fetch("https://broker.test/users/user-openai-b/credentials/openai", {
      method: "DELETE",
    })).status).toBe(204);
    const response = await SELF.fetch(modelRequest(subjectB));
    expect(response.status).toBe(409);
    expect(await response.json()).toEqual({ error: "user_credential_unavailable" });
  });

  it("keeps the deployment readiness probe credential-independent", async () => {
    const denied = await SELF.fetch(
      "https://broker.test/.well-known/nanocodex/broker-readiness",
      { method: "POST" },
    );
    expect(denied.status).toBe(404);
    const ready = await SELF.fetch(
      "https://broker.test/.well-known/nanocodex/broker-readiness",
      {
        method: "POST",
        headers: {
          authorization: "Bearer probe-token-that-is-at-least-thirty-two-bytes",
        },
      },
    );
    expect(await ready.json()).toEqual({ ready: true });

    const emptyPayload = await SELF.fetch(
      "https://broker.test/.well-known/nanocodex/broker-readiness",
      {
        method: "POST",
        headers: {
          authorization: "Bearer probe-token-that-is-at-least-thirty-two-bytes",
        },
        body: "",
      },
    );
    expect(await emptyPayload.json()).toEqual({ ready: true });

    const streamedEmptyPost = await handleEgress(
      new Request("https://broker.test/.well-known/nanocodex/broker-readiness", {
        method: "POST",
        headers: {
          authorization: "Bearer probe-token-that-is-at-least-thirty-two-bytes",
        },
        body: new Uint8Array(),
      }),
      workerEnv,
    );
    expect(await streamedEmptyPost.json()).toEqual({ ready: true });

    const bodyRejected = await handleEgress(
      new Request("https://broker.test/.well-known/nanocodex/broker-readiness", {
        method: "POST",
        headers: {
          authorization: "Bearer probe-token-that-is-at-least-thirty-two-bytes",
        },
        body: "unexpected",
      }),
      workerEnv,
    );
    expect(bodyRejected.status).toBe(404);
    expect(await bodyRejected.json()).toEqual({ error: "not_found" });
  });

  it("stores only opaque subject mappings in the directory DO", async () => {
    const stub = workerEnv.AGENT_SUBJECTS.getByName(`agent-subject-v1:${subjectA}`);
    await runInDurableObject(stub, async (_instance: AgentSubjectDirectory, state) => {
      const mappings = await state.storage.list();
      expect(mappings.get(`subject:${subjectA}`)).toBe("user-openai-a");
      expect(JSON.stringify([...mappings])).not.toContain("sk-user-a-secret");
    });
  });
});

describe("per-user OAuth connectors", () => {
  it("accepts the fixed loopback relay used by every browser development stack", async () => {
    const started = await control("/users/connector-localhost/connectors/github", "POST", {
      redirect_uri: "http://127.0.0.1:47891/v1/connectors/github/callback",
      return_to: "/connect?thread=connector-localhost",
    });
    expect(started.status).toBe(200);
    const authorization = new URL((await started.json<{ authorization_url: string }>()).authorization_url);
    expect(authorization.searchParams.get("redirect_uri")).toBe(
      "http://127.0.0.1:47891/v1/connectors/github/callback",
    );
  });

  it("requires an HTTPS callback for WHOOP", async () => {
    const started = await control("/users/connector-whoop-localhost/connectors/whoop", "POST", {
      redirect_uri: "http://127.0.0.1:47891/v1/connectors/whoop/callback",
      return_to: "/connect?thread=connector-whoop-localhost",
    });
    expect(started.status).toBe(400);
    expect(await started.json()).toEqual({ error: "invalid_request" });
  });

  for (const connector of ["github", "gmail", "gdrive", "x", "whoop"] as const) {
    it(`completes ${connector} authorization without returning provider credentials`, async () => {
      const user = `connector-${connector}`;
      const started = await control(`/users/${user}/connectors/${connector}`, "POST", {
        redirect_uri: `https://nanocodex.test/v1/connectors/${connector}/callback`,
        return_to: "/agent?thread=connector-test",
      });
      expect(started.status).toBe(200);
      const startBody = await started.json<{ authorization_url: string }>();
      const authorization = new URL(startBody.authorization_url);
      expect(authorization.protocol).toBe("https:");
      expect(authorization.searchParams.get("state")).toMatch(/^[A-Za-z0-9_-]{43}$/);
      expect(authorization.searchParams.get("code_challenge_method"))
        .toBe(connector === "whoop" ? null : "S256");
      expect(JSON.stringify(startBody)).not.toContain("client-secret");

      const completed = await control(`/users/${user}/connectors/${connector}/callback`, "POST", {
        code: connector === "gdrive" ? "gdrive-code" : `${connector}-code`,
        state: authorization.searchParams.get("state"),
      });
      expect(completed.status).toBe(200);
      expect(await completed.json()).toEqual({
        connected: true,
        return_to: "/agent?thread=connector-test",
      });

      const status = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
      expect(status.status).toBe(200);
      const publicStatus = await status.json<Record<string, unknown>>();
      expect(publicStatus).toMatchObject({
        connectors: { [connector]: { connected: true } },
      });
      for (const secret of ["connector-access", "connector-refresh", "client-secret"]) {
        expect(JSON.stringify(publicStatus)).not.toContain(secret);
      }
    });
  }

  for (const [code, label] of [
    ["x-no-refresh-code", "refresh token"],
    ["x-reduced-scope-code", "complete scope grant"],
  ] as const) {
    it(`rejects an X authorization without its ${label}`, async () => {
      const user = `connector-${code}`;
      const started = await control(`/users/${user}/connectors/x`, "POST", {
        redirect_uri: "https://nanocodex.test/v1/connectors/x/callback",
        return_to: "/",
      });
      const authorization = new URL(
        (await started.json<{ authorization_url: string }>()).authorization_url,
      );
      const completed = await control(`/users/${user}/connectors/x/callback`, "POST", {
        code,
        state: authorization.searchParams.get("state"),
      });
      expect(completed.status).toBe(502);
      const status = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
      expect(await status.json()).toMatchObject({
        connectors: { x: { connected: false } },
      });
    });
  }

  for (const [code, label] of [
    ["whoop-no-refresh-code", "refresh token"],
    ["whoop-reduced-scope-code", "complete scope grant"],
  ] as const) {
    it(`rejects a WHOOP authorization without its ${label}`, async () => {
      const user = `connector-${code}`;
      const started = await control(`/users/${user}/connectors/whoop`, "POST", {
        redirect_uri: "https://nanocodex.test/v1/connectors/whoop/callback",
        return_to: "/",
      });
      const authorization = new URL(
        (await started.json<{ authorization_url: string }>()).authorization_url,
      );
      const completed = await control(`/users/${user}/connectors/whoop/callback`, "POST", {
        code,
        state: authorization.searchParams.get("state"),
      });
      expect(completed.status).toBe(502);
      expect(await (await SELF.fetch(
        `https://broker.test/users/${user}/connectors`,
      )).json()).toMatchObject({ connectors: { whoop: { connected: false } } });
    });
  }

  it("makes X revocation idempotent and preserves credentials only for retryable failures", async () => {
    await connect("connector-x-disconnect", "x", "x-code");
    const disconnected = await SELF.fetch(
      "https://broker.test/users/connector-x-disconnect/connectors/x",
      { method: "DELETE" },
    );
    expect(disconnected.status).toBe(204);
    expect(await (await SELF.fetch(
      "https://broker.test/users/connector-x-disconnect/connectors",
    )).json()).toMatchObject({ connectors: { x: { connected: false } } });

    for (const code of ["x-partially-revoked-code", "x-already-revoked-code"]) {
      const user = `connector-${code}`;
      await connect(user, "x", code);
      const stale = await SELF.fetch(
        `https://broker.test/users/${user}/connectors/x`,
        { method: "DELETE" },
      );
      expect(stale.status).toBe(204);
      expect(await (await SELF.fetch(
        `https://broker.test/users/${user}/connectors`,
      )).json()).toMatchObject({ connectors: { x: { connected: false } } });
    }

    for (const code of ["x-revocation-failure-code", "x-revocation-throttled-code"]) {
      const user = `connector-${code}`;
      await connect(user, "x", code);
      const failed = await SELF.fetch(
        `https://broker.test/users/${user}/connectors/x`,
        { method: "DELETE" },
      );
      expect(failed.status).toBe(503);
      expect(await failed.json()).toEqual({ error: "connector_revocation_failed" });
      expect(await (await SELF.fetch(
        `https://broker.test/users/${user}/connectors`,
      )).json()).toMatchObject({ connectors: { x: { connected: true } } });
    }
  });

  it("encrypts tokens, refresh tokens, PKCE verifiers, and OAuth state at rest", async () => {
    const stub = workerEnv.USER_CONNECTORS.getByName("connector-gdrive");
    await runInDurableObject(stub, async (_instance: UserConnectorBroker, state) => {
      const row = await state.storage.get("connector-state");
      const encoded = JSON.stringify(row);
      for (const forbidden of [
        "gdrive-connector-access",
        "gdrive-connector-refresh",
        "connector-test",
      ]) expect(encoded).not.toContain(forbidden);
      expect(encoded).toContain("ciphertext");
    });
  });

  it("consumes state once and preserves the existing connection on replay", async () => {
    const started = await control("/users/connector-replay/connectors/github", "POST", {
      redirect_uri: "https://nanocodex.test/v1/connectors/github/callback",
      return_to: "/",
    });
    const authorization = new URL((await started.json<{ authorization_url: string }>()).authorization_url);
    const callback = {
      code: "github-code",
      state: authorization.searchParams.get("state"),
    };
    expect((await control(
      "/users/connector-replay/connectors/github/callback",
      "POST",
      callback,
    )).status).toBe(200);
    const replay = await control(
      "/users/connector-replay/connectors/github/callback",
      "POST",
      callback,
    );
    expect(replay.status).toBe(400);
    expect(await replay.json()).toEqual({ error: "invalid_oauth_state" });
  });
});

describe("private connector data plane", () => {
  it("forwards provider reads and writes with server-side credentials", async () => {
    const subject = connectorSubject("data-plane");
    const user = "connector-data-plane";
    await control(`/subjects/${subject}`, "PUT", { user_id: user });
    for (const connector of ["github", "gmail", "gdrive", "x", "whoop"] as const) {
      await connect(user, connector, connector === "gdrive" ? "gdrive-code" : `${connector}-code`);
    }

    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    const errorLog = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      for (const url of [
        "https://api.github.com/repos/nanocodex/sdk?per_page=1",
        "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=1",
        "https://www.googleapis.com/drive/v3/files?pageSize=1",
        "https://api.x.com/2/dm_events?max_results=5",
        "https://api.prod.whoop.com/developer/v2/recovery?limit=1",
      ]) {
        const response = await SELF.fetch(connectorRequest(url, subject));
        expect(response.status).toBe(200);
        expect(response.headers.get("authorization")).toBeNull();
        expect(response.headers.get("set-cookie")).toBeNull();
        const body = await response.json<Record<string, unknown>>();
        expect(body).toMatchObject({
          caller_cookie: false,
          caller_proxy_credential: false,
          subject: null,
        });
        expect(JSON.stringify(body)).not.toContain("connector-access");
      }
      const write = await SELF.fetch(connectorRequest(
        "https://www.googleapis.com/upload/drive/v3/files?uploadType=media",
        subject,
        { method: "POST", body: "unbounded-provider-write" },
      ));
      expect(write.status).toBe(200);
      expect(await write.json()).toMatchObject({
        method: "POST",
        body: "unbounded-provider-write",
      });
      const xWrite = await SELF.fetch(connectorRequest(
        "https://api.x.com/2/tweets",
        subject,
        { method: "POST", body: JSON.stringify({ text: "hello from Nanocodex" }), contentType: "application/json" },
      ));
      expect(xWrite.status).toBe(200);
      expect(await xWrite.json()).toMatchObject({
        method: "POST",
        body: JSON.stringify({ text: "hello from Nanocodex" }),
        content_type: "application/json",
      });
      const xDelete = await SELF.fetch(connectorRequest(
        "https://api.x.com/2/users/2244994945/bookmarks/1890000000000000000",
        subject,
        { method: "DELETE" },
      ));
      expect(xDelete.status).toBe(200);
      expect(await xDelete.json()).toMatchObject({ method: "DELETE" });
      expect(log.mock.calls.flat().join(" ")).not.toMatch(
        /connector-access|connector-refresh|NANOCODEX_PROVIDER_CREDENTIAL/,
      );
      expect(errorLog.mock.calls.flat().join(" ")).not.toMatch(
        /connector-access|connector-refresh|NANOCODEX_PROVIDER_CREDENTIAL/,
      );
    } finally {
      log.mockRestore();
      errorLog.mockRestore();
    }
  });

  it("denies lookalike origins, cross-account Google paths, missing subjects, and bad placeholders", async () => {
    const subject = connectorSubject("denials");
    await control(`/subjects/${subject}`, "PUT", { user_id: "connector-denials" });
    await connect("connector-denials", "github", "github-code");
    const denied = [
      connectorRequest("http://api.github.com/repos/nanocodex/sdk", subject),
      connectorRequest("https://api.github.com.evil.test/repos/nanocodex/sdk", subject),
      connectorRequest("https://api.github.com:444/repos/nanocodex/sdk", subject),
      connectorRequest("https://github.com/repos/nanocodex/sdk", subject),
      connectorRequest("https://gmail.googleapis.com/gmail/v1/users/other/messages", subject),
      connectorRequest(
        "https://gmail.googleapis.com/gmail/v1/users/me/%2e%2e%2fother/messages",
        subject,
      ),
      connectorRequest("https://www.googleapis.com/drive/v3/%252e%252e%252fother", subject),
      connectorRequest("https://www.googleapis.com/oauth2/v3/userinfo", subject),
      connectorRequest("https://api.x.com/1.1/statuses/home_timeline.json", subject),
      connectorRequest("https://api.x.com/2/oauth2/revoke", subject),
      connectorRequest("https://api.prod.whoop.com/oauth/oauth2/token", subject),
      connectorRequest("https://api.prod.whoop.com/developer/v2/user/access", subject),
      connectorRequest("https://api.prod.whoop.com/developer/v2/partner/token", subject),
      connectorRequest(
        "https://api.prod.whoop.com/developer/v2/activity/%2e%2e%2fpartner/token",
        subject,
      ),
      connectorRequest("https://api.github.com/repos/nanocodex/sdk?access_token=caller", subject),
      connectorRequest("https://api.github.com/repos/nanocodex/sdk", ""),
      connectorRequest("https://api.github.com/repos/nanocodex/sdk", subject, {
        authorization: "Bearer caller-secret",
      }),
    ];
    for (const request of denied) expect((await SELF.fetch(request)).status).toBe(403);
  });

  it("enforces the same origin and Google account path policy inside the user broker", async () => {
    const user = "connector-direct-broker";
    await connect(user, "github", "github-code");
    const broker = workerEnv.USER_CONNECTORS.getByName(user);
    for (const request of [
      new Request("https://api.github.com.evil.test/repos/nanocodex/sdk"),
      new Request("https://gmail.googleapis.com/gmail/v1/users/other/messages"),
      new Request("https://evil.test/v1/status"),
    ]) expect((await broker.fetch(request)).status).toBe(403);
  });

  it("keeps connector selection scoped to the subject's owning user", async () => {
    const alphaSubject = connectorSubject("alpha");
    const betaSubject = connectorSubject("beta");
    await control(`/subjects/${alphaSubject}`, "PUT", { user_id: "connector-alpha" });
    await control(`/subjects/${betaSubject}`, "PUT", { user_id: "connector-beta" });
    await connect("connector-alpha", "github", "alpha-code");
    await connect("connector-beta", "github", "beta-code");

    const url = "https://api.github.com/repos/nanocodex/sdk";
    expect(await (await SELF.fetch(connectorRequest(url, alphaSubject))).json()).toMatchObject({
      account: "alpha",
    });
    expect(await (await SELF.fetch(connectorRequest(url, betaSubject))).json()).toMatchObject({
      account: "beta",
    });
    expect((await SELF.fetch(connectorRequest(
      url,
      connectorSubject("unbound"),
    ))).status).toBe(403);
  });

  it("rejects disconnected connectors and upstream credential projection", async () => {
    const subject = connectorSubject("unavailable");
    const user = "connector-unavailable";
    await control(`/subjects/${subject}`, "PUT", { user_id: user });
    expect((await SELF.fetch(connectorRequest(
      "https://api.github.com/repos/nanocodex/sdk",
      subject,
    ))).status).toBe(409);
    await connect(user, "github", "github-code");
    const reflected = await SELF.fetch(connectorRequest(
      "https://api.github.com/repos/nanocodex/sdk?reflect_credential=1",
      subject,
    ));
    expect(reflected.status).toBe(502);
    expect(await reflected.json()).toEqual({ error: "credential_projection_blocked" });

    const redirected = await SELF.fetch(connectorRequest(
      "https://api.github.com/repos/nanocodex/sdk?redirect=1",
      subject,
    ));
    expect(redirected.status).toBe(502);
    expect(await redirected.json()).toEqual({ error: "connector_redirect_blocked" });

    const oversized = await SELF.fetch(connectorRequest(
      "https://api.github.com/repos/nanocodex/sdk?oversize=1",
      subject,
    ));
    expect(oversized.status).toBe(502);
    expect(await oversized.json()).toEqual({ error: "connector_response_too_large" });
  });

  it("projects expired and unrefreshable connector credentials as disconnected", async () => {
    const githubSubject = connectorSubject("expired");
    const gmailSubject = connectorSubject("unrefreshable");
    await control(`/subjects/${githubSubject}`, "PUT", { user_id: "connector-expired" });
    await control(`/subjects/${gmailSubject}`, "PUT", { user_id: "connector-unrefreshable" });
    await connect("connector-expired", "github", "no-refresh-code");
    await connect("connector-unrefreshable", "gmail", "gmail-no-refresh-code");
    const githubExpired = await SELF.fetch(connectorRequest(
      "https://api.github.com/repos/nanocodex/sdk",
      githubSubject,
    ));
    expect(githubExpired.status).toBe(409);
    expect(await githubExpired.json()).toEqual({ error: "connector_reauthentication_required" });
    expect(await (await SELF.fetch(
      "https://broker.test/users/connector-expired/connectors",
    )).json()).toMatchObject({ connectors: { github: { connected: false } } });
    expect(await (await SELF.fetch(connectorRequest(
      "https://api.github.com/repos/nanocodex/sdk",
      githubSubject,
    ))).json()).toEqual({ error: "connector_not_connected" });
    expect((await SELF.fetch(connectorRequest(
      "https://gmail.googleapis.com/gmail/v1/users/me/messages",
      gmailSubject,
    ))).status).toBe(409);
  });

  it("rotates an expired GitHub connector entirely inside the user broker", async () => {
    const subject = connectorSubject("github-refresh");
    const user = "connector-github-refresh";
    await control(`/subjects/${subject}`, "PUT", { user_id: user });
    await connect(user, "github", "expired-code");
    const response = await SELF.fetch(connectorRequest(
      "https://api.github.com/repos/nanocodex/sdk",
      subject,
    ));
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ account: "github-refreshed" });
    expect(await (await SELF.fetch(
      `https://broker.test/users/${user}/connectors`,
    )).json()).toMatchObject({ connectors: { github: { connected: true } } });
  });

  it("clears a provider-rejected GitHub refresh token and requires reauthorization", async () => {
    const subject = connectorSubject("github-revoked-refresh");
    const user = "connector-github-revoked-refresh";
    await control(`/subjects/${subject}`, "PUT", { user_id: user });
    await connect(user, "github", "revoked-refresh-code");
    const response = await SELF.fetch(connectorRequest(
      "https://api.github.com/repos/nanocodex/sdk",
      subject,
    ));
    expect(response.status).toBe(409);
    expect(await response.json()).toEqual({ error: "connector_reauthentication_required" });
    expect(await (await SELF.fetch(
      `https://broker.test/users/${user}/connectors`,
    )).json()).toMatchObject({ connectors: { github: { connected: false } } });
  });

  it("refreshes an expired Google connector entirely inside the user broker", async () => {
    const subject = connectorSubject("refresh");
    const user = "connector-refresh";
    await control(`/subjects/${subject}`, "PUT", { user_id: user });
    await connect(user, "gmail", "gmail-expiring-code");
    const response = await SELF.fetch(connectorRequest(
      "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=1",
      subject,
    ));
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ account: "gmail-refreshed" });
  });

  it("clears provider-revoked access and refresh tokens and requires reauthorization", async () => {
    const githubSubject = connectorSubject("revoked-access");
    const gmailSubject = connectorSubject("revoked-refresh");
    await control(`/subjects/${githubSubject}`, "PUT", { user_id: "connector-revoked-access" });
    await control(`/subjects/${gmailSubject}`, "PUT", { user_id: "connector-revoked-refresh" });
    await connect("connector-revoked-access", "github", "github-code");
    await connect("connector-revoked-refresh", "gmail", "gmail-revoked-code");

    for (const [request, user, connector] of [
      [connectorRequest("https://api.github.com/repos/nanocodex/sdk?revoked=1", githubSubject),
        "connector-revoked-access", "github"],
      [connectorRequest("https://gmail.googleapis.com/gmail/v1/users/me/messages", gmailSubject),
        "connector-revoked-refresh", "gmail"],
    ] as const) {
      const revoked = await SELF.fetch(request);
      expect(revoked.status).toBe(409);
      expect(await revoked.json()).toEqual({ error: "connector_reauthentication_required" });
      const status = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
      expect(await status.json()).toMatchObject({
        connectors: { [connector]: { connected: false } },
      });
    }
  });

  it("revokes each upstream OAuth grant before deleting local connector state", async () => {
    for (const connector of ["github", "gmail", "gdrive"] as const) {
      const user = `connector-disconnect-${connector}`;
      await connect(user, connector, connector === "gdrive" ? "gdrive-code" : `${connector}-code`);
      const disconnected = await SELF.fetch(
        `https://broker.test/users/${user}/connectors/${connector}`,
        { method: "DELETE" },
      );
      expect(disconnected.status).toBe(204);
      const status = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
      expect(await status.json()).toMatchObject({
        connectors: { [connector]: { connected: false } },
      });
    }
  });

  it("disconnects sibling Google connectors for the same revoked account grant", async () => {
    const user = "connector-google-shared-account";
    await connect(user, "gmail", "gmail-shared-account-code");
    await connect(user, "gdrive", "gdrive-shared-account-code");

    const connected = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
    expect(await connected.json()).toMatchObject({
      connectors: {
        gmail: { connected: true, account_id: "google-shared-account" },
        gdrive: { connected: true, account_id: "google-shared-account" },
      },
    });

    const disconnected = await SELF.fetch(
      `https://broker.test/users/${user}/connectors/gmail`,
      { method: "DELETE" },
    );
    expect(disconnected.status).toBe(204);
    const status = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
    expect(await status.json()).toMatchObject({
      connectors: {
        gmail: { connected: false },
        gdrive: { connected: false },
      },
    });
  });

  it("clears sibling Google connectors when the shared account grant is rejected", async () => {
    const user = "connector-google-rejected-account";
    const subject = connectorSubject("google-rejected-account");
    await control(`/subjects/${subject}`, "PUT", { user_id: user });
    await connect(user, "gmail", "gmail-shared-account-code");
    await connect(user, "gdrive", "gdrive-shared-account-code");

    const rejected = await SELF.fetch(connectorRequest(
      "https://gmail.googleapis.com/gmail/v1/users/me/messages?revoked=1",
      subject,
    ));
    expect(rejected.status).toBe(409);
    expect(await rejected.json()).toEqual({ error: "connector_reauthentication_required" });

    const status = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
    expect(await status.json()).toMatchObject({
      connectors: {
        gmail: { connected: false },
        gdrive: { connected: false },
      },
    });
  });

  it("retains encrypted connector state when upstream revocation is retryable", async () => {
    for (const [connector, code] of [
      ["github", "revoke-failure-code"],
      ["gmail", "gmail-revoke-failure-code"],
    ] as const) {
      const user = `connector-disconnect-retry-${connector}`;
      await connect(user, connector, code);
      const disconnected = await SELF.fetch(
        `https://broker.test/users/${user}/connectors/${connector}`,
        { method: "DELETE" },
      );
      expect(disconnected.status).toBe(503);
      expect(await disconnected.json()).toEqual({ error: "connector_revocation_failed" });
      const status = await SELF.fetch(`https://broker.test/users/${user}/connectors`);
      expect(await status.json()).toMatchObject({
        connectors: { [connector]: { connected: true } },
      });
    }
  });

  it("emits secret-free lifecycle audits for authorization, use, failure, and disconnect", async () => {
    const subject = connectorSubject("audit");
    const user = "connector-audit";
    await control(`/subjects/${subject}`, "PUT", { user_id: user });
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    try {
      await connect(user, "github", "github-code");
      expect((await SELF.fetch(connectorRequest(
        "https://api.github.com/repos/nanocodex/sdk",
        subject,
      ))).status).toBe(200);

      const started = await control(`/users/${user}/connectors/gmail`, "POST", {
        redirect_uri: "https://nanocodex.test/v1/connectors/gmail/callback",
        return_to: "/",
      });
      expect(started.status).toBe(200);
      const failed = await control(`/users/${user}/connectors/gmail/callback`, "POST", {
        code: "authorization-code-must-not-be-logged",
        state: "invalid-state-must-not-be-logged",
      });
      expect(failed.status).toBe(400);
      expect((await SELF.fetch(`https://broker.test/users/${user}/connectors/github`, {
        method: "DELETE",
      })).status).toBe(204);

      const entries = log.mock.calls.flatMap(([value]) => {
        if (!value || typeof value !== "object") return [];
        const entry = value as Record<string, unknown>;
        return entry.type === "connector.audit" ? [entry] : [];
      });
      expect(entries).toEqual(expect.arrayContaining([
        expect.objectContaining({ action: "authorize_start", outcome: "allow", connector: "github" }),
        expect.objectContaining({ action: "authorize_callback", outcome: "allow", connector: "github" }),
        expect.objectContaining({ action: "use", outcome: "allow", connector: "github" }),
        expect.objectContaining({
          action: "authorize_callback",
          outcome: "deny",
          connector: "gmail",
          code: "invalid_oauth_state",
        }),
        expect.objectContaining({
          action: "disconnect",
          outcome: "allow",
          connector: "github",
          provider_revoked: true,
        }),
      ]));
      const encoded = JSON.stringify(entries);
      expect(encoded).not.toMatch(
        /connector-access|connector-refresh|authorization-code-must-not-be-logged|invalid-state-must-not-be-logged|NANOCODEX_PROVIDER_CREDENTIAL/,
      );
      const egressEntries = log.mock.calls.flatMap(([value]) => {
        if (!value || typeof value !== "object") return [];
        const entry = value as Record<string, unknown>;
        return entry.type === "egress.request" && entry.rule === "github" ? [entry] : [];
      });
      expect(egressEntries).toContainEqual(expect.objectContaining({
        path: "/provider-api",
        user_id: user,
        connector: "github",
      }));
      expect(JSON.stringify(egressEntries)).not.toContain("/repos/nanocodex/sdk");
      const controlEntries = log.mock.calls.flatMap(([value]) => {
        if (!value || typeof value !== "object") return [];
        const entry = value as Record<string, unknown>;
        return entry.type === "egress.control" ? [entry] : [];
      });
      expect(controlEntries).toEqual(expect.arrayContaining([
        expect.objectContaining({
          action: "allow",
          connector: "github",
          operation: "connectors",
          user_id: user,
        }),
        expect.objectContaining({
          action: "deny",
          connector: "gmail",
          operation: "connectors",
          user_id: user,
        }),
      ]));
      expect(JSON.stringify(controlEntries)).not.toMatch(
        /connector-access|connector-refresh|authorization-code-must-not-be-logged|invalid-state-must-not-be-logged/,
      );
    } finally {
      log.mockRestore();
    }
  });

  it("refreshes an expiring X connector before polling direct messages", async () => {
    const subject = "X".repeat(43);
    const user = "connector-x-refresh";
    await control(`/subjects/${subject}`, "PUT", { user_id: user });
    await connect(user, "x", "x-expiring-code");
    const response = await SELF.fetch(connectorRequest(
      "https://api.x.com/2/dm_events?max_results=5",
      subject,
    ));
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ account: "x-refreshed" });
  });

  it("rotates an expiring WHOOP refresh token before reading recovery", async () => {
    const subject = "W".repeat(43);
    const user = "connector-whoop-refresh";
    await control(`/subjects/${subject}`, "PUT", { user_id: user });
    await connect(user, "whoop", "whoop-expiring-code");
    const response = await SELF.fetch(connectorRequest(
      "https://api.prod.whoop.com/developer/v2/recovery?limit=1",
      subject,
    ));
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ account: "whoop-refreshed" });
  });

  it("enforces WHOOP read-only access and revokes the provider grant on disconnect", async () => {
    const subject = "R".repeat(43);
    const user = "connector-whoop-read-only";
    await control(`/subjects/${subject}`, "PUT", { user_id: user });
    await connect(user, "whoop", "whoop-code");
    expect((await SELF.fetch(connectorRequest(
      "https://api.prod.whoop.com/developer/v2/activity/workout",
      subject,
      { method: "POST", body: "{}", contentType: "application/json" },
    ))).status).toBe(403);
    expect((await SELF.fetch(
      `https://broker.test/users/${user}/connectors/whoop`,
      { method: "DELETE" },
    )).status).toBe(204);
    expect(await (await SELF.fetch(
      `https://broker.test/users/${user}/connectors`,
    )).json()).toMatchObject({ connectors: { whoop: { connected: false } } });
  });

  it("refreshes an expiring WHOOP token before revoking access", async () => {
    const user = "connector-whoop-expiring-disconnect";
    await connect(user, "whoop", "whoop-expiring-code");
    expect((await SELF.fetch(
      `https://broker.test/users/${user}/connectors/whoop`,
      { method: "DELETE" },
    )).status).toBe(204);
    expect(await (await SELF.fetch(
      `https://broker.test/users/${user}/connectors`,
    )).json()).toMatchObject({ connectors: { whoop: { connected: false } } });
  });
});

function control(path: string, method: string, body: Record<string, unknown>): Promise<Response> {
  return SELF.fetch(`https://broker.test${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

function connectorSubject(label: string): string {
  return `connector_${label.replaceAll("-", "_")}`.padEnd(43, "_");
}

async function connect(
  user: string,
  connector: "github" | "gmail" | "gdrive" | "x" | "whoop",
  code: string,
): Promise<void> {
  const started = await control(`/users/${user}/connectors/${connector}`, "POST", {
    redirect_uri: `https://nanocodex.test/v1/connectors/${connector}/callback`,
    return_to: "/",
  });
  expect(started.status).toBe(200);
  const authorization = new URL((await started.json<{ authorization_url: string }>()).authorization_url);
  const completed = await control(`/users/${user}/connectors/${connector}/callback`, "POST", {
    code,
    state: authorization.searchParams.get("state"),
  });
  expect(completed.status).toBe(200);
}

function connectorRequest(
  url: string,
  subject: string,
  override: Readonly<{
    method?: string;
    authorization?: string;
    body?: string;
    contentType?: string;
  }> = {},
): Request {
  return new Request(url, {
    method: override.method ?? "GET",
    headers: {
      authorization: override.authorization ?? "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      cookie: "caller-secret=cookie",
      "proxy-authorization": "Basic caller-proxy-secret",
      ...(subject ? { "x-nanocodex-subject": subject } : {}),
      ...(override.body === undefined
        ? {}
        : { "content-type": override.contentType ?? "application/octet-stream" }),
    },
    ...(override.body === undefined ? {} : { body: override.body }),
  });
}

function modelRequest(subject: string): Request {
  return new Request("https://nanocodex.internal/v1/search", {
    method: "POST",
    headers: {
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      "content-type": "application/json",
      "user-agent": "nanocodex-test",
      "x-nanocodex-subject": subject,
      "x-should-not-forward": "secret",
    },
    body: JSON.stringify({ query: "safe" }),
  });
}

function realtimeRequest(
  subject: string,
  path: "/v1/realtime/calls" | "/v1/realtime/sideband",
  options: Readonly<{ body?: string; callId?: string; method: "GET" | "POST" }>,
): Request {
  return new Request(`https://nanocodex.internal${path}`, {
    method: options.method,
    headers: {
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
      ...(options.method === "POST" ? { "content-type": "application/json" } : { upgrade: "websocket" }),
      "x-nanocodex-subject": subject,
      "openai-alpha": "quicksilver=v2",
      "x-session-id": "realtime-session",
      "session-id": "lifecycle-session",
      "thread-id": "thread-session",
      ...(options.callId ? { "x-nanocodex-realtime-call-id": options.callId } : {}),
    },
    ...(options.body === undefined ? {} : { body: options.body }),
  });
}

function claimLocalCredential(broker: Pick<UserCredentialBroker, "fetch">): Promise<Response> {
  return broker.fetch(new Request("https://broker.internal/v1/chatgpt/local-claim", {
    method: "POST",
  }));
}

async function brokerCall(
  stub: DurableObjectStub<UserCredentialBroker>,
  path: string,
): Promise<{ status: number; body: Record<string, unknown> }> {
  const response = await stub.fetch(`https://broker.internal${path}`, { method: "POST" });
  return { status: response.status, body: await response.json<Record<string, unknown>>() };
}

function responseLifecycleEnv(
  subject: string,
  requests: Array<{ recover: boolean; revision?: number }>,
  credential: (recover: boolean) => Record<string, unknown>,
): EgressEnv {
  return {
    ...workerEnv,
    CODEX_RELAY_URL: "https://relay.example/",
    AGENT_SUBJECTS: {
      getByName: (name: string) => {
        expect(name).toBe(`agent-subject-v1:${subject}`);
        return {
          fetch: async () => Response.json({ user_id: "response-lifecycle-user" }),
        };
      },
    } as unknown as EgressEnv["AGENT_SUBJECTS"],
    USER_CREDENTIALS: {
      getByName: (name: string) => {
        expect(name).toBe("response-lifecycle-user");
        return {
          fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
            const request = input instanceof Request ? input : new Request(input, init);
            const body = await request.json<{ recover?: boolean; revision?: number }>();
            const observed = {
              recover: body.recover === true,
              ...(body.revision === undefined ? {} : { revision: body.revision }),
            };
            requests.push(observed);
            return Response.json(credential(observed.recover));
          },
        };
      },
    } as unknown as EgressEnv["USER_CREDENTIALS"],
  };
}

async function credential(
  broker: Pick<UserCredentialBroker, "fetch">,
  recover = false,
  revision?: number,
): Promise<{ status: number; body: Record<string, unknown> }> {
  const response = await broker.fetch(new Request("https://broker.internal/v1/credential", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ recover, ...(revision === undefined ? {} : { revision }) }),
  }));
  return { status: response.status, body: await response.json<Record<string, unknown>>() };
}

function durableAlarm(stub: DurableObjectStub<UserCredentialBroker>): Promise<number | null> {
  return runInDurableObject(
    stub,
    async (_instance: UserCredentialBroker, state) => state.storage.getAlarm(),
  );
}

function subjectOwner(
  stub: DurableObjectStub<AgentSubjectDirectory>,
  subject: string,
): Promise<string | undefined> {
  return runInDurableObject(
    stub,
    async (_instance: AgentSubjectDirectory, state) => {
      const retained = await state.storage.get<string>(`subject:${subject}`);
      return retained?.startsWith("!deleted:") ? undefined : retained;
    },
  );
}

function subjectRetained(
  stub: DurableObjectStub<AgentSubjectDirectory>,
  subject: string,
): Promise<string | undefined> {
  return runInDurableObject(
    stub,
    async (_instance: AgentSubjectDirectory, state) => state.storage.get(`subject:${subject}`),
  );
}

function durableRefreshState(stub: DurableObjectStub<UserCredentialBroker>): Promise<{
  refreshAfter?: number;
  refreshAttempts?: number;
  refreshState: string;
}> {
  return runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
    const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("credential-state");
    if (!row) throw new Error("missing durable credential state");
    const vault = new CredentialVault(workerEnv, `user/${state.id.toString()}`);
    const opened = await vault.open<{ chatgpt: {
      refreshAfter?: number;
      refreshAttempts?: number;
      refreshState: string;
    } }>(row.envelope);
    return opened.value.chatgpt;
  });
}

function rateLimitedResponse(
  retryAfter: string | null,
  cancel: () => void,
): Response {
  const body = new ReadableStream({ cancel });
  return new Response(body, {
    status: 429,
    ...(retryAfter === null ? {} : { headers: { "retry-after": retryAfter } }),
  });
}

type ResponseActivity = { pulls: number; cancellations: number };

function responseActivity(): ResponseActivity {
  return { pulls: 0, cancellations: 0 };
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve(value: T | PromiseLike<T>): void;
} {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((accept) => { resolve = accept; });
  return { promise, resolve };
}

function streamedJsonResponse(
  body: Record<string, unknown>,
  status: number,
  activity: ResponseActivity,
): Response {
  return streamedResponse(JSON.stringify(body), status, activity, false, {
    "content-type": "application/json",
  });
}

function streamedResponse(
  body: string,
  status: number,
  activity: ResponseActivity,
  rejectCancellation: boolean,
  headers?: HeadersInit,
): Response {
  const encoded = new TextEncoder().encode(body);
  let sent = false;
  const stream = new ReadableStream<Uint8Array>({
    pull(controller) {
      activity.pulls += 1;
      if (sent) return;
      sent = true;
      controller.enqueue(encoded);
      controller.close();
    },
    cancel() {
      activity.cancellations += 1;
      return rejectCancellation
        ? Promise.reject(new Error("response body cancellation rejected"))
        : undefined;
    },
  }, { highWaterMark: 0 });
  return new Response(stream, {
    status,
    ...(headers === undefined ? {} : { headers }),
  });
}

function refreshedTokens(expiresAt: number): Response {
  return Response.json({
    access_token: testJwt({ exp: Math.floor(expiresAt / 1_000), marker: "refreshed" }),
    refresh_token: "rotated-refresh-token",
  });
}

function testJwt(payload: Record<string, unknown>): string {
  const encode = (value: unknown) => btoa(JSON.stringify(value))
    .replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
  return `${encode({ alg: "none" })}.${encode(payload)}.test`;
}
