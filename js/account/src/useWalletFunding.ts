import { useActiveWallet } from "./useActiveWallet";
import { useQueryClient } from "@tanstack/react-query";
import { refreshAccountResource } from "./accountQueries";
import { useAccountQuery } from "./useAccountQuery";
import { useCallback, useEffect, useRef, useState } from "react";
import { isRecord, responseFailure, useAccountSession } from "./AccountSession";
import { clientFailureMessage } from "./clientFailure";
import {
  classifyFundingOrder,
  decodeFundingAttempt,
  decodeMachineUsdConfig,
  defaultFundingAmountCents,
} from "./walletFunding";

type WalletFundingOperation = "prepare" | "payment";

type WalletFundingRun = Readonly<{
  accountId: string;
  controller: AbortController;
}>;

export function useWalletFunding(enabled: boolean) {
  const session = useAccountSession();
  const queryClient = useQueryClient();
  const accountId = session.account?.id;
  const activeWallet = useActiveWallet(enabled);
  const address = activeWallet.wallet?.address;
  const refreshSession = session.refresh;
  const [operationError, setError] = useState<string | null>(null);
  const [operation, setOperation] = useState<WalletFundingOperation | null>(null);
  const [checkoutUrl, setCheckoutUrl] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const fundingRun = useRef<WalletFundingRun | undefined>(undefined);

  const cancel = useCallback(() => {
    const run = fundingRun.current;
    fundingRun.current = undefined;
    run?.controller.abort();
  }, []);

  const { query: configQuery } = useAccountQuery(accountId, "/v1/machine-usd/config", decodeMachineUsdConfig, { enabled, staleTime: 5 * 60_000 });
  const config = configQuery.data ?? null;
  const error = operationError ?? (activeWallet.error ? "Couldn’t verify the active wallet. Reload before adding funds." : null) ?? (configQuery.error ? "Adding funds is temporarily unavailable. Please try again later." : null);

  useEffect(() => {
    cancel();
    setOperation(null);
    setError(null);
    setCheckoutUrl(null);
    setMessage(null);
  }, [accountId, address, cancel, enabled]);

  useEffect(() => cancel, [cancel]);

  const fund = useCallback(() => {
    if (!enabled || !accountId || !address || !config || !config.onrampEnabled || operation || fundingRun.current) return;
    const controller = new AbortController();
    const run: WalletFundingRun = { accountId, controller };
    cancel();
    fundingRun.current = run;
    setOperation("prepare");
    setError(null);
    setMessage(null);
    setCheckoutUrl(null);
    void (async () => {
      try {
        const storageKey = `nanocodex:mach-funding:${accountId}:${address.toLowerCase()}`;
        const retained = sessionStorage.getItem(storageKey);
        const newIntent = () => ({ accountId, address: address.toLowerCase(), orderToken: randomOrderToken(), idempotencyKey: crypto.randomUUID(), amountCents: defaultFundingAmountCents(config) });
        const intent: RetainedIntent = retained ? decodeRetainedIntent(retained, accountId, address) : newIntent();
        const readStatus = async (id: string) => {
          const status = await apiRequest(`/v1/machine-usd/orders/${encodeURIComponent(id)}`, {
            headers: { authorization: `Bearer ${intent.orderToken}` }, signal: controller.signal,
          });
          if (!status.ok) throw await responseFailure(status, "Couldn’t check the funding order. Try again to resume it.");
          const value: unknown = await status.json();
          if (!isRecord(value) || !isRecord(value.order) || value.order.id !== id
            || typeof value.order.wallet_address !== "string" || value.order.wallet_address.toLowerCase() !== address.toLowerCase()
            || value.order.usd_amount_cents !== intent.amountCents
            || value.order.mach_amount_atomics !== intent.amountCents * 10_000) throw new Error("The funding order did not match this wallet.");
          return classifyFundingOrder(value.order);
        };
        const finish = async (state: "complete" | "failed") => {
          if (fundingRun.current !== run) return;
          sessionStorage.removeItem(storageKey);
          setCheckoutUrl(null);
          if (state === "failed") throw new Error("Funding failed. You can try adding funds again.");
          setMessage("Funds added to your Wallet.");
          await refreshAccountResource(queryClient, accountId, "/v1/wallet/balance");
        };
        if (intent.id) {
          const state = await readStatus(intent.id);
          if (state !== "pending") { await finish(state); return; }
        }
        // Persist before dispatch; an uncertain create must reuse these exact inputs.
        sessionStorage.setItem(storageKey, JSON.stringify(intent));
        const { orderToken } = intent;
        const response = await apiRequest("/v1/machine-usd/orders", {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "idempotency-key": intent.idempotencyKey,
          },
          body: JSON.stringify({
            order_token: orderToken,
            payment_mode: "hosted_checkout",
            usd_amount_cents: intent.amountCents,
            wallet_address: address,
          }),
          signal: controller.signal,
        });
        if (response.status === 401) {
          await response.body?.cancel();
          await refreshSession();
          return;
        }
        if (!response.ok) {
          throw await responseFailure(response, "Couldn’t create the Wallet funding order.");
        }
        const attempt = decodeFundingAttempt(await response.json(), orderToken, address, intent.amountCents);
        if (fundingRun.current !== run) return;
        sessionStorage.setItem(storageKey, JSON.stringify({ ...intent, id: attempt.id }));
        setOperation("payment");
        setCheckoutUrl(attempt.checkoutUrl);
        // Keep this page alive: the hosted checkout returns to the issuer, not Nanocodex.
        // Neither opening nor closing its tab proves payment or token issuance.
        for (let poll = 0; poll < 450; poll += 1) {
          await pause(2000, controller.signal);
          const state = await readStatus(attempt.id);
          if (state !== "pending") { await finish(state); return; }
        }
        throw new Error("Funding is still pending. Try again to resume this order.");
      } catch (cause) {
        if (!controller.signal.aborted && fundingRun.current === run) {
          setError(clientFailureMessage(cause, "Wallet funding did not complete."));
        }
      } finally {
        if (fundingRun.current === run) {
          fundingRun.current = undefined;
          setOperation(null);
        }
      }
    })();
  }, [accountId, address, cancel, config, enabled, operation, queryClient, refreshSession]);

  return {
    checkoutUrl,
    message,
    amountCents: config ? defaultFundingAmountCents(config) : 500,
    available: Boolean(address) && config?.onrampEnabled === true,
    error,
    errorSource: operationError ? "order" : configQuery.error ? "configuration" : null,
    fund,
    loading: configQuery.isLoading || activeWallet.loading,
    operation,
  } as const;
}

