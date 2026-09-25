/** Gmail notifications are hints; the durable history cursor is authoritative. */
export interface GmailPushEnv {
  USER_CONNECTORS: DurableObjectNamespace;
  MANAGED_AGENT_OWNERSHIP: Fetcher;
  GMAIL_PUSH_TOPIC: string;
}

type Config = { crm?: true; userId: string; connectionId: string; agentId: string; email: string };
type Event = { type: "gmail.history" | "gmail.resync"; startHistoryId: string; historyId: string; messageIds: string[]; truncated: boolean };
type Pending = { eventId: string; event: Event };
type Page = { type: Event["type"]; historyId: string; chunks: number; index: number; nextPageToken?: string; commitCursor?: string };
type Mailbox = {
  config: Config; cursor: string; target: string; renewAt: number; expiration: string;
  pageToken?: string; page?: Page; pending?: Pending; retry: number; lastError?: string;
  checkAt: number; reconcile: boolean; recentMessageIds: string[];
  renewalRetry?: number; renewalError?: string;
};
const DAY = 86_400_000;
const HOUR = 3_600_000;
const MAX_PAGES = 4;
const MAX_MESSAGES = 100;
const historyId = (value: unknown): value is string => typeof value === "string" && /^[0-9]{1,40}$/.test(value);
const newer = (a: string, b: string) => BigInt(a) > BigInt(b);
const email = (value: unknown): value is string => typeof value === "string" && value.length <= 320 && /^[^\s@]+@[^\s@]+$/.test(value);

export class GmailPushMailbox {
  private queue: Promise<unknown> = Promise.resolve();
  constructor(private readonly state: DurableObjectState, private readonly env: GmailPushEnv) {}

  // Serialize across provider awaits, where Durable Object input gates reopen.
  private serial<T>(run: () => Promise<T>): Promise<T> {
    const result = this.queue.then(run);
    this.queue = result.catch(() => {});
    return result;
  }

