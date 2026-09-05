import { KeyRound, LockKeyhole, Plus, Trash2, X } from "lucide-react";
import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type FormEvent,
  type InputHTMLAttributes,
  type ReactNode,
  type RefObject,
} from "react";

import { AccountChooser } from "nanocodex-connect-ui/AccountChooser";
import { responseFailure, useAccountSession } from "./AccountSession";
import { clientFailureMessage } from "./clientFailure";
import { useModalBoundary } from "./modalBoundary";
import { SshIdentityManager } from "./SshIdentityManager";
import { decodeSshIdentities, type SshIdentityMetadata } from "./sshIdentities";
import {
  decodeVaultEntries,
  vaultEntryPath,
  type VaultEntryKind,
  type VaultEntryMetadata,
} from "./vaultEntries";

type VaultStatus = Readonly<{
  ssh: readonly SshIdentityMetadata[];
  entries: readonly VaultEntryMetadata[];
}>;

const sections: readonly Readonly<{
  kind: VaultEntryKind;
  title: string;
  addLabel: string;
}>[] = [
  { kind: "login", title: "Logins", addLabel: "Add login" },
  { kind: "card", title: "Cards", addLabel: "Add card" },
  { kind: "address", title: "Addresses", addLabel: "Add address" },
  { kind: "phone", title: "Phones", addLabel: "Add phone" },
];

