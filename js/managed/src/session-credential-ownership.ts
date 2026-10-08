const MANAGED_SESSION_SUBJECT_PREFIX = "managed-session-v1_";

export function managedCredentialSubject(storageId: string): string {
  if (!/^[0-9a-f]{64}$/.test(storageId)) throw new TypeError("invalid managed storage identity");
  return `${MANAGED_SESSION_SUBJECT_PREFIX}${storageId}`;
}

/** Voice callers consume the retained strategy; deployment flags cannot migrate it. */
export async function readSessionCredentialSubject(
  response: Response,
  storageId: string,
): Promise<{ subject: string; direct: boolean; accountId?: string } | undefined> {
  if (!response.ok) {
    await response.body?.cancel();
    return undefined;
  }
  return validateSessionCredentialSubject(await response.json(), storageId);
}

export function validateSessionCredentialSubject(
  value: unknown,
  storageId: string,
): { subject: string; direct: boolean; accountId?: string } | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const retained = value as { subject?: unknown; strategy?: unknown; chatgpt_account_id?: unknown };
  const accountId = retained.chatgpt_account_id;
  if (accountId !== undefined && (typeof accountId !== "string" || !/^[\x21-\x7e]{1,256}$/.test(accountId))) return undefined;
  const selection = accountId === undefined ? {} : { accountId: accountId as string };
  if (retained.strategy === "session_v1" && retained.subject === managedCredentialSubject(storageId)) {
    return { subject: retained.subject, direct: true, ...selection };
  }
  if (retained.strategy === "directory_v1" && retained.subject === storageId) {
    return { subject: retained.subject, direct: false, ...selection };
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

export const SESSION_TOOL_OWNER_HEADER = "x-nanocodex-session-tool-owner";
const SESSION_MODEL_OWNER_HEADER = "x-nanocodex-session-model-owner";

/**
 * Route this Session's own tool egress through its private tool binding with a
 * live local owner assertion. Generic egress would otherwise resolve the
 * Session subject by calling back into this same Durable Object; that callback
 * becomes the newest incoming request, so each later subrequest of the turn
 * inherits a deeper Workers request chain until the platform depth limit fails
 * brain storage, tools, and model calls alike.
 *
 * Caller-supplied tool and model owner headers are always removed. A request
 * for this Session's subject never falls back to the callback path: unavailable
 * ownership (deletion, export, import, inactive binding) fails closed here.
 * Without the private binding (older deployments or directory subjects), traffic
 * keeps the general broker, whose Session ownership callback stays authoritative
 * and fails closed on denial.
 */
export function scopedSessionToolEgress(
  general: Fetcher,
  tool: Fetcher | undefined,
  storageId: string,
  subject: string,
  owner: () => string | undefined,
): Fetcher {
  const direct = tool !== undefined && subject === managedCredentialSubject(storageId);
  const fetch = (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const request = new Request(input, init);
    request.headers.delete(SESSION_TOOL_OWNER_HEADER);
    request.headers.delete(SESSION_MODEL_OWNER_HEADER);
    if (!direct || request.headers.get("x-nanocodex-subject") !== subject) return general.fetch(request);
    const current = owner();
    if (!current) return Promise.reject(new Error("managed tool ownership is unavailable"));
    request.headers.set(SESSION_TOOL_OWNER_HEADER, current);
    return tool.fetch(request);
  };
  return { fetch, connect: (...args: Parameters<Fetcher["connect"]>) => general.connect(...args) } as unknown as Fetcher;
}

/** Exact provider-credential tool routes the Session sends privately. */
const SESSION_MODEL_TOOL_URLS: ReadonlySet<string> = new Set([
  "https://nanocodex.internal/v1/search",
  "https://nanocodex.internal/v1/images/generations",
  "https://nanocodex.internal/v1/images/edits",
]);

/** Preserve the SDK's context identity and scope only its private model egress. */
export function scopedManagedModelEgress(
  binding: Fetcher,
  storageId: string,
  subject: string,
  sessionModel?: Readonly<{
    binding: Fetcher;
    owner(): string | undefined;
  }>,
  chatGptAccountId?: string,
): Pick<Fetcher, "fetch"> {
  if (subject !== storageId && subject !== managedCredentialSubject(storageId)) throw new TypeError("invalid managed subject");
  return {
    fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
      const request = new Request(input, init);
      if (request.headers.get("x-nanocodex-subject") !== storageId) {
        throw new TypeError("managed model subject mismatch");
      }
      request.headers.set("x-nanocodex-subject", subject);
      // The retained session configuration owns selection, never a runtime header.
      request.headers.delete("x-nanocodex-chatgpt-account-id");
      if (chatGptAccountId !== undefined) request.headers.set("x-nanocodex-chatgpt-account-id", chatGptAccountId);
      const transport = (request.url === "https://nanocodex.internal/v1/responses" || request.url === "https://nanocodex.internal/v1/messages") && (request.method === "GET" || request.method === "POST");
      // The Session's own web search and image tools use the same model
      // credential; without the private binding, egress would resolve this
      // subject by calling back into this Session (Workers depth ratchet).
      const tool = SESSION_MODEL_TOOL_URLS.has(request.url) && request.method === "POST";
      if (sessionModel && (transport || tool)) {
        // Check authoritative local state at connection time, including every
        // reconnect and tool call. Do not retain an owner across deletion or
        // durability export; never fall back to the callback path.
        const owner = sessionModel.owner();
        if (!owner) throw new Error("managed model ownership is unavailable");
        request.headers.set("x-nanocodex-session-model-owner", owner);
        return sessionModel.binding.fetch(request);
      }
      return binding.fetch(request);
    },
  };
}
