import { Adapter, local, Provider, Storage } from "accounts";
import { deviceCode } from "accounts/deviceCode";
import { KeyAuthorization, SignatureEnvelope } from "ox/tempo";
import { createClient, http } from "viem";
import { Account, Actions, Secp256k1 } from "viem/tempo";
import { tempo } from "viem/tempo/chains";

const HOST = "https://wallet.tempo.xyz";
const ENDPOINT = `${HOST}/api/auth/device`;
const RPC = "https://rpc.tempo.xyz";
const ADDRESS = /^0x[0-9a-f]{40}$/i;
export const WALLET_OPERATION = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export type LinkedWallet = {
  address: `0x${string}`;
  privateKey: `0x${string}`;
  authorization: KeyAuthorization.Rpc;
  createdAt: number;
  operationId: string;
};
export type WalletLink = {
  operationId: string;
  status: "pending" | "linked" | "rejected" | "expired" | "interrupted" | "cancelled";
  privateKey?: `0x${string}`;
  approvalUrl?: string;
  userCode?: string;
  expiresAt: number;
};
export function newWalletLink(operationId: string): WalletLink {
  return { operationId, status: "pending", privateKey: Secp256k1.randomPrivateKey(), expiresAt: Date.now() + 300_000 };
}
export function publicWalletLink(link: WalletLink) {
  return { operation_id: link.operationId, status: link.status, expires_at: link.expiresAt,
    ...(link.status === "pending" && link.approvalUrl ? { approval_url: link.approvalUrl, user_code: link.userCode } : {}) };
}
export function publicLinkedWallet(wallet: LinkedWallet, originalAddress: string) {
  const authorization = verifiedAuthorization(wallet);
  return { address: wallet.address, created_at: wallet.createdAt, mode: "linked" as const,
    original_address: originalAddress, access_key: { address: authorization.address, chain_id: 4217,
      expiry: null, permissions: "full" as const, spending: "unlimited" as const, calls: "unrestricted" as const } };
}
/** Validate the signed payload, never just the wallet UI's grant metadata. */
export function verifiedAuthorization(wallet: LinkedWallet): KeyAuthorization.Signed {
  if (!ADDRESS.test(wallet.address) || !/^0x[0-9a-f]{64}$/i.test(wallet.privateKey)) throw new Error("invalid_linked_wallet");
  const authorization = KeyAuthorization.fromRpc(wallet.authorization);
  if (authorization.chainId !== 4217n || authorization.type !== "secp256k1"
    || authorization.address.toLowerCase() !== Account.fromSecp256k1(wallet.privateKey).address.toLowerCase()
    || (authorization.expiry != null && authorization.expiry !== 0)
    || authorization.limits !== undefined || authorization.scopes !== undefined
    || !SignatureEnvelope.verify(authorization.signature, {
      address: wallet.address, payload: KeyAuthorization.getSignPayload(authorization),
    })) throw new Error("invalid_linked_wallet_authorization");
  // Zero is the wallet request compatibility alias. Persist/use canonical absence.
  return { ...authorization, expiry: undefined };
}

/** SDK owns RFC8628, PKCE, RPC parsing and polling. It has no resume API: the
 * broker fences a lost live exchange as interrupted, never repeats registration. */