export function Vault() {
  const session = useAccountSession();
  const refreshSession = session.refresh;
  const accountId = session.account?.persistent ? session.account.id : undefined;
  const [status, setStatus] = useState<VaultStatus | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [operation, setOperation] = useState<string | null>(null);
  const [adding, setAdding] = useState<VaultEntryKind | null>(null);
  const dialogReturnFocusRef = useRef<HTMLElement | null>(null);
  const closeDialog = useCallback(() => setAdding(null), []);

  const load = useCallback(async () => {
    if (!accountId) return;
    setFailure(null);
    try {
      const response = await vaultRequest("/v1/credentials");
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        return;
      }
      if (!response.ok) throw await responseFailure(response, "Couldn’t load your vault.");
      const value: unknown = await response.json();
      if (!isRecord(value)) throw new Error("Invalid vault response.");
      setStatus({
        ssh: decodeSshIdentities(value.ssh),
        entries: decodeVaultEntries(value.vault),
      });
    } catch (cause) {
      setFailure(clientFailureMessage(cause, "Couldn’t load your vault."));
    }
  }, [accountId, refreshSession]);

  useEffect(() => {
    setStatus(null);
    setFailure(null);
    setAdding(null);
    if (accountId) void load();
  }, [accountId, load]);

  const save = async (kind: VaultEntryKind, values: Record<string, string>) => {
    if (operation) return;
    setOperation(`add:${kind}`);
    setFailure(null);
    try {
      const response = await vaultRequest(vaultEntryPath(kind), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(values),
      });
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        return;
      }
      if (!response.ok) throw await responseFailure(response, `Couldn’t add the ${kind}.`);
      const entry = decodeVaultEntries([await response.json()])[0]!;
      setStatus((current) => current ? {
        ...current,
        entries: [entry, ...current.entries.filter((candidate) => candidate.id !== entry.id)],
      } : current);
      setAdding(null);
    } catch (cause) {
      setFailure(clientFailureMessage(cause, `Couldn’t add the ${kind}. Check every field and try again.`));
    } finally {
      setOperation(null);
    }
  };

  const remove = async (entry: VaultEntryMetadata) => {
    if (operation) return;
    setOperation(entry.id);
    setFailure(null);
    try {
      const response = await vaultRequest(vaultEntryPath(entry.kind, entry.id), { method: "DELETE" });
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        return;
      }
      if (!response.ok) throw await responseFailure(response, "Couldn’t delete the vault item.");
      setStatus((current) => current ? {
        ...current,
        entries: current.entries.filter((candidate) => candidate.id !== entry.id),
      } : current);
    } catch (cause) {
      setFailure(clientFailureMessage(cause, "Couldn’t delete the vault item."));
    } finally {
      setOperation(null);
    }
  };

  if (session.status === "checking") return null;
  if (!accountId) {
    return (
      <div className="wizard-page wizard-account-page vault-sign-in">
        <header className="wizard-intro">
          <div className="wizard-app">
            <h1>Vault</h1>
            <p>Verify your phone before storing encrypted credentials and personal details.</p>
          </div>
        </header>
        <AccountChooser
          description={session.reauthenticationRequired
            ? "Your session expired. Enter your phone number to restore your vault."
            : "Enter your phone number to create or restore your Nanocodex account."}
          disabled={session.operation !== null}
          failure={session.error}
          onChooseAccount={(selection) => void session.chooseAccount(selection)}
        />
      </div>
    );
  }

  return (
    <div className="vault-page">
      <div className="vault-content">
        <header className="vault-heading">
          <div>
            <span>Private broker</span>
            <h1>Vault</h1>
          </div>
          <p>Passwords and full card details stay encrypted; only safe identifiers remain available after saving.</p>
        </header>

        {session.error || failure ? (
          <div className="account-failure vault-failure" role="alert">
            <p>{session.error ?? failure}</p>
            <button type="button" onClick={() => void load()}>Retry</button>
          </div>
        ) : null}

        <div className="vault-ssh">
          <SshIdentityManager
            disabled={operation !== null}
            identities={status?.ssh ?? null}
            onChanged={load}
            presentation="wizard"
            refreshSession={refreshSession}
            title="SSH keys"
          />
        </div>

        {sections.map((section) => {
          const entries = status?.entries.filter((entry) => entry.kind === section.kind) ?? [];
          return (
            <section className="vault-section" aria-labelledby={`vault-${section.kind}-title`} key={section.kind}>
              <div className="vault-section-heading">
                <div>
                  <span>Personal data</span>
                  <h2 id={`vault-${section.kind}-title`}>{section.title}</h2>
                </div>
                <small>{status ? `${entries.length} saved` : "Loading"}</small>
              </div>
              {entries.length ? (
                <ul className="vault-entry-list">
                  {entries.map((entry) => (
                    <li key={entry.id}>
                      <div className="vault-entry-icon" aria-hidden="true"><LockKeyhole /></div>
                      <div>
                        <strong>{entry.name}</strong>
                        <span>{labelForKind(entry.kind)} · encrypted</span>
                      </div>
                      <button
                        aria-label={`Delete ${entry.name}`}
                        disabled={operation !== null}
                        onClick={() => void remove(entry)}
                        type="button"
                      ><Trash2 aria-hidden="true" /></button>
                    </li>
                  ))}
                </ul>
              ) : null}
              <button
                className="vault-add"
                disabled={!status || operation !== null}
                onClick={(event) => {
                  dialogReturnFocusRef.current = event.currentTarget;
                  setAdding(section.kind);
                }}
                type="button"
              >
                <Plus aria-hidden="true" />
                {section.addLabel}
              </button>
            </section>
          );
        })}
      </div>
      {adding ? (
        <VaultEntryDialog
          busy={operation !== null}
          kind={adding}
          onClose={closeDialog}
          onSave={save}
          returnFocusRef={dialogReturnFocusRef}
        />
      ) : null}
    </div>
  );
}

function VaultEntryDialog({
  busy,
  kind,
  onClose,
  onSave,
  returnFocusRef,
}: Readonly<{
  busy: boolean;
  kind: VaultEntryKind;
  onClose(): void;
  onSave(kind: VaultEntryKind, values: Record<string, string>): Promise<void>;
  returnFocusRef: RefObject<HTMLElement | null>;
}>) {
  const titleId = useId();
  const backdropRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLElement>(null);
  const firstInputRef = useRef<HTMLInputElement>(null);
  const dismiss = useCallback(() => {
    if (!busy) onClose();
  }, [busy, onClose]);
  useModalBoundary({
    backdropRef,
    initialFocusRef: firstInputRef,
    onDismiss: dismiss,
    open: true,
    panelRef: dialogRef,
    returnFocusRef,
  });

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const data = new FormData(event.currentTarget);
    const values = Object.fromEntries(
      [...data.entries()].flatMap(([key, value]) => typeof value === "string" && value.trim()
        ? [[key, key === "password" ? value : value.trim()]]
        : []),
    );
    void onSave(kind, values);
  };

  return (
    <div className="vault-dialog-backdrop" ref={backdropRef} onMouseDown={(event) => {
      if (event.target === event.currentTarget && !busy) onClose();
    }}>
      <section aria-labelledby={titleId} aria-modal="true" className="vault-dialog" ref={dialogRef} role="dialog">
        <header>
          <div>
            <h2 id={titleId}>Add {labelForKind(kind).toLowerCase()}</h2>
            <p>Values are encrypted in your vault.</p>
          </div>
          <button aria-label="Close" disabled={busy} onClick={onClose} type="button"><X aria-hidden="true" /></button>
        </header>
        <form onSubmit={submit}>
          <div className="vault-dialog-fields">
            <VaultField autoComplete="off" inputRef={firstInputRef} label="Name" maxLength={120} name="name" placeholder={namePlaceholder(kind)} required />
            {fieldsForKind(kind)}
          </div>
          <footer>
            <button disabled={busy} onClick={onClose} type="button">Cancel</button>
            <button className="vault-save" disabled={busy} type="submit">{busy ? "Saving…" : "Save"}</button>
          </footer>
        </form>
      </section>
    </div>
  );
}