  fetch(request: Request): Promise<Response> {
    return this.serial(async () => {
      const path = new URL(request.url).pathname;
      let box = await this.state.storage.get<Mailbox>("mailbox");
      if (request.method === "GET" && (path === "/status" || path === "/configure")) {
        return Response.json(box ? { enabled: true, ...box.config, cursor: box.cursor, targetHistoryId: box.target,
          pending: !!box.pending || !!box.page, renewAt: box.renewAt, expiration: box.expiration, lastError: box.lastError ?? null, renewalError: box.renewalError ?? null } : { enabled: false });
      }
      if (path === "/configure" && request.method === "DELETE") {
        if (request.body !== null) {
          let expected: unknown;
          try {
            const raw = await request.text();
            if (raw.length > 4096) throw new Error();
            expected = JSON.parse(raw);
          } catch { return Response.json({ error: "invalid_json" }, { status: 400 }); }
          const agentId = expected && typeof expected === "object" && !Array.isArray(expected)
            ? (expected as Record<string, unknown>).agentId : undefined;
          if (typeof agentId !== "string" || !agentId) return Response.json({ error: "invalid_agent" }, { status: 400 });
          if (box && box.config.agentId !== agentId) return Response.json({ error: "agent_mismatch" }, { status: 409 });
        }
        // Persist local disable before calling the provider; no subsequent wake is possible.
        await this.state.storage.deleteAll();
        await this.state.storage.deleteAlarm();
        let stopped = true;
        if (box) { try { stopped = (await this.gmail(box.config, "stop", {})).ok; } catch { stopped = false; } }
        return Response.json({ enabled: false, watchStopped: stopped });
      }
      if ((request.method !== "POST" && !(request.method === "PUT" && path === "/configure")) || (path !== "/configure" && path !== "/notify")) return new Response(null, { status: 404 });
      let body: Record<string, unknown>;
      try {
        const raw = await request.text();
        if (raw.length > 4096) return new Response(null, { status: 413 });
        body = JSON.parse(raw);
        if (!body || typeof body !== "object" || Array.isArray(body)) throw new Error();
      } catch { return Response.json({ error: "invalid_json" }, { status: 400 }); }
      if (path === "/notify") {
        if (!email(body.emailAddress) || !historyId(body.historyId)) {
          return Response.json({ error: "invalid_notification" }, { status: 400 });
        }
        // Authenticated stale watch deliveries are acknowledged, never retried forever.
        if (!box || body.emailAddress.toLowerCase() !== box.config.email) return new Response(null, { status: 204 });
        if (newer(body.historyId, box.target)) {
          box.target = body.historyId;
          // Arm first: a crash between persistence operations must not strand work.
          await this.state.storage.setAlarm(Date.now() + 1000);
          await this.save(box);
        }
        return Response.json({ accepted: true }, { status: 202 });
      }
      if (![body.userId, body.connectionId, body.agentId].every(v => typeof v === "string" && v.length > 0 && v.length <= 256)
        || typeof body.agentId !== "string" || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(body.agentId)
        || (body.crm !== undefined && typeof body.crm !== "boolean")
        || !email(body.email)) return Response.json({ error: "invalid_config" }, { status: 400 });
      if (!/^projects\/[^/]+\/topics\/[^/]+$/.test(this.env.GMAIL_PUSH_TOPIC ?? "")) return Response.json({ error: "topic_unavailable" }, { status: 503 });
      const config: Config = { userId: body.userId as string, connectionId: body.connectionId as string, agentId: body.agentId as string, email: body.email.toLowerCase(), ...(body.crm === true ? { crm: true as const } : {}) };
      // Reconfiguration cannot silently discard another mailbox's pending outbox.
      if (box && JSON.stringify(box.config) !== JSON.stringify(config)) return Response.json({ error: "disable_before_reconfigure" }, { status: 409 });
      try {
        const profile = await this.profile(config);
        if (profile.emailAddress.toLowerCase() !== config.email) return Response.json({ error: "mailbox_mismatch" }, { status: 409 });
        const watch = await this.watch(config);
        box ??= { config, cursor: watch.historyId, target: watch.historyId, renewAt: 0, expiration: watch.expiration, retry: 0, checkAt: Date.now() + HOUR, reconcile: false, recentMessageIds: [] };
        box.renewAt = Math.min(Date.now() + DAY, Number(watch.expiration) - 60_000); box.expiration = watch.expiration;
        delete box.renewalRetry; delete box.renewalError;
        if (newer(watch.historyId, box.target)) box.target = watch.historyId;
        await this.schedule(box);
        await this.save(box);
        return Response.json({ enabled: true, email: config.email, cursor: box.cursor, expiration: box.expiration });
      } catch { return Response.json({ error: "gmail_configuration_failed" }, { status: 502 }); }
    });
  }

