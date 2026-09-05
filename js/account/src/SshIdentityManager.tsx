import { useId, useState, type FormEvent } from "react";

import { AccountConnectionSection } from "nanocodex-connect-ui/AccountConnectionSurface";
import { responseFailure } from "./AccountSession";
import { clientFailureMessage } from "./clientFailure";
import {
  createSshIdentityPayload,
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
  const [operation, setOperation] = useState<string | null>(null);

  if (identities === null) return null;

  const provision = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (disabled || operation) return;
    const form = event.currentTarget;
    const data = new FormData(form);
    const reference = formValue(data, "reference");
    const keyFile = data.get("private-key");
    if (!(keyFile instanceof File) || keyFile.size === 0) {
      setError("Choose the private-key file to host.");
      return;
    }
    setOperation("provision");
    setError(null);
    let privateKey = "";
    try {
      privateKey = await keyFile.text();
      const payload = createSshIdentityPayload({
        reference,
        hostname: formValue(data, "hostname"),
        port: Number(formValue(data, "port")),
        username: formValue(data, "username"),
        hostKeySha256: formValue(data, "host-key-sha256"),
      }, privateKey);
      const response = await credentialRequest(sshIdentityPath(reference), {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(payload),
      });
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        setError("Your account session changed. Choose the key file and try again.");
        return;
      }
      if (!response.ok) throw await sshResponseFailure(response, "Couldn’t host the SSH identity.");
      await response.body?.cancel();
      form.reset();
      await onChanged();
    } catch (cause) {
      setError(clientFailureMessage(cause, "Couldn’t host the SSH identity. Check every field and try again."));
    } finally {
      privateKey = "";
      setOperation(null);
    }
  };

  const remove = async (identity: SshIdentityMetadata) => {
    if (disabled || operation) return;
    setOperation(identity.reference);
    setError(null);
    try {
      const response = await credentialRequest(sshIdentityPath(identity.reference), { method: "DELETE" });
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        setError("Your account session changed. Try deleting the identity again.");
        return;
      }
      if (!response.ok) throw await sshResponseFailure(response, "Couldn’t delete the SSH identity.");
      await response.body?.cancel();
      await onChanged();
    } catch (cause) {
      setError(clientFailureMessage(cause, "Couldn’t delete the SSH identity. Try again."));
    } finally {
      setOperation(null);
    }
  };

  const content = (
    <div className={`ssh-identity-manager${disabled ? " is-locked" : ""}`}>
      <p className="ssh-identity-intro">
        Add a target-bound PEM private key. Its contents are sent once to your private broker and are never shown here.
      </p>
      {error ? <div className="account-failure" role="alert"><p>{error}</p></div> : null}
      <form className="ssh-identity-form" onSubmit={(event) => void provision(event)}>
        <label htmlFor={`ssh-reference-${id}`}>
          Reference
          <input
            autoComplete="off"
            disabled={disabled || operation !== null}
            id={`ssh-reference-${id}`}
            maxLength={64}
            name="reference"
            pattern="[A-Za-z0-9][A-Za-z0-9._-]{0,63}"
            placeholder="production"
            required
          />
        </label>
        <label htmlFor={`ssh-hostname-${id}`}>
          Lowercase hostname
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
            pattern="[A-Za-z0-9._-]{1,128}"
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
            pattern="SHA256:[A-Za-z0-9+/]{43}=?"
            placeholder="SHA256:…"
            required
            spellCheck={false}
          />
        </label>
        <label className="ssh-key-file-field" htmlFor={`ssh-private-key-${id}`}>
          PEM private-key file
          <input
            disabled={disabled || operation !== null}
            id={`ssh-private-key-${id}`}
            name="private-key"
            required
            type="file"
          />
        </label>
        <button disabled={disabled || operation !== null} type="submit">Host identity</button>
      </form>
      {identities.length ? (
        <ul className="ssh-identity-list">
          {identities.map((identity) => (
            <li key={identity.reference}>
              <div>
                <strong>{identity.reference}</strong>
                <span>{identity.username}@{identity.hostname}:{identity.port}</span>
                <code>{identity.hostKeySha256}</code>
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
    headers: {
      accept: "application/json",
      ...Object.fromEntries(new Headers(init.headers)),
    },
  });
}

async function sshResponseFailure(response: Response, fallback: string): Promise<Error> {
  const failure = await responseFailure(response, fallback);
  if (failure.message === "invalid ssh identity") {
    return new Error("The broker rejected this identity. Check the public lowercase host, port, username, SHA256 fingerprint, and unencrypted PEM key file.");
  }
  if (failure.message === "invalid ssh identity reference") {
    return new Error("The reference is unavailable. Choose another reference using letters, numbers, dots, underscores, or hyphens.");
  }
  if (failure.message === "credential broker unavailable") {
    return new Error("The private credential broker is unavailable. Try again.");
  }
  return failure;
}
