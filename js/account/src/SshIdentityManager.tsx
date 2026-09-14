import { useEffect, useId, useRef, useState, type FormEvent } from "react";

import { AccountConnectionSection } from "nanocodex-connect-ui/AccountConnectionSurface";
import { responseFailure } from "./AccountSession";
import { clientFailureMessage } from "./clientFailure";
import "./SshIdentityManager.css";
import {
  createSshIdentityPayload,
  createSshTargetPayload,
  sshIdentityPath,
  type SshIdentityMetadata,
} from "./sshIdentities";

export function SshIdentityManager({
  disabled,
  identities,
  onChanged,
  presentation,
  refreshSession,
  title = "SSH identities",
}: Readonly<{
  disabled: boolean;
  identities: readonly SshIdentityMetadata[] | null;
  onChanged(): Promise<void>;
  presentation: "profile" | "wizard";
  refreshSession(): Promise<void>;
  title?: string;
}>) {
  const id = useId().replaceAll(":", "");
  const [error, setError] = useState<string | null>(null);
  const [keyMode, setKeyMode] = useState<"generate" | "upload">("generate");
  const [operation, setOperation] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const pendingRequest = useRef<AbortController | null>(null);

  useEffect(() => () => {
    pendingRequest.current?.abort();
    pendingRequest.current = null;
  }, []);
  const unavailable = identities === null;
  useEffect(() => {
    if (!unavailable) return;
    pendingRequest.current?.abort();
    pendingRequest.current = null;
    setOperation(null);
    setKeyMode("generate");
    setError(null);
    setCopied(null);
  }, [unavailable]);

  if (identities === null) return null;

  const provision = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (disabled || pendingRequest.current) return;
    const form = event.currentTarget;
    const data = new FormData(form);
    const reference = formValue(data, "reference");
    const keyFile = data.get("private-key");
    const controller = new AbortController();
    pendingRequest.current = controller;
    const active = () => pendingRequest.current === controller && !controller.signal.aborted;
    setOperation("provision");
    setError(null);
    setCopied(null);
    let privateKey = "";
    try {
      const provisioning = {
        reference,
        hostname: formValue(data, "hostname"),
        port: Number(formValue(data, "port")),
        username: formValue(data, "username"),
        hostKeySha256: formValue(data, "host-key-sha256"),
      };
      const target = createSshTargetPayload(provisioning);
      if (keyMode === "upload") {
        if (!(keyFile instanceof File) || keyFile.size === 0) throw new Error("Choose the private-key file to host.");
        if (keyFile.size > 64 * 1024) throw new Error("Choose an unencrypted PEM private-key file no larger than 64 KiB.");
        privateKey = await keyFile.text();
        if (!active()) return;
      }
      const payload = keyMode === "generate" ? { ...target, generate: true } : createSshIdentityPayload(provisioning, privateKey);
      const response = await credentialRequest(sshIdentityPath(reference), {
        method: "PUT",
        signal: controller.signal,
        headers: { "content-type": "application/json" },
        body: JSON.stringify(payload),
      });
      if (!active()) { await response.body?.cancel(); return; }
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        if (active()) setError(keyMode === "generate"
          ? "Your account session changed. Try creating the server key again."
          : "Your account session changed. Choose the key file and try again.");
        return;
      }
      if (!response.ok) throw await sshResponseFailure(response, "Couldn’t save the SSH identity.", keyMode);
      await response.body?.cancel();
      form.reset();
      await onChanged();
    } catch (cause) {
      if (active()) setError(clientFailureMessage(cause, "Couldn’t save the SSH identity. Check every field and try again."));
    } finally {
      privateKey = "";
      const input = form.elements.namedItem("private-key");
      if (input instanceof HTMLInputElement) input.value = "";
      if (pendingRequest.current === controller) {
        pendingRequest.current = null;
        setOperation(null);
      }
    }
  };

  const remove = async (identity: SshIdentityMetadata) => {
    if (disabled || pendingRequest.current) return;
    const controller = new AbortController();
    pendingRequest.current = controller;
    const active = () => pendingRequest.current === controller && !controller.signal.aborted;
    setOperation(identity.reference);
    setError(null);
    setCopied(null);
    try {
      const response = await credentialRequest(sshIdentityPath(identity.reference), { method: "DELETE", signal: controller.signal });
      if (!active()) { await response.body?.cancel(); return; }
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        if (active()) setError("Your account session changed. Try deleting the identity again.");
        return;
      }
      if (!response.ok) throw await sshResponseFailure(response, "Couldn’t delete the SSH identity.");
      await response.body?.cancel();
      await onChanged();
    } catch (cause) {
      if (active()) setError(clientFailureMessage(cause, "Couldn’t delete the SSH identity. Try again."));
    } finally {
      if (pendingRequest.current === controller) {
        pendingRequest.current = null;
        setOperation(null);
      }
    }
  };

  const copyPublicKey = async (identity: SshIdentityMetadata) => {
    if (!identity.publicKey) return;
    setError(null);
    setCopied(null);
    try {
      await navigator.clipboard.writeText(identity.publicKey);
      setCopied(identity.reference);
    } catch { setError("Select and copy the public key above."); }
  };

  const content = (
    <div className={`ssh-identity-manager${disabled ? " is-locked" : ""}`}>
      <p className="ssh-identity-intro">
        Create a key in your vault, then add its public key to this server’s authorized_keys file. You can also upload an existing PEM key.
      </p>
      {error ? <div className="account-failure" role="alert"><p>{error}</p></div> : null}
      {copied ? <p role="status">Public key for {copied} copied.</p> : null}
      <form className="ssh-identity-form" onSubmit={(event) => void provision(event)}>
        <label htmlFor={`ssh-reference-${id}`}>
          Reference
          <input
            autoComplete="off"
            disabled={disabled || operation !== null}
            id={`ssh-reference-${id}`}
            maxLength={64}
            name="reference"
            pattern={"[A-Za-z0-9][A-Za-z0-9._\\-]{0,63}"}
            placeholder="production"
            required
          />
        </label>
        <label htmlFor={`ssh-hostname-${id}`}>
          Public lowercase hostname
          <input
            autoCapitalize="none"
            autoComplete="off"
            disabled={disabled || operation !== null}
            id={`ssh-hostname-${id}`}
            maxLength={253}
            name="hostname"
            placeholder="ssh.example.com"
            required
            spellCheck={false}
          />
        </label>
        <label htmlFor={`ssh-port-${id}`}>
          Port
          <input
            defaultValue="22"
            disabled={disabled || operation !== null}
            id={`ssh-port-${id}`}
            inputMode="numeric"
            max={65535}
            min={1}
            name="port"
            required
            type="number"
          />
        </label>
        <label htmlFor={`ssh-username-${id}`}>
          Username
          <input
            autoCapitalize="none"
            autoComplete="username"
            disabled={disabled || operation !== null}
            id={`ssh-username-${id}`}
            maxLength={128}
            name="username"
            pattern={"[A-Za-z0-9._\\-]{1,128}"}
            placeholder="deploy"
            required
            spellCheck={false}
          />
        </label>
        <label className="ssh-fingerprint-field" htmlFor={`ssh-fingerprint-${id}`}>
          Trusted host fingerprint
          <input
            autoCapitalize="none"
            autoComplete="off"
            disabled={disabled || operation !== null}
            id={`ssh-fingerprint-${id}`}
            name="host-key-sha256"
            pattern={"SHA256:[A-Za-z0-9+\\/]{43}=?"}
            placeholder="SHA256:…"
            required
            spellCheck={false}
          />
        </label>
        <label className="ssh-key-mode-field" htmlFor={`ssh-key-mode-${id}`}>
          SSH key
          <select id={`ssh-key-mode-${id}`} value={keyMode} disabled={disabled || operation !== null}
            onChange={event => { setKeyMode(event.target.value as "generate" | "upload"); setError(null); }}>
            <option value="generate">Create in vault</option>
            <option value="upload">Upload existing key</option>
          </select>
        </label>
        {keyMode === "upload" ? <label className="ssh-key-file-field" htmlFor={`ssh-private-key-${id}`}>
          PEM private-key file
          <input
            disabled={disabled || operation !== null}
            id={`ssh-private-key-${id}`}
            name="private-key"
            required
            type="file"
          />
        </label>
        : null}
        <button disabled={disabled || operation !== null} type="submit">{keyMode === "generate" ? "Create server key" : "Host identity"}</button>
      </form>
      {identities.length ? (
        <ul className="ssh-identity-list">
          {identities.map((identity) => (
            <li key={identity.reference}>
              <div>
                <strong>{identity.reference}</strong>
                <span>{identity.username}@{identity.hostname}:{identity.port}</span>
                <code>{identity.hostKeySha256}</code>
                {identity.publicKey ? <>
                  <label className="ssh-public-key-field">Public key to install
                    <textarea readOnly rows={3} spellCheck={false} value={identity.publicKey} aria-label={`Public key for ${identity.reference}`} />
                  </label>
                  <button className="ssh-copy-key" type="button" onClick={() => void copyPublicKey(identity)}>Copy public key</button>
                  <p className="ssh-public-key-help">After installing the key, ask your agent to connect the server using SSH identity “{identity.reference}”. The server needs Docker access.</p>
                </> : null}
              </div>
              <button
                aria-label={`Delete SSH identity ${identity.reference}`}
                disabled={disabled || operation !== null}
                onClick={() => void remove(identity)}
                type="button"
              >
                Delete
              </button>
            </li>
          ))}
        </ul>
      ) : <p className="api-key-empty">No hosted SSH identities.</p>}
    </div>
  );

  if (presentation === "wizard") {
    return (
      <AccountConnectionSection
        eyebrow="Infrastructure"
        meta="Private broker"
        title={title}
        titleId={`ssh-identities-${id}`}
      >
        {content}
      </AccountConnectionSection>
    );
  }

  return (
    <section className="account-ssh-identities" aria-labelledby={`ssh-identities-${id}`}>
      <div className="api-key-heading">
        <div>
          <h2 id={`ssh-identities-${id}`}>{title}</h2>
          <p>Host target-bound SSH keys for account-owned managed agents.</p>
        </div>
      </div>
      {content}
    </section>
  );
}

