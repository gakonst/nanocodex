const MANAGED_SESSION_SUBJECT_PREFIX = "managed-session-v1_";

export function managedCredentialSubject(storageId: string): string {
  if (!/^[0-9a-f]{64}$/.test(storageId)) throw new TypeError("invalid managed storage identity");
  return `${MANAGED_SESSION_SUBJECT_PREFIX}${storageId}`;
}

/** Voice callers consume the retained strategy; deployment flags cannot migrate it. */
export async function readSessionCredentialSubject(
  response: Response,
  storageId: string,
): Promise<{ subject: string; direct: boolean } | undefined> {
  if (!response.ok) {
    await response.body?.cancel();
    return undefined;
  }
  const value: unknown = await response.json();
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const retained = value as { subject?: unknown; strategy?: unknown };
  if (retained.strategy === "session_v1" && retained.subject === managedCredentialSubject(storageId)) {
    return { subject: retained.subject, direct: true };
  }
  if (retained.strategy === "directory_v1" && retained.subject === storageId) {
    return { subject: retained.subject, direct: false };
  }
  return undefined;
}

type OwnershipCoordinates = Readonly<{
  owner_id: string | null;
  session_id: string | null;
  runtime_profile: string | null;
}>;

/** The durable Session is the sole authority for the new subject version. */
export function sessionCredentialOwner(input: Readonly<{
  subject: string;
  storageId: string;
  binding: Readonly<{
    owner_id: string;
    session_id: string;
    subject: string;
    state: string;
    strategy?: string;
  }> | undefined;
  session: OwnershipCoordinates | undefined;
  initialization: (OwnershipCoordinates & { state: string }) | undefined;
  deleting: boolean;
  deleted: boolean;
  exported: boolean;
  importPending: boolean;
}>): string | undefined {
  const { binding, session, initialization } = input;
  if (input.deleting || input.deleted || input.exported || input.importPending
    || input.subject !== managedCredentialSubject(input.storageId)
    || !binding || binding.strategy !== "session_v1" || binding.state !== "active"
    || binding.subject !== input.storageId
    || !session || session.runtime_profile !== "managed"
    || !initialization || initialization.state !== "active"
    || initialization.runtime_profile !== "managed"
    || binding.owner_id !== session.owner_id || binding.session_id !== session.session_id
    || initialization.owner_id !== session.owner_id
    || initialization.session_id !== session.session_id) return undefined;
  return binding.owner_id;
}

/** Preserve the SDK's context identity and scope only its private model egress. */
export function scopedManagedModelEgress(
  binding: Fetcher,
  storageId: string,
  subject: string,
): Pick<Fetcher, "fetch"> {
  if (subject !== managedCredentialSubject(storageId)) throw new TypeError("invalid managed subject");
  return {
    fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
      const request = new Request(input, init);
      if (request.headers.get("x-nanocodex-subject") !== storageId) {
        throw new TypeError("managed model subject mismatch");
      }
      request.headers.set("x-nanocodex-subject", subject);
      return binding.fetch(request);
    },
  };
}
