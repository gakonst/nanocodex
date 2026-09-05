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
    expect(await metadata.json()).toEqual(first);

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

    const malformed = await walletConnect(user, {
      request: { method: "personal_sign", params: [] },
    });
    expect(malformed.status).toBe(400);

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
  return response.json<{ address: string; created_at: number }>();
}

function walletConnect(user: string, body: unknown): Promise<Response> {
  return SELF.fetch(`https://broker.internal/users/${user}/wallet/connect`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}
