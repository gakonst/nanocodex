const MANAGED_AGENT_SETTINGS_TABLE = `
  CREATE TABLE IF NOT EXISTS managed_agent_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    model TEXT NOT NULL CHECK (
      model IN ('gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna', 'gpt-6-astra')
    ),
    thinking TEXT NOT NULL CHECK (thinking IN ('none', 'low', 'medium', 'high', 'xhigh', 'max')),
    reasoning_mode TEXT NOT NULL CHECK (reasoning_mode IN ('standard', 'pro')),
    fast_mode INTEGER NOT NULL CHECK (fast_mode IN (0, 1))
  );
  INSERT OR IGNORE INTO managed_agent_settings
    (singleton, model, thinking, reasoning_mode, fast_mode)
  VALUES (1, 'gpt-5.6-sol', 'high', 'standard', 0);
`;

type AgentSettingsSchemaStorage = Pick<DurableObjectStorage, "sql" | "transactionSync">;

export function initializeManagedAgentSettingsSchema(
  storage: AgentSettingsSchemaStorage,
): void {
  storage.sql.exec(MANAGED_AGENT_SETTINGS_TABLE);
  const installed = storage.sql.exec<{ sql: string }>(
    `SELECT sql FROM sqlite_master
     WHERE type = 'table' AND name = 'managed_agent_settings'`,
  ).one().sql;
  if (installed.includes("'gpt-6-astra'")) return;

  storage.transactionSync(() => {
    storage.sql.exec(`
      CREATE TABLE managed_agent_settings_next (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        model TEXT NOT NULL CHECK (
          model IN ('gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna', 'gpt-6-astra')
        ),
        thinking TEXT NOT NULL CHECK (thinking IN ('none', 'low', 'medium', 'high', 'xhigh', 'max')),
        reasoning_mode TEXT NOT NULL CHECK (reasoning_mode IN ('standard', 'pro')),
        fast_mode INTEGER NOT NULL CHECK (fast_mode IN (0, 1))
      );
      INSERT INTO managed_agent_settings_next
        (singleton, model, thinking, reasoning_mode, fast_mode)
      SELECT singleton, model, thinking, reasoning_mode, fast_mode
      FROM managed_agent_settings;
      DROP TABLE managed_agent_settings;
      ALTER TABLE managed_agent_settings_next RENAME TO managed_agent_settings;
    `);
  });
}
