/** Persisted project membership is account data, never repository configuration. */
export function initializeConversationProjects(storage: DurableObjectStorage): void {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS conversation_projects (
    agent_id TEXT PRIMARY KEY, project_root_id TEXT NOT NULL, project_name TEXT NOT NULL);`);
}
