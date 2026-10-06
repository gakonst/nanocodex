import { env } from "cloudflare:workers";
import { runInDurableObject, SELF } from "cloudflare:test";
import { Provider, secp256k1, Storage } from "accounts";
import { describe, expect, it } from "vitest";

import type { UserCredentialBroker } from "../src/broker";
import { CredentialVault, type EncryptedEnvelope } from "../src/credential-vault";
import type { EgressEnv } from "../src/egress";

const workerEnv = env as unknown as EgressEnv;

describe("per-user root wallets", () => {
  it("provisions once, keeps root material encrypted, and separates users", async () => {
    const first = await provision("wallet-provision-a");
    const second = await provision("wallet-provision-a");
    const other = await provision("wallet-provision-b");

    expect(second).toEqual(first);
    expect(other.address).not.toBe(first.address);
    expect(JSON.stringify(first)).not.toMatch(/private/i);

    const metadata = await SELF.fetch("https://broker.internal/users/wallet-provision-a/wallet");
    expect(metadata.status).toBe(200);
    expect(await metadata.json()).toEqual({ ...first, mode: "internal", original_address: first.address });

    const stub = workerEnv.USER_CREDENTIALS.getByName("wallet-provision-a");
    await runInDurableObject(stub, async (_instance: UserCredentialBroker, state) => {
      const row = await state.storage.get<{ envelope: EncryptedEnvelope }>("credential-state");
      expect(row).toBeDefined();
      expect(JSON.stringify(row)).not.toContain(first.address);
      expect(JSON.stringify(row)).not.toMatch(/privateKey|private_key/);

      const vault = new CredentialVault(workerEnv, `user/${state.id.toString()}`);
      const opened = await vault.open<{ wallet?: { address: string; privateKey: string } }>(row!.envelope);
      expect(opened.value.wallet?.address).toBe(first.address);
      expect(opened.value.wallet?.privateKey).toMatch(/^0x[0-9a-f]{64}$/i);
      const provider = Provider.create({
        adapter: secp256k1({ privateKey: opened.value.wallet!.privateKey as `0x${string}` }),
        storage: Storage.memory({ key: "root-wallet-vault-proof" }),
        mpp: false,
      });
      const derived = await provider.request({
        method: "wallet_connect",
        params: [{ chainId: "0x1079", capabilities: { method: "login" } }],
      } as never) as { accounts?: readonly { address?: string }[] };
      expect(derived.accounts?.[0]?.address?.toLowerCase()).toBe(first.address);
    });
  });

  it("returns the exact SDK connect result through mocked Connect auth fetches", async () => {
    const wallet = await provision("wallet-connect");
    const response = await walletConnect("wallet-connect", {
      request: {
        method: "wallet_connect",
        params: [{
          chainId: "0x1079",
          capabilities: {
            method: "login",
            auth: {
              challenge: "https://nanocodex.localhost/v1/connect/auth/challenge",
              verify: "https://nanocodex.localhost/v1/connect/auth",
              logout: "https://nanocodex.localhost/v1/connect/auth/logout",
              resources: ["urn:nanocodex:agent:run"],
              returnToken: true,
            },
          },
        }],
      },
    });
    expect(response.status).toBe(200);
    const result = await response.json<Record<string, unknown>>();
    const accounts = result.accounts as readonly Record<string, unknown>[];
    expect(String(accounts[0]?.address).toLowerCase()).toBe(wallet.address);
    expect(result).toMatchObject({
      accounts: [{
        capabilities: { auth: { approval_id: "wallet-test-approval", token: "wallet-test-token" } },
      }],
    });
    expect(JSON.stringify(result)).not.toMatch(/privateKey|private_key/);
  });

  it("reads MACH through the signer-backed SDK provider without exposing root material", async () => {
    const wallet = await provision("wallet-balance");
    const response = await SELF.fetch("https://broker.internal/users/wallet-balance/wallet/balance");
    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({
      account: wallet.address,
      balance: "12345678",
      decimals: 6,
      symbol: "MACH",
      token: "0x20c000000000000000000000f37de3740adec032",
    });
  });

  it("returns public identity and live balance together without changing metadata-only reads", async () => {
    const user = "wallet-startup-snapshot";
    const missing = await SELF.fetch(`https://broker.internal/users/${user}/wallet`, { headers: { accept: "application/vnd.nanocodex.wallet-snapshot+json" } });
    expect(missing.status).toBe(404);
    await expect(missing.json()).resolves.toEqual({ error: "wallet_not_configured" });
    const wallet = await provision(user);
    const snapshot = await SELF.fetch(`https://broker.internal/users/${user}/wallet`, { headers: { accept: "application/vnd.nanocodex.wallet-snapshot+json" } });
    expect(snapshot.status).toBe(200);
    await expect(snapshot.json()).resolves.toEqual({ ...wallet, mode: "internal", original_address: wallet.address, balance: {
      account: wallet.address, balance: "12345678", decimals: 6, symbol: "MACH",
      token: "0x20c000000000000000000000f37de3740adec032",
    } });
    const metadata = await SELF.fetch(`https://broker.internal/users/${user}/wallet`);
    await expect(metadata.json()).resolves.toEqual({ ...wallet, mode: "internal", original_address: wallet.address });
  });

  it("keeps a stalled balance out of the credential queue and refreshes the next snapshot live", async () => {
    const user = "wallet-startup-stalled";
    const wallet = await provision(user);
    const connected = await SELF.fetch(`https://broker.internal/users/${user}/credentials/openai`, {
      method: "PUT", headers: { "content-type": "application/json" },
      body: JSON.stringify({ api_key: "sk-synthetic-wallet-concurrency" }),
    });
    expect(connected.status).toBe(204);
    const control = async (action: string, result?: string) => {
      const response = await fetch("https://rpc.tempo.xyz/__wallet-balance-fixture", {
        method: "POST", body: JSON.stringify({ action, account: wallet.address, result }),
      });
      return response.json<{ started: boolean }>();
    };
    await control("hold");
    try {
      const snapshot = SELF.fetch(`https://broker.internal/users/${user}/wallet`, { headers: { accept: "application/vnd.nanocodex.wallet-snapshot+json" } });
      for (let attempt = 0; ; attempt++) {
        if ((await control("status")).started) break;
        expect(attempt).toBeLessThan(100);
        await new Promise(resolve => setTimeout(resolve, 5));
      }
      const revoked = SELF.fetch(`https://broker.internal/users/${user}/credentials/openai`, { method: "DELETE" });
      expect(await Promise.race([revoked.then(() => "revoked"), snapshot.then(() => "snapshot")])).toBe("revoked");
      expect((await revoked).status).toBe(204);
      const status = await SELF.fetch(`https://broker.internal/users/${user}/credentials`);
      await expect(status.json()).resolves.toMatchObject({ ready: false, openai: { connected: false } });
      const metadata = await SELF.fetch(`https://broker.internal/users/${user}/wallet`);
      await expect(metadata.json()).resolves.toEqual({ ...wallet, mode: "internal", original_address: wallet.address });
      const unavailable = await snapshot;
      expect(unavailable.status).toBe(200);
      await expect(unavailable.json()).resolves.toEqual({ ...wallet, mode: "internal", original_address: wallet.address, balance: null });
      await control("release", "0x" + "0".repeat(62) + "2a");
      const recovered = await SELF.fetch(`https://broker.internal/users/${user}/wallet`, { headers: { accept: "application/vnd.nanocodex.wallet-snapshot+json" } });
      await expect(recovered.json()).resolves.toEqual({ ...wallet, mode: "internal", original_address: wallet.address, balance: {
        account: wallet.address, balance: "42", decimals: 6, symbol: "MACH",
        token: "0x20c000000000000000000000f37de3740adec032",
      } });
    } finally { await control("clear"); }
  });

  it("accepts the SDK base-url auth form while pinning its derived endpoints", async () => {
    const response = await walletConnect("wallet-connect-url", {
      request: {
        method: "wallet_connect",
        params: [{
          chainId: "0x1079",
          capabilities: {
            method: "login",
            auth: {
              url: "https://nanocodex.localhost/v1/connect/auth",
              resources: ["urn:nanocodex:agent:run"],
              returnToken: true,
            },
          },
        }],
      },
    });
    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      accounts: [{ capabilities: { auth: { token: "wallet-test-token" } } }],
    });
  });

  it("rejects malformed and other wallet methods without exposing root material", async () => {
    const user = "wallet-invalid";
    const missing = await SELF.fetch(`https://broker.internal/users/${user}/wallet`);
    expect(missing.status).toBe(404);

    // A discovered payment challenge or model-produced RPC request must never
    // turn the login-only bridge into root-wallet signing authority. Rejections
    // must happen before wallet provisioning, not just before transmission.
    for (const request of [
      { method: "personal_sign", params: ["0x68656c6c6f", "0x0000000000000000000000000000000000000001"] },
      { method: "eth_signTypedData_v4", params: ["0x0000000000000000000000000000000000000001", "{}"] },
      { method: "eth_sendTransaction", params: [{ to: "0x0000000000000000000000000000000000000002", value: "0x1" }] },
      { method: "wallet_sendCalls", params: [{ version: "2.0.0", chainId: "0x1079", calls: [{ to: "0x0000000000000000000000000000000000000002", data: "0x" }] }] },
    ]) {
      const rejected = await walletConnect(user, { request });
      expect(rejected.status, request.method).toBe(400);
      await expect(rejected.json()).resolves.toEqual({ error: "invalid_wallet_connect_request" });
    }
    const afterRejectedSigning = await SELF.fetch(`https://broker.internal/users/${user}/wallet`);
    expect(afterRejectedSigning.status).toBe(404);

    const wrongChain = await walletConnect(user, {
      request: {
        method: "wallet_connect",
        params: [{ chainId: "0x1", capabilities: { method: "login", auth: {} } }],
      },
    });
    expect(wrongChain.status).toBe(400);

    const provisioned = await provision(user);
    const revoke = await SELF.fetch(`https://broker.internal/users/${user}/wallet/revoke-access-key`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        request: {
          method: "wallet_revokeAccessKey",
          params: [{
            address: "0x0000000000000000000000000000000000000001",
            accessKeyAddress: "0x0000000000000000000000000000000000000002",
          }],
        },
      }),
    });
    expect(revoke.status).toBe(403);
    expect(JSON.stringify(await revoke.json())).not.toContain(provisioned.address);
  });
});

async function provision(user: string): Promise<{ address: string; created_at: number }> {
  const response = await SELF.fetch(`https://broker.internal/users/${user}/wallet`, { method: "PUT" });
  expect(response.status).toBe(200);
  const value = await response.json<{ address: string; created_at: number }>();
  expect(Object.keys(value).sort()).toEqual(["address", "created_at"]);
  return value;
}

function walletConnect(user: string, body: unknown): Promise<Response> {
  return SELF.fetch(`https://broker.internal/users/${user}/wallet/connect`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}
