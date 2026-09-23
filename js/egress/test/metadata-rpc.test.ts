import { env, exports } from "cloudflare:workers";
import { createExecutionContext, SELF } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import Egress, { type EgressEnv } from "../src/egress";

const runtime = env as unknown as EgressEnv;
// The loopback service dispatches through workerd RPC, then the owning DO RPC.
const service = (exports as unknown as {
  default: Pick<Egress, "readAccountCatalog" | "readAccountVault">;
}).default;

function control(path: string, method = "GET", body?: unknown) {
  return SELF.fetch(`https://broker.internal${path}`, {
    method,
    ...(body === undefined ? {} : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
  });
}

describe("private account metadata RPC", () => {
  it("reads live connector/MCP metadata through both RPC legs with the HTTP projection", async () => {
    const user = "rpc-catalog-owner";
    const id = "R".repeat(43);
    const started = await control(`/users/${user}/connectors/github`, "POST", {
      redirect_uri: "https://nanocodex.test/v1/connectors/callback", return_to: "/agent",
    });
    expect(started.status).toBe(200);
    const state = new URL((await started.json<{ authorization_url: string }>()).authorization_url).searchParams.get("state");
    expect((await control(`/users/${user}/connectors/github/callback`, "POST", { code: "github-code", state })).status).toBe(200);
    expect((await control(`/users/${user}/mcp-connections/${id}`, "PUT", {
      endpoint: "https://mcp.linear.app/mcp", name: "Synthetic workspace",
    })).status).toBe(200);

    const read = service.readAccountCatalog;
    expect(typeof read).toBe("function");
    const result = await Reflect.apply(read, service, [user]);
    expect(result.status).toBe(200);
    expect(result.catalog).toEqual(await (await control(`/users/${user}/catalog`)).json());
    expect(result).toEqual(await runtime.USER_CONNECTORS.getByName(user).readCatalog());
    expect(result.catalog).toMatchObject({ connectors: { github: { connected: true } },
      mcp_connections: [{ id, name: "Synthetic workspace" }] });
    expect(JSON.stringify(result)).not.toMatch(/github-connector-access|mcp\.linear\.app|access_token|refresh_token|authorization_url/);

    expect((await control(`/users/${user}/mcp-connections/${id}`, "DELETE")).status).toBe(200);
    expect((await service.readAccountCatalog(user)).catalog).toMatchObject({
      mcp_connections: [{ id, name: "Synthetic workspace", status: "revoked" }],
    });
    expect((await service.readAccountCatalog("rpc-other-owner")).catalog).toMatchObject({
      connectors: { github: { connected: false } }, mcp_connections: [],
    });
  });

  it("returns only public vault fields through real service and DO RPC", async () => {
    const user = "rpc-vault-owner";
    for (const [kind, body] of [
      ["api_key", { name: "Synthetic API", api_key: "synthetic-private-api-key" }],
      ["login", { name: "Synthetic login", username: "user@example.test", password: "synthetic-private-password" }],
      ["card", { name: "Synthetic card", card_number: "4111111111111111", expiry_month: "09", expiry_year: "2031", cvv: "123", billing_zip: "10001" }],
    ] as const) {
      expect((await control(`/users/${user}/credentials/vault/${kind}`, "POST", body)).status).toBe(201);
    }
    const read = service.readAccountVault;
    expect(typeof read).toBe("function");
    const result = await Reflect.apply(read, service, [user]);
    expect(result).toEqual({ status: 200, ...(await (await control(`/users/${user}/credentials/vault`)).json<object>()) });
    expect(result).toEqual(await runtime.USER_CREDENTIALS.getByName(user).readVaultMetadata());
    expect(result.vault).toEqual(expect.arrayContaining([
      expect.objectContaining({ kind: "api_key", name: "Synthetic API" }),
      expect.objectContaining({ kind: "login", username: "user@example.test" }),
      expect.objectContaining({ kind: "card", last4: "1111" }),
    ]));
    expect(JSON.stringify(result)).not.toMatch(/synthetic-private|4111111111111111|password|api_key"\s*:|cvv|expiry_month|billing_zip/);
    expect(await service.readAccountVault("rpc-empty-vault")).toEqual({ status: 200, vault: [] });
  });

  it("rejects invalid owner IDs before resolving either DO and leaves HTTP method routing unchanged", async () => {
    for (const owner of [null, "", "../other", "owner/other", "a".repeat(129)]) {
      expect(await service.readAccountCatalog(owner)).toEqual({ status: 400, catalog: null });
      expect(await service.readAccountVault(owner)).toEqual({ status: 400, vault: null });
    }
    expect((await control("/readAccountCatalog")).status).toBe(403);
    expect((await control("/users/rpc-owner/catalog", "POST", {})).status).toBe(405);
  });

  it("uses a single owning DO RPC per read and never falls back after an RPC failure", async () => {
    const fetch = vi.fn(async () => { throw new Error("unexpected metadata HTTP"); });
    const readCatalog = vi.fn(async () => ({ status: 200, catalog: { connectors: {}, mcp_connections: [] } }));
    const readVaultMetadata = vi.fn(async () => ({ status: 200, vault: [] }));
    const connector = vi.fn(() => ({ fetch, readCatalog }));
    const credential = vi.fn(() => ({ fetch, readVaultMetadata }));
    const entry = new Egress(createExecutionContext(), {
      USER_CONNECTORS: { getByName: connector }, USER_CREDENTIALS: { getByName: credential },
    } as unknown as EgressEnv);
    await Promise.all([entry.readAccountCatalog("rpc-owner"), entry.readAccountVault("rpc-owner")]);
    expect(connector).toHaveBeenCalledTimes(1);
    expect(credential).toHaveBeenCalledTimes(1);
    expect(readCatalog).toHaveBeenCalledOnce();
    expect(readVaultMetadata).toHaveBeenCalledOnce();
    readCatalog.mockRejectedValueOnce(new Error("synthetic RPC failure"));
    await expect(entry.readAccountCatalog("rpc-owner")).rejects.toThrow("synthetic RPC failure");
    expect(fetch).not.toHaveBeenCalled();
  });
});

describe("metadata forwarding ownership", () => {
  it.each([200, 503])("disposes both DO owners and forwards detached data (status=%s)", async status => {
    const catalogDispose = vi.fn(), vaultDispose = vi.fn();
    const catalog = { status, catalog: { connectors: {}, mcp_connections: [] as unknown[] },
      [Symbol.dispose]() { catalog.catalog.mcp_connections.push({ id: "disposed" }); catalogDispose(); } };
    const vault = { status, vault: [] as unknown[],
      [Symbol.dispose]() { vault.vault.push({ id: "disposed" }); vaultDispose(); } };
    const fetch = vi.fn(async () => { throw new Error("unexpected metadata HTTP fallback"); });
    const entry = new Egress(createExecutionContext(), {
      USER_CONNECTORS: { getByName: () => ({ readCatalog: async () => catalog, fetch }) },
      USER_CREDENTIALS: { getByName: () => ({ readVaultMetadata: async () => vault, fetch }) },
    } as unknown as EgressEnv);
    const forwardedCatalog = await entry.readAccountCatalog("rpc-owner");
    const forwardedVault = await entry.readAccountVault("rpc-owner");
    expect(catalogDispose).toHaveBeenCalledOnce(); expect(vaultDispose).toHaveBeenCalledOnce();
    expect(forwardedCatalog).toEqual({ status, catalog: { connectors: {}, mcp_connections: [] } });
    expect(forwardedVault).toEqual({ status, vault: [] });
    expect(Object.getOwnPropertySymbols(forwardedCatalog)).toEqual([]);
    expect(Object.getOwnPropertySymbols(forwardedVault)).toEqual([]);
    expect(fetch).not.toHaveBeenCalled();
  });
});
