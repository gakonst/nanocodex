import { useEffect, useState } from "react";
import { Copy } from "lucide-react";
import { ConnectionLogo } from "nanocodex-connect-ui/ConnectionLogo";
import { useActiveWallet, useActiveWalletBalance } from "./useActiveWallet";
import { useWalletFunding } from "./useWalletFunding";
import { useWalletLink } from "./useWalletLink";
import { formatDollars } from "./walletFunding";

export function TempoWalletConnectionCard({
  address, mode, accessKeyAddress, walletLink, walletError, balance, fundingAmountCents, fundingAvailable, fundingError,
  fundingOperation, fundingLoading, fundingErrorSource, checkoutUrl, fundingMessage, onFund,
}: Readonly<{
  address?: string;
  mode?: "internal" | "linked";
  accessKeyAddress?: string;
  walletLink?: ReturnType<typeof useWalletLink>;
  walletError?: string | null;
  balance: string;
  fundingAmountCents: number;
  fundingAvailable: boolean;
  fundingError: string | null;
  fundingErrorSource?: "order" | "configuration" | null;
  fundingOperation: "prepare" | "payment" | null;
  fundingLoading: boolean;
  checkoutUrl?: string | null;
  fundingMessage?: string | null;
  onFund(): void;
}>) {
  const [copyResult, setCopyResult] = useState<{ address: string; status: "copied" | "failed" } | null>(null);
  const copyStatus = copyResult?.address === address ? copyResult?.status : null;
  useEffect(() => setCopyResult(null), [address]);
  const busy = fundingOperation !== null;
  const [confirmLink, setConfirmLink] = useState(false);
  const [confirmUnlink, setConfirmUnlink] = useState(false);
  async function copyAddress() {
    if (!address) return;
    try {
      await navigator.clipboard.writeText(address);
      setCopyResult({ address, status: "copied" });
    } catch {
      setCopyResult({ address, status: "failed" });
    }
  }
  const balanceReassurance = address ? " This does not affect your wallet address or existing balance." : "";
  const fundingStatus = fundingOperation === "prepare"
    ? "Preparing secure checkout…"
    : fundingOperation === "payment"
      ? "Open Stripe checkout in a new tab. Keep this page open while we check for funds."
      : fundingError
        ? fundingErrorSource === "order"
          ? fundingError
          : `Adding funds is temporarily unavailable.${balanceReassurance} Please try again later.`
        : fundingLoading
          ? "Checking funding availability…"
          : !fundingAvailable
            ? `Adding funds is currently unavailable.${balanceReassurance}`
            : fundingMessage;
  return (
    <div className="wizard-connector-card tempo-wallet-connection" id="wallet" role="listitem">
      <div className={`connection-card tempo-wallet-card${address ? " is-connected" : " is-unavailable"}`}>
        <ConnectionLogo id="tempo" />
        <div className="connection-card-copy">
          <strong>Wallet</strong>
          <span className="tempo-wallet-balance">Balance: {balance}</span>
          {fundingStatus ? <span className="tempo-wallet-message" role="status">{fundingStatus}</span> : null}
        </div>
        <span className="tempo-wallet-card-actions">
          <button disabled={!address || !fundingAvailable || fundingLoading || busy || Boolean(walletLink?.intent)} onClick={onFund} type="button">
            {busy ? "Checking funding…" : `Add ${formatDollars(fundingAmountCents)}`}
          </button>
          {checkoutUrl ? <a href={checkoutUrl} target="_blank" rel="noopener noreferrer">Open Stripe checkout</a> : null}
        </span>
          {address ? <div className="tempo-wallet-address">
            <span>{mode === "linked" ? "Linked Tempo wallet address" : "Wallet address"}</span>
            <code>{address}</code>
            <button aria-label="Copy wallet address" onClick={() => void copyAddress()} type="button"><Copy size={14} /> Copy address</button>
            {copyStatus === "copied" ? <span role="status">Address copied</span> : null}
            {copyStatus === "failed" ? <span role="alert">Couldn’t copy. Select the address above to copy it manually.</span> : null}
          </div> : null}
        {walletError ? <p role="alert">{walletError}</p> : null}
        {walletLink ? <div className="tempo-wallet-address">
          {walletLink.error ? <p role="alert">{walletLink.error}</p> : null}
          {walletLink.message ? <p role="status">{walletLink.message}</p> : null}
          {walletLink.approval ? <>
            <p>Confirm code <strong>{walletLink.approval.user_code}</strong> in Tempo Wallet.</p>
            <a href={walletLink.approval.approval_url} target="_blank" rel="noopener noreferrer">Open Tempo Wallet</a>
          </> : null}
          {walletLink.intent ? <>
            {!walletLink.busy ? <button type="button" onClick={walletLink.resume}>Resume wallet request</button> : null}
            {walletLink.intent.kind === "link" ? <button type="button" onClick={walletLink.cancel}>Cancel wallet link</button> : null}
          </> : mode === "linked" ? <>
            <p>Approved access: unlimited spending and all contract calls, with no expiry. Revocation is checked before signing.</p>
            {accessKeyAddress ? <><span>Access key to revoke in Tempo Wallet</span><code>{accessKeyAddress}</code></> : null}
            {confirmUnlink ? <>
              <p>Disconnecting switches back to your original wallet, whose funds are retained. This only disconnects Nanocodex locally; it does not revoke the onchain access key. Revoke that key in Tempo Wallet.</p>
              <button type="button" disabled={busy || !address} onClick={() => { if (address) walletLink.unlink(address); setConfirmUnlink(false); }}>Disconnect linked wallet</button>
              <button type="button" onClick={() => setConfirmUnlink(false)}>Keep linked wallet</button>
            </> : <button type="button" disabled={busy} onClick={() => setConfirmUnlink(true)}>Unlink Tempo Wallet</button>}
          </> : confirmLink ? <>
            <p>Link your existing Tempo wallet to let Nanocodex spend without limits and make all contract calls. The access key has no expiry and remains authorized until you revoke it in Tempo Wallet.</p>
            <p>Funding and spending will switch to the linked address. Your original wallet and its funds are retained; your Nanocodex sign-in stays the same.</p>
            <button type="button" disabled={busy || !address} onClick={() => { setConfirmLink(false); walletLink.start(); }}>Continue to Tempo Wallet</button>
            <button type="button" onClick={() => setConfirmLink(false)}>Keep current wallet</button>
          </> : <button type="button" disabled={busy || !address || Boolean(walletLink.error)} onClick={() => setConfirmLink(true)}>Link Tempo Wallet</button>}
        </div> : null}
      </div>
    </div>
  );
}

/** Shared production wiring for account settings and the browser wallet journey. */
export function ActiveTempoWalletConnectionCard({ enabled }: { enabled: boolean }) {
  const activeWallet = useActiveWallet(enabled);
  const walletLink = useWalletLink(enabled);
  const funding = useWalletFunding(enabled);
  const address = activeWallet.wallet?.address;
  const balance = useActiveWalletBalance(enabled, address);
  return <TempoWalletConnectionCard
    address={address} mode={activeWallet.wallet?.mode} accessKeyAddress={activeWallet.wallet?.access_key?.address} walletLink={walletLink}
    walletError={activeWallet.error ? "Couldn’t verify the active wallet. Reload to try again." : null}
    balance={balance} fundingAmountCents={funding.amountCents}
    fundingAvailable={funding.available} fundingError={funding.error}
    fundingErrorSource={funding.errorSource} fundingOperation={funding.operation}
    checkoutUrl={funding.checkoutUrl} fundingMessage={funding.message}
    fundingLoading={funding.loading} onFund={funding.fund}
  />;
}
