import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { isRecord, responseFailure, useAccountSession } from "./AccountSession";
import { accountResourceKey, refreshAccountResource } from "./accountQueries";
import { clientFailureMessage } from "./clientFailure";
import { decodeActiveWallet } from "./useActiveWallet";

type Intent = { operation_id: string; kind: "link" | "unlink"; expected_address?: string; cancel_requested?: boolean };
type Approval = { approval_url: string; user_code: string };
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export function useWalletLink(enabled: boolean) {
  const accountId = useAccountSession().account?.id;
  const client = useQueryClient();
  const key = accountId ? `nanocodex:wallet-link:${accountId}` : undefined;
  const [intent, setIntent] = useState<Intent | null>(null);
  const [approval, setApproval] = useState<Approval | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const active = useRef<AbortController | null>(null);
  const identity = useRef(key);
  identity.current = key;

  const run = useCallback(async (next: Intent, action: "start" | "poll" | "cancel" | "unlink") => {
    if (!key || !accountId || active.current) return;
    const controller = new AbortController();
    active.current = controller;
    const current = () => !controller.signal.aborted && identity.current === key;
    setBusy(true); setError(null); setMessage(null);
    const refresh = async () => {
      // Cancel an old balance response before changing the wallet selection.
      await client.cancelQueries({ queryKey: accountResourceKey(accountId, "/v1/wallet/balance"), exact: true });
      client.removeQueries({ queryKey: accountResourceKey(accountId, "/v1/wallet/balance"), exact: true });
      const wallet = await refreshAccountResource(client, accountId, "/v1/wallet");
      const verified = wallet === undefined ? undefined : decodeActiveWallet(wallet);
      if (verified === undefined) throw new Error("Couldn’t verify the active wallet. Resume to check the same request.");
      await client.invalidateQueries({ queryKey: accountResourceKey(accountId, "/v1/wallet/balance"), exact: true });
      return verified;
    };
    try {
      localStorage.setItem(key, JSON.stringify(next));
      setIntent(next);
      let path = action === "unlink" ? "/v1/wallet/unlink" : action === "start" ? "/v1/wallet/link" : `/v1/wallet/link/${action}`;
      while (current()) {
        const response = await fetch(path, { method: "POST", credentials: "same-origin", cache: "no-store", signal: controller.signal,
          headers: { "content-type": "application/json", accept: "application/json" },
          body: JSON.stringify({ operation_id: next.operation_id, ...(next.kind === "unlink" ? { expected_address: next.expected_address } : {}) }) });
        if (!response.ok) throw await responseFailure(response, "Couldn’t check the wallet connection. Resume to check the same request.");
        const value: unknown = await response.json();
        if (!current()) return;
        if (!isRecord(value) || value.operation_id !== next.operation_id || typeof value.status !== "string") throw new Error("The wallet connection response is invalid. Resume to check the same request.");
        if (value.status === "pending") {
          if (typeof value.approval_url === "string") {
            const url = new URL(value.approval_url);
            if (url.origin !== "https://wallet.tempo.xyz" || url.username || url.password || typeof value.user_code !== "string") throw new Error("The wallet approval destination is invalid. Cancel this request.");
            setApproval({ approval_url: url.href, user_code: value.user_code });
          }
          if (action === "cancel") throw new Error("Cancellation is still pending. Resume to check its status.");
          setMessage(typeof value.approval_url === "string" ? "Waiting for approval in Tempo Wallet. Closing its tab does not cancel this request." : "Preparing the Tempo Wallet approval…");
          await new Promise<void>((resolve, reject) => {
            const abort = () => { clearTimeout(timer); reject(new Error("Interrupted")); };
            const timer = setTimeout(() => { controller.signal.removeEventListener("abort", abort); resolve(); }, 2000);
            controller.signal.addEventListener("abort", abort, { once: true });
          });
          path = "/v1/wallet/link/poll";
          continue;
        }
        if (!["linked", "unlinked", "cancelled", "rejected", "expired", "interrupted"].includes(value.status)) throw new Error("The wallet connection response is invalid.");
        const wallet = await refresh();
        if ((value.status === "linked" && wallet.mode !== "linked") || (value.status === "unlinked" && wallet.mode !== "internal")) {
          throw new Error("The active wallet has not confirmed this change. Resume to check the same request.");
        }
        if (!current()) return;
        localStorage.removeItem(key); setIntent(null); setApproval(null);
        setMessage(value.status === "linked" ? "Tempo Wallet linked. Funding and spending now use the linked address. Your original wallet and its funds are retained."
          : value.status === "unlinked" ? "Wallet disconnected locally. Your original wallet is active again. Revoke the access key in Tempo Wallet to revoke its onchain permission."
          : value.status === "interrupted" ? "The wallet request was interrupted. Start a new link for a fresh approval. Check Tempo Wallet for any previously approved access key."
          : `Wallet link ${value.status}. Your active wallet is unchanged.`);
        return;
      }
    } catch (cause) {
      if (current()) setError(clientFailureMessage(cause, "Couldn’t finish the wallet connection. Resume the same request."));
    } finally {
      if (active.current === controller) { active.current = null; if (current()) setBusy(false); }
    }
  }, [accountId, client, key]);

  useEffect(() => {
    active.current?.abort(); active.current = null;
    setIntent(null); setApproval(null); setBusy(false); setMessage(null); setError(null);
    if (key && enabled) {
      try {
        const raw = localStorage.getItem(key);
        if (raw) {
          const saved: unknown = JSON.parse(raw);
          if (!isRecord(saved) || typeof saved.operation_id !== "string" || !uuid.test(saved.operation_id)
            || !["link", "unlink"].includes(String(saved.kind))
            || (saved.cancel_requested !== undefined && (saved.kind !== "link" || saved.cancel_requested !== true))
            || (saved.kind === "unlink" && (typeof saved.expected_address !== "string" || !/^0x[0-9a-f]{40}$/i.test(saved.expected_address)))) throw new Error("The saved wallet request cannot be recovered.");
          const next = saved as Intent;
          setIntent(next);
          void run(next, next.kind === "unlink" ? "unlink" : next.cancel_requested ? "cancel" : "start");
        }
      } catch { setError("The saved wallet request cannot be recovered. Contact support before starting another connection."); }
    }
    return () => { active.current?.abort(); active.current = null; };
  }, [enabled, key, run]);

  return {
    intent, approval, busy, message, error,
    start: () => { if (!intent && !error) void run({ operation_id: crypto.randomUUID(), kind: "link" }, "start"); },
    resume: () => { if (intent) void run(intent, intent.kind === "unlink" ? "unlink" : intent.cancel_requested ? "cancel" : "start"); },
    cancel: () => {
      if (!intent || intent.kind !== "link") return;
      active.current?.abort(); active.current = null;
      void run({ ...intent, cancel_requested: true }, "cancel");
    },
    unlink: (address: string) => { if (!intent) void run({ operation_id: crypto.randomUUID(), kind: "unlink", expected_address: address }, "unlink"); },
  };
}
