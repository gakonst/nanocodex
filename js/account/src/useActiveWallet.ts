import { useCallback } from "react";
import { isRecord, useAccountSession } from "./AccountSession";
import { useAccountQuery } from "./useAccountQuery";
import { decodeWalletBalance, formatWalletBalance } from "./walletFunding";

export type ActiveWallet = Readonly<{
  address: string;
  mode: "internal" | "linked";
  original_address: string;
  access_key?: { address: string };
}>;

export function decodeActiveWallet(value: unknown): ActiveWallet {
  if (!isRecord(value) || typeof value.address !== "string" || !/^0x[0-9a-f]{40}$/i.test(value.address)
    || (value.mode !== "internal" && value.mode !== "linked")
    || typeof value.original_address !== "string" || !/^0x[0-9a-f]{40}$/i.test(value.original_address)) {
    throw new Error("Couldn’t verify the active wallet.");
  }
  if (value.mode === "linked" && (!isRecord(value.access_key) || value.access_key.expiry !== null || value.access_key.permissions !== "full"
    || typeof value.access_key.address !== "string" || !/^0x[0-9a-f]{40}$/i.test(value.access_key.address))) {
    throw new Error("Couldn’t verify the linked wallet permissions.");
  }
  return { address: value.address, mode: value.mode, original_address: value.original_address,
    ...(value.mode === "linked" && isRecord(value.access_key) ? { access_key: { address: String(value.access_key.address) } } : {}),
  };
}

export function useActiveWallet(enabled: boolean) {
  const accountId = useAccountSession().account?.id;
  const { query } = useAccountQuery(accountId, "/v1/wallet", decodeActiveWallet, { enabled, staleTime: 0 });
  // A failed refresh must not expose a previously active address for funding.
  const wallet = query.error ? undefined : query.data;
  return { wallet, error: query.error, loading: query.isLoading };
}

export function useActiveWalletBalance(enabled: boolean, address?: string) {
  const accountId = useAccountSession().account?.id;
  const select = useCallback((value: unknown) => decodeWalletBalance(value, address ?? ""), [address]);
  const { query } = useAccountQuery(accountId, "/v1/wallet/balance", select, {
    enabled: enabled && Boolean(address), staleTime: 30_000, refetchInterval: enabled ? 5 * 60_000 : false,
  });
  // React Query can retain previously selected data when a new selector fails.
  const balance = address && query.data?.account.toLowerCase() === address.toLowerCase() ? query.data : undefined;
  return balance ? `${formatWalletBalance(balance)}${query.error ? " · refresh failed" : ""}`
    : query.error ? "Balance unavailable" : "Loading balance…";
}