function randomOrderToken(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

async function apiRequest(path: string, init: RequestInit = {}): Promise<Response> {
  return fetch(path, {
    ...init,
    cache: "no-store",
    credentials: "same-origin",
    headers: {
      accept: "application/json",
      ...Object.fromEntries(new Headers(init.headers)),
    },
  });
}


type RetainedIntent = { accountId: string; address: string; orderToken: string; idempotencyKey: string; amountCents: number; id?: string };

function decodeRetainedIntent(raw: string, accountId: string, address: string): RetainedIntent {
  let value: unknown;
  try { value = JSON.parse(raw); } catch { throw new Error("The saved funding order cannot be recovered. Contact support before starting another payment."); }
  if (!isRecord(value) || Object.keys(value).some(key => !["accountId", "address", "orderToken", "idempotencyKey", "amountCents", "id"].includes(key))
    || value.accountId !== accountId || value.address !== address.toLowerCase()
    || typeof value.orderToken !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(value.orderToken)
    || typeof value.idempotencyKey !== "string" || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value.idempotencyKey)
    || !Number.isSafeInteger(value.amountCents) || (value.amountCents as number) < 500 || (value.amountCents as number) > 10_000
    || (value.id !== undefined && (typeof value.id !== "string" || !/^ord_[0-9a-f]{32}$/.test(value.id)))) {
    throw new Error("The saved funding order does not match this account. Contact support before starting another payment.");
  }
  return value as RetainedIntent;
}

function pause(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) { reject(signal.reason); return; }
    const abort = () => { clearTimeout(timer); reject(signal.reason); };
    const timer = setTimeout(() => { signal.removeEventListener("abort", abort); resolve(); }, ms);
    signal.addEventListener("abort", abort, { once: true });
  });
}
