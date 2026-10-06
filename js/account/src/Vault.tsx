import { completeVaultEnrollment, completeVaultSelection, enrollmentTarget, vaultEnrollmentKind } from "./serviceEnrollment";
import "./DeviceConnect.css";
import "./Vault.css";
import "./VaultWorkspace.css";
import { useLocation, useSearchParams } from "react-router";
import { DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem } from "./DropdownMenu";
import { useAccountQuery } from "./useAccountQuery";
import { ArrowLeft, ChevronDown, CreditCard, KeyRound, LockKeyhole, MapPin, Phone, Plus, Search, Server, Trash2, X } from "lucide-react";
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
  { kind: "totp", title: "Authenticator accounts", addLabel: "Add authenticator" },
  { kind: "login", title: "Logins", addLabel: "Add login" },
  { kind: "api_key", title: "API keys", addLabel: "Add API key" },
  { kind: "card", title: "Cards", addLabel: "Add card" },
  { kind: "address", title: "Addresses", addLabel: "Add address" },
  { kind: "phone", title: "Phones", addLabel: "Add phone" },
];

const filters = [
  ["all", "All"], ["totp", "Authenticators"], ["login", "Logins"], ["api_key", "API keys"],
  ["card", "Cards"], ["address", "Addresses"], ["phone", "Phones"], ["ssh", "SSH"],
] as const;
type VaultFilter = typeof filters[number][0];
const kindIcons = { totp: KeyRound, login: LockKeyhole, api_key: KeyRound, card: CreditCard, address: MapPin, phone: Phone };

export function Vault() {
  const session = useAccountSession();
  if (window.top !== window) return <p>Open Vault in a secure window.</p>;
  const accountId = session.account?.persistent ? session.account.id : undefined;
  if (session.status === "checking") return <div className="vault-workspace">
    <header className="vault-workspace-heading"><h1 tabIndex={-1}>Vault</h1></header>
    <p className="vault-workspace-status" role="status">Loading vault…</p>
  </div>;
  if (!accountId) return (
    <div className="vault-workspace">
      <header className="vault-workspace-heading"><h1 tabIndex={-1}>Vault</h1></header>
      <AccountChooser
        description={session.reauthenticationRequired
          ? "Your session expired. Sign in to open your vault."
          : "Sign in to save your credentials and personal details."}
        disabled={session.operation !== null}
        failure={session.error}
        onChooseAccount={(selection) => void session.chooseAccount(selection)}
      />
    </div>
  );
  return <VaultWorkspace key={accountId} accountId={accountId} />;
}

