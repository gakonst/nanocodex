export type MainThreadCompletion = { sequence: number; turn_id: string };
export type MainThreadWatch = {
  agent_id: string;
  cursor: number;
  authorization_json: string;
  authorization_epoch: number;
  state: 'watching' | 'idle' | 'retired';
  generation: number;
  poll_delay: number;
  retry_at: number;
};

/** Coordinator completion ledger and main-thread-owned durable subscriptions. */
export class MainThreadCompletions {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS main_thread_completions (
      sequence INTEGER PRIMARY KEY AUTOINCREMENT, turn_id TEXT NOT NULL UNIQUE);
      CREATE TABLE IF NOT EXISTS main_thread_completion_watches (
        agent_id TEXT PRIMARY KEY, cursor INTEGER NOT NULL,
        authorization_json TEXT NOT NULL, authorization_epoch INTEGER NOT NULL,
        state TEXT NOT NULL, retry_at INTEGER NOT NULL);
      CREATE INDEX IF NOT EXISTS main_thread_completion_watches_pending
        ON main_thread_completion_watches(state,retry_at);`);
    const columns = new Set(storage.sql.exec<{ name: string }>("PRAGMA table_info(main_thread_completion_watches)").toArray().map(row => row.name));
    if (!columns.has("generation")) storage.sql.exec("ALTER TABLE main_thread_completion_watches ADD COLUMN generation INTEGER NOT NULL DEFAULT 0");
    if (!columns.has("poll_delay")) storage.sql.exec("ALTER TABLE main_thread_completion_watches ADD COLUMN poll_delay INTEGER NOT NULL DEFAULT 30000");
  }

  /** Call only when an internal coordinator turn's completion is committed. */
  publish(turnId: string): number {
    this.storage.sql.exec('INSERT INTO main_thread_completions (turn_id) VALUES (?) ON CONFLICT(turn_id) DO NOTHING', turnId);
    return this.storage.sql.exec<MainThreadCompletion>(
      'SELECT sequence,turn_id FROM main_thread_completions WHERE turn_id=?', turnId).one().sequence;
  }

  entries(after: number): MainThreadCompletion[] {
    return this.storage.sql.exec<MainThreadCompletion>(
      'SELECT sequence,turn_id FROM main_thread_completions WHERE sequence>? ORDER BY sequence LIMIT 32', after).toArray();
  }

  latestSequence(): number {
    return this.storage.sql.exec<{ sequence: number }>(
      'SELECT COALESCE(MAX(sequence),0) AS sequence FROM main_thread_completions').one().sequence;
  }

  /** Retries cannot replace the initial authority, rewind progress, or revive revocation. */
  watch(agentId: string, initialCursor: number, authorizationJson: string, authorizationEpoch: number): MainThreadWatch {
    if (!this.get(agentId)) this.assertCapacity();
    this.storage.sql.exec(`INSERT INTO main_thread_completion_watches
      (agent_id,cursor,authorization_json,authorization_epoch,state,retry_at)
      VALUES (?,?,?,?,'watching',?) ON CONFLICT(agent_id) DO NOTHING`,
    agentId, initialCursor, authorizationJson, authorizationEpoch, Date.now());
    return this.get(agentId)!;
  }

  /** Only a fresh explicitly authorized route may replace a retired subscription. */
  reauthorize(agentId: string, initialCursor: number, authorizationJson: string, authorizationEpoch: number): MainThreadWatch {
    const old = this.get(agentId);
    if (old?.state === 'retired' && old.authorization_epoch < authorizationEpoch) this.assertCapacity();
    this.storage.sql.exec(`UPDATE main_thread_completion_watches
      SET cursor=?,authorization_json=?,authorization_epoch=?,state='watching',retry_at=?,generation=generation+1,poll_delay=30000
      WHERE agent_id=? AND state='retired' AND authorization_epoch<?`,
    initialCursor, authorizationJson, authorizationEpoch, Date.now(), agentId, authorizationEpoch);
    return this.watch(agentId, initialCursor, authorizationJson, authorizationEpoch);
  }

  private assertCapacity(): void {
    const count = this.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM main_thread_completion_watches WHERE state='watching'").one().count;
    if (count >= 128) throw new Error("project has 128 active completion subscriptions; wait for work to finish");
  }

  /** An explicit retained admission can resume quiescent work, never a revoked watch. */
  activate(agentId: string, authorizationJson: string, authorizationEpoch: number): void {
    const old = this.get(agentId);
    if (!old || old.state === 'retired' || old.authorization_epoch !== authorizationEpoch) return;
    if (old.state === 'idle') this.assertCapacity();
    this.storage.sql.exec(`UPDATE main_thread_completion_watches
      SET state='watching',authorization_json=?,retry_at=?,poll_delay=30000,generation=generation+1 WHERE agent_id=?`,
    authorizationJson, Date.now(), agentId);
  }

  /** A successful caught-up read may sleep only when the entire child subtree is quiescent. */
  settle(watch: MainThreadWatch, busy: boolean, latest: number, progressed: boolean): void {
    if (!busy) {
      this.storage.sql.exec(`UPDATE main_thread_completion_watches SET state='idle',authorization_json=''
        WHERE agent_id=? AND state='watching' AND generation=? AND cursor>=?`, watch.agent_id, watch.generation, latest);
    } else {
      const delay = progressed ? 30_000 : Math.min(300_000, watch.poll_delay * 2);
      this.storage.sql.exec(`UPDATE main_thread_completion_watches SET poll_delay=?,retry_at=?
        WHERE agent_id=? AND state='watching' AND generation=?`, delay, Date.now() + delay, watch.agent_id, watch.generation);
    }
  }

  get(agentId: string): MainThreadWatch | undefined {
    return this.storage.sql.exec<MainThreadWatch>(
      'SELECT * FROM main_thread_completion_watches WHERE agent_id=?', agentId).toArray()[0];
  }

  due(now: number): MainThreadWatch[] {
    return this.storage.sql.exec<MainThreadWatch>(`SELECT * FROM main_thread_completion_watches
      WHERE state='watching' AND retry_at<=? ORDER BY retry_at,agent_id LIMIT 8`, now).toArray();
  }

  nextAlarm(): number | undefined {
    return this.storage.sql.exec<{ deadline: number | null }>(
      "SELECT MIN(retry_at) AS deadline FROM main_thread_completion_watches WHERE state='watching'").one().deadline ?? undefined;
  }

  /** Advance only after the corresponding notification is durably admitted. */
  advance(agentId: string, cursor: number): void {
    this.storage.sql.exec(`UPDATE main_thread_completion_watches SET cursor=MAX(cursor,?)
      WHERE agent_id=? AND state='watching'`, cursor, agentId);
  }

  retry(agentId: string, at: number, generation?: number): void {
    this.storage.sql.exec("UPDATE main_thread_completion_watches SET retry_at=? WHERE agent_id=? AND state='watching' AND (? IS NULL OR generation=?)", at, agentId, generation ?? null, generation ?? null);
  }

  /** Keep a tombstone so stale watch admissions cannot restore revoked authority. */
  retire(agentId: string, generation?: number): void {
    this.storage.sql.exec("UPDATE main_thread_completion_watches SET state='retired',authorization_json='' WHERE agent_id=? AND (? IS NULL OR generation=?)", agentId, generation ?? null, generation ?? null);
  }
}
