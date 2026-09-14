import { useAccountQuery } from "./useAccountQuery";
import { useCallback, useEffect, useRef, useState } from "react";
import { responseFailure, useAccountSession } from "./AccountSession";
import { clientFailureMessage } from "./clientFailure";
import {
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
  const accountId = session.account?.id;
  const address = session.account?.address;
  const refreshSession = session.refresh;
  const [operationError, setError] = useState<string | null>(null);
  const [operation, setOperation] = useState<WalletFundingOperation | null>(null);
  const fundingRun = useRef<WalletFundingRun | undefined>(undefined);

  const cancel = useCallback(() => {
    const run = fundingRun.current;
    fundingRun.current = undefined;
    run?.controller.abort();
  }, []);

  const { query: configQuery } = useAccountQuery(accountId, "/v1/machine-usd/config", decodeMachineUsdConfig, { enabled, staleTime: 5 * 60_000 });
  const config = configQuery.data ?? null;
  const error = operationError ?? (configQuery.error ? clientFailureMessage(configQuery.error, "Couldn’t load Wallet funding.") : null);

  useEffect(() => {
    cancel();
    setOperation(null);
    setError(null);
  }, [accountId, cancel, enabled]);

  useEffect(() => cancel, [cancel]);

  const fund = useCallback(() => {
    if (!accountId || !address || !config || !config.onrampEnabled || operation) return;
    const controller = new AbortController();
    const run: WalletFundingRun = { accountId, controller };
    cancel();
    fundingRun.current = run;
    setOperation("prepare");
    setError(null);
    void (async () => {
      try {
        const orderToken = randomOrderToken();
        const response = await apiRequest("/v1/machine-usd/orders", {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "idempotency-key": crypto.randomUUID(),
          },
          body: JSON.stringify({
            order_token: orderToken,
            payment_mode: "hosted_checkout",
            usd_amount_cents: defaultFundingAmountCents(config),
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
        const attempt = decodeFundingAttempt(await response.json(), orderToken);
        if (fundingRun.current !== run) return;
        setOperation("payment");
        window.location.assign(attempt.checkoutUrl);
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
  }, [accountId, address, cancel, config, operation, refreshSession]);

  return {
    amountCents: config ? defaultFundingAmountCents(config) : 500,
    available: config?.onrampEnabled === true,
    error,
    fund,
    loading: configQuery.isLoading,
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