function formValue(data: FormData, name: string): string {
  const value = data.get(name);
  return typeof value === "string" ? value.trim() : "";
}

async function credentialRequest(path: string, init: RequestInit): Promise<Response> {
  return fetch(path, {
    ...init,
    cache: "no-store",
    credentials: "same-origin",
    redirect: "error",
    signal: init.signal ? AbortSignal.any([init.signal, AbortSignal.timeout(30_000)]) : AbortSignal.timeout(30_000),
    headers: {
      accept: "application/json",
      ...Object.fromEntries(new Headers(init.headers)),
    },
  });
}

async function sshResponseFailure(response: Response, fallback: string, keyMode?: "generate" | "upload"): Promise<Error> {
  const failure = await responseFailure(response, fallback);
  if (failure.message === "invalid ssh identity") {
    return new Error(keyMode === "generate"
      ? "The broker rejected this target. Check the public lowercase host, port, username, and SHA256 fingerprint."
      : "The broker rejected this identity. Check the public lowercase host, port, username, SHA256 fingerprint, and unencrypted PEM key file.");
  }
  if (failure.message === "ssh identity already exists") {
    return new Error("That reference already has a key. Copy its public key below, or choose a new reference.");
  }
  if (failure.message === "invalid ssh identity reference") {
    return new Error("The reference is unavailable. Choose another reference using letters, numbers, dots, underscores, or hyphens.");
  }
  if (failure.message === "credential broker unavailable") {
    return new Error("The private credential broker is unavailable. Try again.");
  }
  return failure;
}
