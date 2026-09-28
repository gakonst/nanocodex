import { createHash } from "node:crypto";

export type SharePermission = "read" | "write";
type Link = { id: string; permission: SharePermission; created_at: number; revoked_at: number | null };
type Comment = { id: string; input: string; created_at: number; author: "guest"; share_link_id: string };
const MAX_ACTIVE_LINKS = 20;
const MAX_COMMENTS = 10_000;
const COMMENT_PAGE_SIZE = 100;
const tokenPattern = /^nsl_[A-Za-z0-9_-]{43}$/;
const commentIdPattern = /^[A-Za-z0-9._:-]{1,128}$/;
const tokenHash = (token: string) => createHash("sha256").update(token).digest("hex");
const bearerToken = (header: string | null) => header?.startsWith("Bearer ") && tokenPattern.test(header.slice(7))
  ? header.slice(7) : undefined;

/** Each thread's Durable Object is the sole authority for its links and comments. */
export class ThreadShareLinks {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS managed_share_links (
        id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE,
        permission TEXT NOT NULL CHECK(permission IN ('read','write')),
        created_at INTEGER NOT NULL, revoked_at INTEGER
      );
      CREATE TABLE IF NOT EXISTS managed_share_comments (
        id TEXT PRIMARY KEY, link_id TEXT NOT NULL, input TEXT NOT NULL,
        created_at INTEGER NOT NULL, FOREIGN KEY(link_id) REFERENCES managed_share_links(id)
      );
    `);
  }

  create(permission: SharePermission): (Link & { token: string }) | undefined {
    if (this.storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM managed_share_links WHERE revoked_at IS NULL").one().count >= MAX_ACTIVE_LINKS) return undefined;
    const token = `nsl_${btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(32))))
      .replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")}`;
    const link: Link = { id: crypto.randomUUID(), permission, created_at: Date.now(), revoked_at: null };
    this.storage.sql.exec("INSERT INTO managed_share_links(id,token_hash,permission,created_at) VALUES(?,?,?,?)",
      link.id, tokenHash(token), permission, link.created_at);
    return { ...link, token };
  }

  list(): Omit<Link, "revoked_at">[] {
    return this.storage.sql.exec<Omit<Link, "revoked_at">>(
      "SELECT id,permission,created_at FROM managed_share_links WHERE revoked_at IS NULL ORDER BY created_at,id",
    ).toArray();
  }

  revoke(id: string): boolean {
    return this.storage.sql.exec<{ id: string }>(
      "UPDATE managed_share_links SET revoked_at=? WHERE id=? AND revoked_at IS NULL RETURNING id", Date.now(), id,
    ).toArray().length > 0;
  }

  validate(header: string | null): Link | undefined {
    const token = bearerToken(header);
    if (!token) return undefined;
    return this.storage.sql.exec<Link>(
      "SELECT id,permission,created_at,revoked_at FROM managed_share_links WHERE token_hash=? AND revoked_at IS NULL",
      tokenHash(token),
    ).toArray()[0];
  }

  comments(before?: number): { data: Comment[]; has_more: boolean; next_cursor: string | null } {
    const rows = this.storage.sql.exec<Comment & { cursor: number }>(
      `SELECT rowid AS cursor,id,link_id AS share_link_id,input,created_at,'guest' AS author FROM managed_share_comments
       WHERE (? IS NULL OR rowid < ?) ORDER BY rowid DESC LIMIT ?`,
      before ?? null, before ?? null, COMMENT_PAGE_SIZE + 1,
    ).toArray();
    const visible = rows.slice(0, COMMENT_PAGE_SIZE);
    const oldestCursor = visible.at(-1)?.cursor;
    return {
      data: visible.reverse().map(({ cursor: _cursor, ...comment }) => comment),
      has_more: rows.length > COMMENT_PAGE_SIZE,
      next_cursor: oldestCursor === undefined ? null : String(oldestCursor),
    };
  }

  /** Validation and insertion share one synchronous DO SQLite transaction: revoke wins or write wins, never both. */
  comment(header: string | null, candidate: unknown): { status: 200 | 201 | 400 | 403 | 404 | 409 | 429; value?: Comment } {
    const token = bearerToken(header);
    if (!token) return { status: 404 };
    return this.storage.transactionSync(() => {
      const link = this.validate(header);
      if (!link) return { status: 404 };
      if (link.permission !== "write") return { status: 403 };
      if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) return { status: 400 };
      const value = candidate as Record<string, unknown>;
      if (Object.keys(value).some(key => key !== "id" && key !== "input")
        || (value.id !== undefined && (typeof value.id !== "string" || !commentIdPattern.test(value.id)))
        || typeof value.input !== "string" || value.input.trim().length === 0 || value.input.length > 4000) {
        return { status: 400 };
      }
      const comment: Comment = { id: (value.id as string | undefined) ?? crypto.randomUUID(),
        input: value.input, created_at: Date.now(), author: "guest", share_link_id: link.id };
      const existing = this.storage.sql.exec<{ id: string; link_id: string; input: string; created_at: number }>(
        "SELECT id,link_id,input,created_at FROM managed_share_comments WHERE id=?", comment.id,
      ).toArray()[0];
      if (existing) return existing.link_id === link.id && existing.input === comment.input
        ? { status: 200, value: { id: existing.id, input: existing.input,
          created_at: existing.created_at, author: "guest" as const, share_link_id: existing.link_id } }
        : { status: 409 };
      if (this.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_share_comments").one().count >= MAX_COMMENTS)
        return { status: 429 as const };
      this.storage.sql.exec(
        "INSERT INTO managed_share_comments(id,link_id,input,created_at) VALUES(?,?,?,?)",
        comment.id, link.id, comment.input, comment.created_at,
      );
      return { status: 201, value: comment };
    });
  }

  clear(): void {
    this.storage.sql.exec("DELETE FROM managed_share_comments");
    this.storage.sql.exec("DELETE FROM managed_share_links");
  }
}