  alarm(): Promise<void> {
    return this.serial(async () => {
      const box = await this.state.storage.get<Mailbox>("mailbox");
      if (!box) return;
      await this.state.storage.setAlarm(Date.now() + 60_000);
      try {
        if (box.renewAt <= Date.now()) {
          // Watch availability must not gate the durable outbox or history reads.
          // Persist a separate retry deadline so frequent deliveries cannot hammer watch.
          try {
            const watch = await this.watch(box.config);
            box.renewAt = Math.min(Date.now() + DAY, Number(watch.expiration) - 60_000);
            box.expiration = watch.expiration;
            if (newer(watch.historyId, box.target)) box.target = watch.historyId;
            delete box.renewalRetry; delete box.renewalError;
          } catch {
            box.renewalRetry = Math.min((box.renewalRetry ?? 0) + 1, 7);
            box.renewalError = "gmail_watch_retry";
            box.renewAt = Date.now() + Math.min(HOUR, 60_000 * 2 ** (box.renewalRetry - 1));
          }
          await this.save(box);
        }
        if (box.checkAt <= Date.now()) {
          box.reconcile = true;
          box.checkAt = Date.now() + HOUR;
          await this.save(box);
        }
        let delivered = false;
        for (let pages = 0; pages < MAX_PAGES; pages++) {
          if (!box.page && !box.pending && !box.pageToken && !box.reconcile && !newer(box.target, box.cursor)) break;
          if (!box.page) {
            box.page = await this.readPage(box);
            await this.save(box);
          }
          const page = box.page;
          while (page.index < page.chunks) {
            if (!box.pending) {
              const stored = await this.state.storage.get<string[]>(`page:${page.index}`);
              if (!stored) throw new Error("missing_page_chunk");
              const messageIds = stored.filter(id => !box.recentMessageIds.includes(id));
              if (messageIds.length === 0 && page.type !== "gmail.resync") {
                const consumedIndex = page.index++;
                await this.save(box);
                await this.state.storage.delete(`page:${consumedIndex}`);
                continue;
              }
              const event: Event = { type: page.type, startHistoryId: box.cursor, historyId: page.historyId,
                messageIds, truncated: page.type === "gmail.resync" };
              box.pending = { event, eventId: await this.eventId(box.config, event) };
              await this.save(box);
            }
            if (delivered) break;
            if (!await this.deliver(box)) {
              // Trusted continuation means CRM made durable progress. Preserve
              // the exact pending event and history cursor, without failure backoff.
              box.retry = 0; delete box.lastError;
              await this.save(box);
              await this.state.storage.setAlarm(Date.now() + 1000);
              return;
            }
            delivered = true;
            box.recentMessageIds = [...new Set([...box.recentMessageIds, ...box.pending.event.messageIds])].slice(-512);
            delete box.pending;
            const consumedIndex = page.index++;
            box.retry = 0; delete box.lastError;
            await this.save(box);
            await this.state.storage.delete(`page:${consumedIndex}`);
          }
          if (page.index < page.chunks) break;
          if (page.nextPageToken) box.pageToken = page.nextPageToken;
          else {
            delete box.pageToken;
            box.reconcile = false;
            if (page.commitCursor && newer(page.commitCursor, box.cursor)) box.cursor = page.commitCursor;
          }
          delete box.page;
          await this.save(box);
          if (delivered) break; // At most one new agent turn per alarm.
        }
        await this.schedule(box);
      } catch {
        box.retry = Math.min(box.retry + 1, 10);
        box.lastError = "gmail_or_wake_retry";
        await this.save(box);
        await this.state.storage.setAlarm(Math.min(box.renewAt, Date.now() + Math.min(HOUR, 1000 * 2 ** box.retry)));
      }
    });
  }