function VaultWorkspace({ accountId }: { accountId: string }) {
  const session = useAccountSession();
  const [searchParams, setSearchParams] = useSearchParams();
  const location = useLocation();
  const service = searchParams.get("service");
  const enrollmentKind = vaultEnrollmentKind();
  const selecting = service === "select";
  const recipient = enrollmentTarget();
  const [selectedId, setSelectedId] = useState("");
  const [savedNotice, setSavedNotice] = useState<string>();
  const [uncertain, setUncertain] = useState(false);
  const requestedKind = searchParams.get("add");
  const adding = sections.find((section) => section.kind === requestedKind)?.kind ?? null;
  const [filter, setFilter] = useState<VaultFilter>("all");
  const [search, setSearch] = useState("");
  const [operationFailure, setFailure] = useState<string | null>(null);
  const [operation, setOperation] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const addButtonRef = useRef<HTMLButtonElement | null>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const pending = useRef<AbortController | null>(null);
  const wasAdding = useRef(false);
  const { query, refresh } = useAccountQuery(accountId, "/v1/credentials", decodeVaultStatus);
  const status = query.data ?? null;
  const failure = operationFailure ?? (query.error ? clientFailureMessage(query.error, "Couldn’t load your vault.") : null);
  const load = useCallback(async () => { await refresh(); }, [refresh]);

  // Leaving a form or account discards its inputs and invalidates pending callbacks.
  useEffect(() => {
    setFailure(null);
    setOperation(null);
    return () => {
      pending.current?.abort();
      pending.current = null;
    };
  }, [location.key]);
  useEffect(() => {
    if (wasAdding.current && !adding) addButtonRef.current?.focus();
    wasAdding.current = Boolean(adding);
  }, [adding]);
  const closeForm = useCallback(() => {
    setSearchParams((current) => {
      const next = new URLSearchParams(current);
      next.delete("add");
      return next;
    }, { replace: true });
  }, [setSearchParams]);
  const openForm = (kind: VaultEntryKind) => {
    returnFocusRef.current = addButtonRef.current;
    setSearchParams((current) => {
      const next = new URLSearchParams(current);
      next.set("add", kind);
      return next;
    });
  };

  const mutate = async (path: string, init: RequestInit, label: string, saved = false) => {
    if (pending.current || uncertain) return;
    const controller = new AbortController();
    pending.current = controller;
    const active = () => pending.current === controller && !controller.signal.aborted;
    setOperation(label);
    setFailure(null);
    try {
      const response = await vaultRequest(path, { ...init, signal: controller.signal });
      if (!active()) { await response.body?.cancel(); return; }
      if (response.status === 401) {
        await response.body?.cancel();
        await session.refresh();
        if (active()) setFailure("Your session changed. Sign in and try again.");
        return;
      }
      if (!response.ok) {
        if ([400,403,422].includes(response.status)) { await response.body?.cancel(); setFailure("Couldn’t save the item. Check the fields and your access."); return; }
        throw await responseFailure(response, "Couldn’t save the change.");
      }
      if (saved) {
        const wire: unknown = await response.json();
        if (!active()) return;
        const entry = decodeVaultEntries([wire])[0];
        if (!entry) throw new Error("Invalid Vault receipt");
        if (enrollmentKind === entry.kind) completeVaultEnrollment(entry, isRecord(wire) && typeof wire.origin === "string" ? wire.origin : isRecord(wire) && typeof wire.browser_origin === "string" ? wire.browser_origin : undefined);
        setSavedNotice(entry.kind === "totp" ? "Authenticator saved to Vault." : "Item saved to Vault.");
      } else await response.body?.cancel();
      if (!active()) return;
      // Clear secret inputs immediately after the confirmed save.
      if (saved) closeForm();
      setDeleting(null);
      await load();
    } catch (cause) {
      if (active()) {
        if (saved) { setUncertain(true); closeForm(); }
        setFailure("The save could not be confirmed. Check your Vault before adding this item again.");
      }
    } finally {
      if (active()) {
        pending.current = null;
        setOperation(null);
      }
    }
  };
  const save = (kind: VaultEntryKind, values: Record<string, string>) => mutate(vaultEntryPath(kind), {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(values),
  }, "save", true);
  const entries = (status?.entries ?? []).filter((entry) =>
    (filter === "all" || filter === entry.kind)
    && (entry.name + " " + labelForKind(entry.kind)).toLowerCase().includes(search.toLowerCase().trim()));
  const ssh = (filter === "all" || filter === "ssh" ? status?.ssh ?? [] : [])
    .filter((entry) => (entry.reference + " " + entry.username + " " + entry.hostname).toLowerCase().includes(search.toLowerCase().trim()));
  const count = entries.length + ssh.length;

  if (selecting) return <div className="vault-page"><div className="vault-content vault-picker">
    <header className="vault-heading"><div><h1>Choose a Vault item</h1></div></header>
    {recipient ? <>
      <p>Share the selected item’s name, kind, and Vault ID with <strong>{recipient.origin}</strong>.</p>
      <p>This selection does not grant access to use the item. You review that access separately in Connect.</p>
      {failure ? <p role="alert">{failure}</p> : null}
      {!status ? <p role="status">Loading your Vault…</p> : status.entries.length ? <>
        <ul className="vault-picker-list">{status.entries.map(entry => <li key={entry.id}>
          <label><input type="radio" name="vault-selection" value={entry.id} checked={selectedId === entry.id} onChange={() => {setSelectedId(entry.id); setSavedNotice(undefined);}} /><span><strong>{entry.name}</strong><small>{labelForKind(entry.kind)}</small></span></label>
        </li>)}</ul>
        <button className="vault-save" type="button" disabled={!status.entries.some(entry => entry.id === selectedId)} onClick={() => {
          const entry = status.entries.find(item => item.id === selectedId);
          if (entry && completeVaultSelection(entry)) setSavedNotice(`Shared ${entry.name} with ${recipient.origin}.`);
          else setFailure("The requesting window is unavailable. Reopen the picker from your app.");
        }}>Share selected item with {recipient.origin}</button>
      </> : <p>No saved Vault items. Add an item in your Vault, then reopen this picker.</p>}
      {savedNotice ? <p role="status">{savedNotice}</p> : null}
    </> : <p role="alert">This picker needs a valid requesting origin and state. Reopen it from your app.</p>}
  </div></div>;

  return (
    <div className="vault-workspace">
      <header className="vault-workspace-heading">
        <h1 tabIndex={-1}>Vault</h1>
        {!adding ? <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button className="vault-primary-action" ref={addButtonRef} disabled={operation !== null || uncertain} type="button">
              <Plus aria-hidden="true" /> Add item <ChevronDown aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="vault-add-menu" onCloseAutoFocus={(event) => {
            if (document.querySelector(".vault-inline-entry")) event.preventDefault();
          }}>
            {sections.map(({ kind, addLabel }) => {
              const Icon = kindIcons[kind];
              return <DropdownMenuItem key={kind} onSelect={() => openForm(kind)} className="vault-add-menu-item">
                <Icon aria-hidden="true" />{addLabel}
              </DropdownMenuItem>;
            })}
            <DropdownMenuItem className="vault-add-menu-item" onSelect={() => { setFilter("ssh"); setSearch(""); }}>
              <Server aria-hidden="true" />Manage SSH keys
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu> : null}
      </header>
      {savedNotice ? <p role="status">{savedNotice}</p> : null}
      {adding ? <>
        <button className="vault-back" disabled={operation !== null} onClick={closeForm} type="button"><ArrowLeft aria-hidden="true" /> All items</button>
        <VaultEntryDialog key={location.key + adding} busy={operation !== null} kind={adding}
          onClose={closeForm} onSave={save} returnFocusRef={returnFocusRef}
          completionOrigin={enrollmentKind === adding ? recipient?.origin : undefined} error={session.error ?? operationFailure} presentation="page" description="" />
      </> : <>
        <div className="vault-workspace-tools">
          <label className="vault-search"><Search aria-hidden="true" /><input aria-label="Search vault" placeholder="Search vault" value={search} onChange={(event) => setSearch(event.target.value)} type="search" /></label>
          <div className="vault-filters" aria-label="Filter vault">
            {filters.map(([value, title]) => <button key={value} type="button" aria-pressed={filter === value} onClick={() => { setFilter(value); setDeleting(null); }}>{title}</button>)}
          </div>
        </div>
        {session.error || failure ? <div className="vault-workspace-error" role="alert">
          <p>{session.error ?? failure}</p>
          <button type="button" disabled={operation !== null || query.isFetching} onClick={() => { setFailure(null); void load(); }}>Retry</button>
        </div> : null}
        {!status && query.isPending ? <p className="vault-workspace-status" role="status">Loading vault…</p> : status ? <>
          <p className="vault-item-count" role="status">{count} {count === 1 ? "item" : "items"}</p>
          {count ? filter !== "ssh" ? <ul className="vault-items">
            {entries.map((entry) => {
              const Icon = kindIcons[entry.kind];
              return <li key={entry.id}>
                <span className="vault-kind-icon"><Icon aria-hidden="true" /></span>
                <div className="vault-item-copy"><strong>{entry.name}</strong><span>{labelForKind(entry.kind)}</span></div>
                {deleting === entry.id ? <div className="vault-delete-confirm">
                  <span>Delete item?</span>
                  <button type="button" disabled={operation !== null} onClick={() => void mutate(vaultEntryPath(entry.kind, entry.id), { method: "DELETE" }, entry.id)}>Delete</button>
                  <button type="button" disabled={operation !== null} onClick={() => setDeleting(null)}>Cancel</button>
                </div> : <button className="vault-row-action" type="button" aria-label={`Delete ${entry.name}`} disabled={operation !== null} onClick={() => setDeleting(entry.id)}><Trash2 aria-hidden="true" /></button>}
              </li>;
            })}
            {ssh.map((entry) => <li key={entry.reference}>
              <span className="vault-kind-icon"><Server aria-hidden="true" /></span>
              <div className="vault-item-copy"><strong>{entry.reference}</strong><span>{entry.username}@{entry.hostname}</span></div>
              <button className="vault-row-action" type="button" onClick={() => { setFilter("ssh"); setSearch(""); }}>Manage</button>
            </li>)}
          </ul> : null : filter !== "ssh" ? <div className="vault-empty"><LockKeyhole aria-hidden="true" /><h2>{search ? "No matching items" : filter === "all" ? "Your vault is empty" : "No items yet"}</h2><p>{search ? "Try another name or filter." : "Add credentials and details for your tasks."}</p></div> : null}
        </> : null}
        {filter === "ssh" ? <div className="vault-ssh-manager">
          <SshIdentityManager key={accountId} disabled={operation !== null} identities={status ? ssh : null}
            emptyMessage={search ? "No matching SSH keys." : "No SSH keys yet."}
            onChanged={load} presentation="workspace" refreshSession={session.refresh} title="SSH keys" />
        </div> : null}
      </>}
    </div>
  );
}

