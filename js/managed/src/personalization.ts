import { createHash } from "node:crypto";
import { MarkdownMemoryStore } from "./markdown-memory";
import { preparedMarkdownText } from "./markdown-memory-tools";

export const PERSONALIZATION_REFRESH_MS = 5 * 60_000;
export const PERSONALIZATION_LEASE_MS = 5 * 60_000;
const MAX_SUBSCRIBERS = 256;

export type PersonalizationScope = Readonly<{ organization_id: string; team_id: string; user_id: string }>;
export type PersonalizationSnapshot = PersonalizationScope & Readonly<{
  generation: number;
  version: string;
  expires_at: number;
  team_markdown?: ReturnType<MarkdownMemoryStore["bootstrap"]>;
  user_markdown?: ReturnType<MarkdownMemoryStore["bootstrap"]>;
  user_generation?: number;
  user_version?: string;
}>;
type StoredProfile = { team_id: string; generation: number; invalidated_through: number;
  built_at: number; body_json: string | null; dirty: number };
/** Shared across agents. Builds only from saved memory, never from the current prompt. */
export class PreparedPersonalizationStore {
  private readonly markdown: MarkdownMemoryStore;

  constructor(private readonly storage: DurableObjectStorage) {
    // Canonical tables must exist before their personalization triggers are installed.
    this.markdown = new MarkdownMemoryStore(storage);
    storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS prepared_personalization (
        team_id TEXT PRIMARY KEY, generation INTEGER NOT NULL DEFAULT 0,
        invalidated_through INTEGER NOT NULL DEFAULT -1, built_at INTEGER NOT NULL DEFAULT 0,
        body_json TEXT, dirty INTEGER NOT NULL DEFAULT 1, markdown_only INTEGER NOT NULL DEFAULT 1
      );
      CREATE TABLE IF NOT EXISTS personalization_subscribers (
        storage_id TEXT PRIMARY KEY, team_id TEXT NOT NULL, user_id TEXT NOT NULL,
        expires_at INTEGER NOT NULL, acknowledged_generation INTEGER NOT NULL DEFAULT -1
      );
      CREATE INDEX IF NOT EXISTS personalization_subscribers_team ON personalization_subscribers(team_id, expires_at);
      CREATE INDEX IF NOT EXISTS personalization_subscribers_expiry ON personalization_subscribers(expires_at);
      CREATE TRIGGER IF NOT EXISTS personalization_markdown_insert AFTER INSERT ON markdown_memory_documents BEGIN
        INSERT INTO prepared_personalization(team_id, generation, invalidated_through, dirty)
          VALUES (new.owner, 1, 1, 1)
        ON CONFLICT(team_id) DO UPDATE SET generation = generation + 1,
          invalidated_through = generation + 1, body_json = NULL, dirty = 1;
      END;
      CREATE TRIGGER IF NOT EXISTS personalization_markdown_replace AFTER UPDATE OF revision ON markdown_memory_documents
        WHEN new.revision <> old.revision BEGIN
        INSERT INTO prepared_personalization(team_id, generation, invalidated_through, dirty)
          VALUES (new.owner, 1, 1, 1)
        ON CONFLICT(team_id) DO UPDATE SET generation = generation + 1,
          invalidated_through = generation + 1, body_json = NULL, dirty = 1;
      END;
      CREATE TRIGGER IF NOT EXISTS personalization_markdown_delete AFTER DELETE ON markdown_memory_documents BEGIN
        INSERT INTO prepared_personalization(team_id, generation, invalidated_through, dirty)
          VALUES (old.owner, 1, 1, 1)
        ON CONFLICT(team_id) DO UPDATE SET generation = generation + 1,
          invalidated_through = generation + 1, body_json = NULL, dirty = 1;
      END;
    `);
    // Existing cached bodies may contain retired facts. Fence them once without
    // waiting for remote subscribers; the alarm retains invalidation debt.
    storage.transactionSync(() => {
      if (!storage.sql.exec<{ name: string }>("PRAGMA table_info(prepared_personalization)")
        .toArray().some(column => column.name === "markdown_only")) {
        storage.sql.exec("ALTER TABLE prepared_personalization ADD COLUMN markdown_only INTEGER NOT NULL DEFAULT 1");
        storage.sql.exec(`UPDATE prepared_personalization SET generation = generation + 1,
          invalidated_through = generation + 1, body_json = NULL, dirty = 1`);
      }
    });
  }

  /** Called by a background client request, never awaited by prompt admission. */
  snapshot(scope: PersonalizationScope, storageId: string, now = Date.now()): PersonalizationSnapshot | undefined {
    this.storage.sql.exec("DELETE FROM personalization_subscribers WHERE expires_at <= ?", now);
    const existing = this.storage.sql.exec<{ team_id: string; user_id: string }>(
      "SELECT team_id, user_id FROM personalization_subscribers WHERE storage_id = ?", storageId).toArray()[0];
    if (existing && (existing.team_id !== scope.team_id || existing.user_id !== scope.user_id)) return;
    if (!existing && this.storage.sql.exec<{ n: number }>(
      "SELECT COUNT(*) AS n FROM personalization_subscribers").one().n >= MAX_SUBSCRIBERS) return;
    this.storage.sql.exec("INSERT OR IGNORE INTO prepared_personalization(team_id) VALUES (?)", scope.team_id);
    let row = this.row(scope.team_id)!;
    // Empty caches build on this background request. Canonical edits clear the body immediately.
    if (row.body_json === null || (row.dirty && now - row.built_at >= PERSONALIZATION_REFRESH_MS)) {
      this.build(scope.team_id, now);
      row = this.row(scope.team_id)!;
    }
    const body = JSON.parse(row.body_json!) as { version: string; generation: number; valid_until: number; team_markdown?: PersonalizationSnapshot["team_markdown"] };
    if (body.valid_until <= now || body.team_markdown === undefined) { this.build(scope.team_id, now); return this.snapshot(scope, storageId, now); }
    const expiresAt = Math.min(now + PERSONALIZATION_LEASE_MS, body.valid_until);
    this.storage.sql.exec(`INSERT INTO personalization_subscribers(storage_id, team_id, user_id, expires_at, acknowledged_generation)
      VALUES (?, ?, ?, ?, ?) ON CONFLICT(storage_id) DO UPDATE SET expires_at = excluded.expires_at`,
    storageId, scope.team_id, scope.user_id, expiresAt, row.invalidated_through);
    return { ...scope, generation: body.generation, version: body.version, expires_at: expiresAt, team_markdown: body.team_markdown };
  }

  private build(teamId: string, now: number): void {
    // The daily-note window changes at UTC midnight even without a document edit.
    const validUntil = Math.min(now + PERSONALIZATION_REFRESH_MS, (Math.floor(now / 86_400_000) + 1) * 86_400_000);
    const generation = this.row(teamId)!.generation;
    // Source versions identify the exact immutable content without timestamps in
    // the model-visible text. Content updates increment memory version.
    const teamMarkdown = this.markdown.bootstrap(teamId, now);
    // Keep model-visible identity fixed-size regardless of Markdown path lengths.
    const markdownVersion = createHash("sha256")
      .update(JSON.stringify(teamMarkdown.documents.map(doc => [doc.path, doc.revision]))).digest("hex");
    const version = "markdown:" + markdownVersion;
    this.storage.sql.exec("UPDATE prepared_personalization SET body_json = ?, built_at = ?, dirty = 0 WHERE team_id = ?",
      JSON.stringify({ version, generation, valid_until: validUntil, team_markdown: teamMarkdown }), now, teamId);
  }

  invalidationPending(now = Date.now()): boolean {
    return this.storage.sql.exec(`SELECT 1 FROM personalization_subscribers s
      JOIN prepared_personalization p ON p.team_id = s.team_id
      WHERE s.expires_at > ? AND s.acknowledged_generation < p.invalidated_through LIMIT 1`, now).toArray().length > 0;
  }

  private row(teamId: string): StoredProfile | undefined {
    return this.storage.sql.exec<StoredProfile>("SELECT * FROM prepared_personalization WHERE team_id = ?", teamId).toArray()[0];
  }

  /** Background delivery fences outstanding local copies. Failure retains debt
   * for the next alarm without delaying canonical reads or writes. */
  async invalidate(notify: (storageId: string, scope: { team_id: string; user_id: string; generation: number }) => Promise<void>, now = Date.now()): Promise<void> {
    const pending = this.storage.sql.exec<{ storage_id: string; team_id: string; user_id: string; invalidated_through: number }>(`
      SELECT s.storage_id, s.team_id, s.user_id, p.invalidated_through
      FROM personalization_subscribers s JOIN prepared_personalization p ON p.team_id = s.team_id
      WHERE s.expires_at > ? AND s.acknowledged_generation < p.invalidated_through`, now).toArray();
    const results = await Promise.allSettled(pending.map(async row => {
      await notify(row.storage_id, { team_id: row.team_id, user_id: row.user_id, generation: row.invalidated_through });
      this.storage.sql.exec(`UPDATE personalization_subscribers SET acknowledged_generation = MAX(acknowledged_generation, ?)
        WHERE storage_id = ?`, row.invalidated_through, row.storage_id);
    }));
    if (results.some(r => r.status === "rejected")) throw new Error("personalization invalidation pending");
  }
}

/** Disposable local copy. Background refresh is never returned to the caller. */
export class PreparedPersonalizationCache {
  private cached?: PersonalizationSnapshot;
  private pending?: Promise<void>;
  private generationFloor = -1;
  private userGenerationFloor = -1;
  private retryAfter = 0;

  peek(scope: PersonalizationScope, now = Date.now()): PersonalizationSnapshot | undefined {
    const value = this.cached;
    return value && value.expires_at > now && value.generation >= this.generationFloor
      && (value.user_generation ?? -1) >= this.userGenerationFloor
      && sameScope(value, scope) ? value : undefined;
  }

  warm(scope: PersonalizationScope, load: () => Promise<PersonalizationSnapshot | undefined>, retain: (task: Promise<void>) => void, now = Date.now()): void {
    if (this.pending || now < this.retryAfter) return;
    const current = this.peek(scope, now);
    if (current && current.expires_at - now > 30_000) return;
    this.retryAfter = now + 5_000;
    const task = Promise.resolve().then(load).then(value => {
      if (value && sameScope(value, scope) && value.generation >= this.generationFloor && (value.user_generation ?? -1) >= this.userGenerationFloor && value.expires_at > Date.now()) this.cached = value;
    }).catch(() => {}).finally(() => { if (this.pending === task) this.pending = undefined; });
    this.pending = task;
    retain(task);
  }

  invalidate(generation: number, scope: "team" | "personal" = "team"): void {
    if (scope === "personal") this.userGenerationFloor = Math.max(this.userGenerationFloor, generation);
    else this.generationFloor = Math.max(this.generationFloor, generation);
    if (this.cached && (this.cached.generation < this.generationFloor
      || (this.cached.user_generation ?? -1) < this.userGenerationFloor)) this.cached = undefined;
    this.retryAfter = 0;
  }
}

export function sameScope(a: PersonalizationScope, b: PersonalizationScope): boolean {
  return a.organization_id === b.organization_id && a.team_id === b.team_id && a.user_id === b.user_id;
}

/** Also strips copies retained by older lifecycle receipts on replay. */
export function personalizedVoiceContext(context: Record<string, unknown>, snapshot?: PersonalizationSnapshot): Record<string, unknown> {
  const { prepared_personalization: _retained, markdown_memory: _markdown, ...current } = context;
  const markdownMemory = preparedMarkdownText(snapshot);
  return { ...current, ...(markdownMemory ? { markdown_memory: markdownMemory } : {}) };
}