  private schedule(box: Mailbox) {
    return this.state.storage.setAlarm(box.pending || box.page || box.pageToken || box.reconcile || newer(box.target, box.cursor)
      ? Date.now() + 1000 : Math.min(box.renewAt, box.checkAt));
  }
  private save(box: Mailbox) { return this.state.storage.put("mailbox", box); }
  private gmail(config: Config, path: string, body?: unknown): Promise<Response> {
    return this.env.USER_CONNECTORS.get(this.env.USER_CONNECTORS.idFromName(config.userId)).fetch(new Request(
      `https://gmail.googleapis.com/gmail/v1/users/me/${path}`, {
        method: body === undefined ? "GET" : "POST", signal: AbortSignal.timeout(20_000),
        headers: { "x-nanocodex-connector-connection": config.connectionId, "content-type": "application/json" },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      }));
  }
  private async json(response: Response): Promise<Record<string, unknown>> {
    if (!response.ok) throw new Error("gmail_failed");
    // Gmail history is requested in bounded pages; reject malformed or enormous responses.
    const reader = response.body?.getReader();
    if (!reader) throw new Error("gmail_empty");
    let length = 0; const chunks: Uint8Array[] = [];
    for (;;) {
      const { done, value } = await reader.read(); if (done) break;
      length += value.byteLength;
      if (length > 1_048_576) { await reader.cancel(); throw new Error("gmail_response_too_large"); }
      chunks.push(value);
    }
    const bytes = new Uint8Array(length); let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    const result = JSON.parse(new TextDecoder().decode(bytes));
    if (!result || typeof result !== "object" || Array.isArray(result)) throw new Error("gmail_invalid");
    return result;
  }
  private async profile(config: Config): Promise<{ emailAddress: string; historyId: string }> {
    const data = await this.json(await this.gmail(config, "profile"));
    if (!email(data.emailAddress) || !historyId(data.historyId)) throw new Error("gmail_invalid_profile");
    return { emailAddress: data.emailAddress, historyId: data.historyId };
  }
  private async watch(config: Config): Promise<{ historyId: string; expiration: string }> {
    const data = await this.json(await this.gmail(config, "watch", { topicName: this.env.GMAIL_PUSH_TOPIC, labelIds: ["INBOX"], labelFilterBehavior: "include" }));
    if (!historyId(data.historyId) || typeof data.expiration !== "string" || !Number.isFinite(Number(data.expiration)) || Number(data.expiration) <= Date.now()) throw new Error("gmail_invalid_watch");
    return { historyId: data.historyId, expiration: data.expiration };
  }
  private async readPage(box: Mailbox): Promise<Page> {
    const query = new URLSearchParams({ startHistoryId: box.cursor, maxResults: "100", historyTypes: "messageAdded", labelId: "INBOX" });
    if (box.pageToken) query.set("pageToken", box.pageToken);
    const response = await this.gmail(box.config, `history?${query}`);
    let type: Event["type"] = "gmail.history", current: string;
    let nextPageToken: string | undefined;
    const ids = new Set<string>();
    if (response.status === 404) {
      const profile = await this.profile(box.config);
      if (profile.emailAddress.toLowerCase() !== box.config.email) throw new Error("mailbox_changed");
      type = "gmail.resync";
      current = profile.historyId;
    } else {
      const data = await this.json(response);
      if (!historyId(data.historyId) || (data.history !== undefined && !Array.isArray(data.history))) throw new Error("gmail_invalid_history");
      if (data.nextPageToken !== undefined && (typeof data.nextPageToken !== "string" || !data.nextPageToken || data.nextPageToken.length > 4096 || data.nextPageToken === box.pageToken)) throw new Error("gmail_invalid_page");
      current = data.historyId;
      nextPageToken = data.nextPageToken as string | undefined;
      for (const row of (data.history ?? []) as Record<string, unknown>[]) {
        if (!row || typeof row !== "object") throw new Error("gmail_invalid_history");
        if (!Array.isArray(row.messagesAdded)) continue;
        for (const entry of row.messagesAdded) {
          const message = entry?.message;
          if (!message) continue;
          // Gmail history normally includes only id/threadId. The server-side
          // labelId filter is authoritative when labelIds are absent.
          if (message.labelIds !== undefined && (!Array.isArray(message.labelIds) || !message.labelIds.includes("INBOX")
            || message.labelIds.includes("DRAFT"))) continue;
          if (typeof message.id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(message.id)) throw new Error("gmail_invalid_message_id");
          if (!box.recentMessageIds.includes(message.id)) ids.add(message.id);
        }
      }
    }
    // Stage every ID in bounded values before exposing the page. Cursor advancement
    // waits for all chunks, so a large history record never silently loses mail.
    const messageIds = [...ids];
    const chunks = type === "gmail.resync" ? 1 : Math.ceil(messageIds.length / MAX_MESSAGES);
    for (let index = 0; index < chunks; index++) {
      await this.state.storage.put(`page:${index}`, messageIds.slice(index * MAX_MESSAGES, (index + 1) * MAX_MESSAGES));
    }
    return { type, historyId: current, chunks, index: 0,
      ...(nextPageToken ? { nextPageToken } : { commitCursor: current }) };
  }
  private async eventId(config: Config, event: Event): Promise<string> {
    const bytes = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify([config, event])));
    return `gmail-${Array.from(new Uint8Array(bytes), byte => byte.toString(16).padStart(2, "0")).join("")}`;
  }
  private async deliver(box: Mailbox): Promise<boolean> {
    const pending = box.pending!;
    const response = await this.env.MANAGED_AGENT_OWNERSHIP.fetch(new Request("https://managed-ownership.internal/v1/gmail-push/wake", {
      method: "POST", signal: AbortSignal.timeout(20_000), headers: { "content-type": "application/json" },
      body: JSON.stringify({ userId: box.config.userId, agentId: box.config.agentId, eventId: pending.eventId,
        input: JSON.stringify({ connectionId: box.config.connectionId, email: box.config.email, ...(box.config.crm === true ? { crm: true } : {}), ...pending.event }) }),
    }));
    if (!response.ok) throw new Error("wake_retry");
    const result = await response.json() as { status?: string; progress?: unknown };
    if (box.config.crm === true && result.status === "busy" && result.progress === true) return false;
    if (result.status !== "accepted" && result.status !== "duplicate") throw new Error("wake_retry");
    return true;
  }
}