export async function pairWallet(link: WalletLink, signal: AbortSignal,
  onPrompt: (prompt: { approvalUrl: string; userCode: string; expiresAt: number }) => Promise<void>,
): Promise<LinkedWallet> {
  const privateKey = link.privateKey!;
  const provider = Provider.create({
    adapter: deviceCode({ name: "Tempo Wallet", rdns: "xyz.tempo.wallet", url: ENDPOINT,
      meta: { name: "Nanocodex", description: "Permanent access to spend tokens and call contracts. Disconnecting removes local custody; revoke access in Tempo Wallet." },
      methods: ["wallet_connect"], timeout: 300_000,
      fetch: async (input, init) => {
        signal.throwIfAborted();
        const url = String(input);
        if (![`${ENDPOINT}/register`, `${ENDPOINT}/token`].includes(url)) throw new Error("invalid_wallet_host");
        const response = await fetch(input, { ...init, redirect: "manual",
          signal: AbortSignal.any([signal, AbortSignal.timeout(10_000), ...(init?.signal ? [init.signal] : [])]) });
        if (response.status >= 300 && response.status < 400) throw new Error("invalid_wallet_redirect");
        const reader = response.body?.getReader();
        const chunks: Uint8Array[] = []; let size = 0;
        if (reader) try { while (true) { const part = await reader.read(); if (part.done) break;
          size += part.value.length; if (size > 32_768) throw new Error("wallet_response_too_large"); chunks.push(part.value); }
        } finally { await reader.cancel().catch(() => {}); }
        const bytes = new Uint8Array(size); let offset = 0;
        for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
        return new Response(bytes, { status: response.status, headers: { "content-type": "application/json" } });
      },
      async onPrompt(prompt) {
        const url = new URL(prompt.verificationUriFull ?? prompt.verificationUri);
        if (url.origin !== HOST || url.username || url.password || url.href.length > 2048
          || !/^[A-Za-z0-9-]{4,32}$/.test(prompt.userCode) || !Number.isFinite(prompt.expiresIn)
          || prompt.expiresIn <= 0) throw new Error("invalid_wallet_prompt");
        await onPrompt({ approvalUrl: url.href, userCode: prompt.userCode,
          expiresAt: Math.min(link.expiresAt, Date.now() + prompt.expiresIn * 1_000) });
      },
    }), chains: [tempo], storage: Storage.memory({ key: `link-${link.operationId}` }), mpp: false,
    transports: { [tempo.id]: http(RPC, { retryCount: 0, timeout: 5_000 }) },
  });
  const result = await provider.request({ method: "wallet_connect", params: [{ chainId: "0x1079",
    capabilities: { method: "login", authorizeAccessKey: { address: Account.fromSecp256k1(privateKey).address, keyType: "secp256k1", expiry: 0 } },
  }] } as never) as { accounts: { address: `0x${string}`; capabilities?: { keyAuthorization?: KeyAuthorization.Rpc } }[] };
  signal.throwIfAborted();
  if (result.accounts.length !== 1 || !result.accounts[0]?.capabilities?.keyAuthorization) throw new Error("invalid_wallet_grant");
  const wallet: LinkedWallet = { address: result.accounts[0].address.toLowerCase() as `0x${string}`, privateKey,
    authorization: result.accounts[0].capabilities.keyAuthorization, createdAt: Date.now(), operationId: link.operationId };
  wallet.authorization = KeyAuthorization.toRpc(verifiedAuthorization(wallet));
  return wallet;
}

/** Public-only root adapter cannot sign, authorize another key, or open a prompt.
 * The SDK resolves the encrypted delegated secret through its access-key manager. */
export async function linkedWalletProvider(wallet: LinkedWallet) {
  const authorization = verifiedAuthorization(wallet);
  const client = createClient({ chain: tempo, transport: http(RPC, { retryCount: 0, timeout: 5_000 }) });
  const metadata = await Actions.accessKey.getMetadata(client, { account: wallet.address, accessKey: authorization.address });
  if (metadata.isRevoked || (metadata.address.toLowerCase() === authorization.address.toLowerCase()
    && metadata.expiry !== 0xffffffffffffffffn)) throw new Error("linked_wallet_access_unavailable");
  // An absent key is publishable using the retained root-signed authorization.
  if (metadata.address.toLowerCase() !== authorization.address.toLowerCase()
    && metadata.address !== "0x0000000000000000000000000000000000000000") throw new Error("linked_wallet_access_unavailable");
  const provider = Provider.create({ adapter: Adapter.define({ name: "Linked Tempo Wallet" }, (config) => {
    const instance = local({ async loadAccounts() { return { accounts: [] }; } })(config);
    return { ...instance, actions: { ...instance.actions,
      async loadAccounts() { return { accounts: [{ address: wallet.address }] }; },
      async createAccount() { throw new Error("linked_wallet_cannot_create_account"); },
    } };
  }), chains: [tempo], storage: Storage.memory({ key: `linked-${wallet.operationId}` }), mpp: false,
    transports: { [tempo.id]: http(RPC, { retryCount: 0, timeout: 5_000 }) },
  });
  await provider.request({ method: "wallet_connect", params: [{ chainId: "0x1079", capabilities: { method: "login" } }] } as never);
  // The published SDK's Zustand middleware augmentation is absent from its .d.ts.
  const store = provider.store as unknown as { accessKeys: { add(value: {
    account: `0x${string}`; authorization: KeyAuthorization.Signed; privateKey: `0x${string}`;
  }): unknown } };
  store.accessKeys.add({ account: wallet.address, authorization, privateKey: wallet.privateKey });
  return provider;
}