export function VaultEntryDialog({
  busy,
  kind,
  onClose,
  onSave,
  returnFocusRef,
  name = "",
  origin,
  title,
  description = "Values are encrypted in your vault.",
  error,
  children,
  completionOrigin,
  presentation = "dialog",
}: Readonly<{
  busy: boolean;
  kind: VaultEntryKind;
  onClose(): void;
  onSave(kind: VaultEntryKind, values: Record<string, string>): Promise<void>;
  returnFocusRef: RefObject<HTMLElement | null>;
  name?: string;
  origin?: string;
  title?: string;
  description?: string;
  error?: string | null;
  children?: ReactNode;
  completionOrigin?: string;
  presentation?: "dialog" | "page";
}>) {
  const titleId = useId();
  const [shareCompletion, setShareCompletion] = useState(false);
  const [validationError, setValidationError] = useState<string>();
  const backdropRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLElement>(null);
  const firstInputRef = useRef<HTMLInputElement>(null);
  const formRef = useRef<HTMLFormElement>(null);
  useEffect(() => {
    const form = formRef.current;
    return () => form?.reset();
  }, []);
  const dismiss = useCallback(() => {
    if (!busy) onClose();
  }, [busy, onClose]);
  useModalBoundary({
    backdropRef,
    initialFocusRef: firstInputRef,
    onDismiss: dismiss,
    open: presentation === "dialog",
    panelRef: dialogRef,
    returnFocusRef,
  });

  useEffect(() => {
    if (presentation !== "page") return;
    firstInputRef.current?.focus();
  }, [presentation]);

  const submitting = useRef(false);
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (completionOrigin && !shareCompletion) return;
    if (busy || submitting.current) return;
    const data = new FormData(event.currentTarget);
    const values = Object.fromEntries(
      [...data.entries()].flatMap(([key, value]) => typeof value === "string" && value.trim()
        ? [[key, key === "password" || key === "api_key" ? value : value.trim()]]
        : []),
    );
    if (kind === "totp") {
      try {
        const url = new URL(values.origin ?? "");
        if (url.protocol !== "https:" || url.origin !== values.origin || url.username || url.password) throw new Error();
      } catch { setValidationError("Enter an exact HTTPS origin, such as https://example.com, without a path or trailing slash."); return; }
    }
    setValidationError(undefined);
    // Clear private inputs before handing off the ephemeral same-origin request.
    for (const input of event.currentTarget.querySelectorAll<HTMLInputElement>('input[type="password"]')) input.value = "";
    submitting.current = true;
    void onSave(kind, values).finally(() => {
      submitting.current = false;
      for (const key of Object.keys(values)) delete values[key];
    });
  };

  return (
    <div className={presentation === "page" ? "vault-inline-entry" : "vault-dialog-backdrop"} ref={backdropRef} onMouseDown={(event) => {
      if (presentation === "dialog" && event.target === event.currentTarget && !busy) onClose();
    }}>
      <section aria-labelledby={titleId} aria-modal={presentation === "dialog" ? true : undefined} className="vault-dialog" ref={dialogRef} role={presentation === "dialog" ? "dialog" : undefined}>
        <header>
          <div>
            <h2 id={titleId}>{title ?? `Add ${kind === "api_key" ? "API key" : labelForKind(kind).toLowerCase()}`}</h2>
            {description ? <p>{description}</p> : null}
          </div>
          {presentation === "dialog" ? <button aria-label="Close" disabled={busy} onClick={onClose} type="button"><X aria-hidden="true" /></button> : null}
        </header>
        {children}
        {error || validationError ? <p role="alert">{error ?? validationError}</p> : null}
        <form autoComplete="off" onSubmit={submit} ref={formRef}>
          <fieldset className="vault-dialog-fields" disabled={busy}>
            <VaultField autoComplete="off" defaultValue={name} inputRef={firstInputRef} label="Name" maxLength={120} name="name" placeholder={namePlaceholder(kind)} required />
            {kind === "login" ? <VaultField autoComplete="off" defaultValue={origin} label="Website (optional)" maxLength={2048} name="browser_origin" placeholder="https://example.com" type="url" /> : null}
            {kind === "totp" ? <VaultField autoComplete="off" defaultValue={origin} label="Website origin" maxLength={2048} name="origin" placeholder="https://example.com" type="url" required /> : null}
            {fieldsForKind(kind)}
          </fieldset>
          {completionOrigin ? <label className="vault-share-consent"><input type="checkbox" checked={shareCompletion} onChange={event => setShareCompletion(event.target.checked)} />Share completion with {completionOrigin}. This includes the saved name, kind, and Vault ID.</label> : null}
          <footer>
            <button disabled={busy} onClick={onClose} type="button">Cancel</button>
            <button className="vault-save" disabled={busy || Boolean(completionOrigin && !shareCompletion)} type="submit">{busy ? "Saving…" : "Save"}</button>
          </footer>
        </form>
      </section>
    </div>
  );
}

