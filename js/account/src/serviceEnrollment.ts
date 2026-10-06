/** Callback messages contain only a fixed metadata projection, never request bodies. */
export function enrollmentTarget(search = window.location.search): { origin: string; state: string } | undefined {
  const query = new URLSearchParams(search);
  if (!query.has("enrollment_origin") && !query.has("state")) return undefined;
  const origin = query.get("enrollment_origin") ?? "";
  const state = query.get("state") ?? "";
  try {
    const url = new URL(origin);
    if (url.origin !== origin || url.username || url.password
      || (url.protocol !== "https:" && !(url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)))
      || !/^[A-Za-z0-9_-]{16,128}$/.test(state)) return undefined;
    return { origin, state };
  } catch { return undefined; }
}

export function vaultEnrollmentKind(search = window.location.search): string | undefined {
  const query = new URLSearchParams(search);
  const service = query.get("service");
  const kind = service === "totp" ? "totp" : service === "enroll" ? query.get("kind") ?? "totp" : undefined;
  return kind && ["login", "api_key", "card", "address", "phone", "totp"].includes(kind) ? kind : undefined;
}

/** Called after an explicit Save with recipient consent. */
export function completeVaultEnrollment(entry: { id: string; kind: string; name: string }, origin?: string) {
  const target = enrollmentTarget();
  if (!target || entry.kind !== vaultEnrollmentKind()) return;
  if (entry.kind === "totp" || origin !== undefined) {
    try { const url = new URL(origin ?? ""); if (url.protocol !== "https:" || url.origin !== origin) return; }
    catch { return; }
  }
  post(target, { service: "vault", vault_id: entry.id, kind: entry.kind, name: entry.name, ...(origin === undefined ? {} : { origin }) });
}

/** Called only by the picker's explicit recipient-labelled share action. */
export function completeVaultSelection(entry: { id: string; kind: string; name: string }) {
  const target = enrollmentTarget();
  if (!target || new URLSearchParams(window.location.search).get("service") !== "select") return false;
  return post(target, { service: "vault", action: "select", vault_id: entry.id, kind: entry.kind, name: entry.name });
}

export function completePhoneApproval(operation_id: string, status: string) {
  const target = enrollmentTarget();
  if (!target) return;
  post(target, { service: "phone", operation_id, status });
}

function post(target: { origin: string; state: string }, metadata: Record<string, string>) {
  if (window.top !== window || !window.opener || window.opener.closed) return false;
  const recipient = window.opener;
  recipient.postMessage({ type: "nanocodex:service-enrollment", state: target.state, ...metadata }, target.origin);
  return true;
}