function fieldsForKind(kind: VaultEntryKind): ReactNode {
  if (kind === "login") return <>
    <VaultField autoCapitalize="none" autoComplete="username" label="Username" maxLength={512} name="username" required spellCheck={false} />
    <VaultField autoComplete="new-password" label="Password" maxLength={8192} name="password" required type="password" secure />
  </>;
  if (kind === "card") return <>
    <VaultField autoComplete="cc-number" inputMode="numeric" label="Card number" maxLength={23} name="card_number" required secure />
    <div className="vault-field-row">
      <VaultField autoComplete="cc-exp-month" inputMode="numeric" label="Expiry month" maxLength={2} name="expiry_month" pattern="(?:0?[1-9]|1[0-2])" required />
      <VaultField autoComplete="cc-exp-year" inputMode="numeric" label="Expiry year" maxLength={4} minLength={4} name="expiry_year" pattern="[0-9]{4}" required />
    </div>
    <div className="vault-field-row">
      <VaultField autoComplete="cc-csc" inputMode="numeric" label="CVV" maxLength={4} minLength={3} name="cvv" pattern="[0-9]{3,4}" required secure type="password" />
      <VaultField autoComplete="postal-code" label="Billing ZIP" maxLength={32} name="billing_zip" required />
    </div>
  </>;
  if (kind === "address") return <>
    <VaultField autoComplete="address-line1" label="Address line 1" maxLength={256} name="address_line_1" required />
    <div className="vault-field-row">
      <VaultField autoComplete="address-level2" label="City" maxLength={120} name="city" required />
      <VaultField autoComplete="address-level1" label="State" maxLength={120} name="state" required />
    </div>
    <div className="vault-field-row">
      <VaultField autoComplete="postal-code" label="ZIP" maxLength={32} name="zip" required />
      <VaultField autoComplete="country-name" label="Country" maxLength={120} name="country" required />
    </div>
    <details className="vault-advanced">
      <summary>Advanced · Address line 2</summary>
      <VaultField autoComplete="address-line2" label="Address line 2" maxLength={256} name="address_line_2" />
    </details>
  </>;
  return <VaultField autoComplete="tel" inputMode="tel" label="Phone number" maxLength={64} name="phone_number" required />;
}

function VaultField({ inputRef, label, secure = false, ...input }: Readonly<{
  inputRef?: RefObject<HTMLInputElement | null>;
  label: string;
  secure?: boolean;
}> & InputHTMLAttributes<HTMLInputElement>) {
  const id = useId();
  return (
    <label className="vault-field" htmlFor={id}>
      <span>{secure ? <KeyRound aria-hidden="true" /> : null}{label}</span>
      <input id={id} ref={inputRef} {...input} />
    </label>
  );
}

function labelForKind(kind: VaultEntryKind): string {
  return kind === "login" ? "Login" : kind === "card" ? "Card" : kind === "address" ? "Address" : "Phone";
}

function namePlaceholder(kind: VaultEntryKind): string {
  if (kind === "card") return 'e.g. "Amex", "Chase"';
  if (kind === "address") return 'e.g. "Home", "Office"';
  if (kind === "phone") return 'e.g. "Mobile", "Work"';
  return 'e.g. "Gmail", "GitHub"';
}

async function vaultRequest(path: string, init: RequestInit = {}): Promise<Response> {
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