function fieldsForKind(kind: VaultEntryKind): ReactNode {
  if (kind === "totp") return <TotpFields />;
  if (kind === "api_key") return <VaultField autoCapitalize="none" autoComplete="off" label="API key" maxLength={8192} name="api_key" required spellCheck={false} type="password" secure />;
  // This edits a third-party credential, rather than signing in to this site.
  // Explicitly disable autocomplete on both fields: Chromium's address-on-typing
  // suggestions can otherwise mix with login suggestions and crash Brave 1.95.
  if (kind === "login") return <>
    <VaultField autoCapitalize="none" autoComplete="off" label="Username" maxLength={512} name="username" required spellCheck={false} />
    <VaultField autoComplete="off" label="Password" maxLength={8192} name="password" required type="password" secure />
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

function TotpFields() {
  const [mode, setMode] = useState("uri");
  return <>
    <label className="vault-field"><span>Enrollment format</span><select value={mode} onChange={event => setMode(event.target.value)}><option value="uri">Authenticator URI</option><option value="seed">Setup key</option></select></label>
    {mode === "uri" ? <VaultField key="uri" label="Authenticator URI" name="otpauth_uri" type="password" autoComplete="off" autoCapitalize="none" spellCheck={false} maxLength={4096} required secure /> : <>
      <VaultField key="seed" label="Setup key" name="seed" type="password" autoComplete="off" autoCapitalize="none" spellCheck={false} maxLength={208} required secure />
      <VaultField label="Issuer" name="issuer" maxLength={256} required />
      <VaultField label="Account label" name="account" maxLength={256} required />
    </>}
    <p>Paste your otpauth URI or setup key here. It goes directly to your encrypted Vault. Only account metadata is returned.</p>
  </>;
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
  return kind === "totp" ? "Authenticator" : kind === "api_key" ? "API key" : kind === "login" ? "Login" : kind === "card" ? "Card" : kind === "address" ? "Address" : "Phone";
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
    redirect: "error",
    referrerPolicy: "no-referrer",
    credentials: "same-origin",
    signal: init.signal ? AbortSignal.any([init.signal, AbortSignal.timeout(30_000)]) : AbortSignal.timeout(30_000),
    headers: {
      accept: "application/json",
      ...Object.fromEntries(new Headers(init.headers)),
    },
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function decodeVaultStatus(value: unknown): VaultStatus {
  if (!isRecord(value)) throw new Error("Invalid vault response.");
  return { ssh: decodeSshIdentities(value.ssh), entries: decodeVaultEntries(value.vault) };
}
