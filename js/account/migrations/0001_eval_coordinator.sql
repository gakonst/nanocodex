PRAGMA foreign_keys = ON;

CREATE TABLE worksets(
  id INTEGER PRIMARY KEY,
  profile TEXT NOT NULL,
  digest TEXT NOT NULL UNIQUE,
  created_at_ms INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('materializing', 'ready'))
);

CREATE TABLE task_definitions(
  id INTEGER PRIMARY KEY,
  workset_id INTEGER NOT NULL REFERENCES worksets(id) ON DELETE CASCADE,
  public_id TEXT NOT NULL,
  selector TEXT NOT NULL,
  name TEXT NOT NULL,
  digest TEXT NOT NULL,
  task_key TEXT NOT NULL,
  UNIQUE(workset_id, public_id),
  UNIQUE(workset_id, selector)
);

CREATE TABLE eval_tasks(
  id INTEGER PRIMARY KEY,
  public_id TEXT NOT NULL,
  workset_id INTEGER NOT NULL REFERENCES worksets(id) ON DELETE CASCADE,
  definition_id INTEGER NOT NULL REFERENCES task_definitions(id) ON DELETE CASCADE,
  family_key TEXT NOT NULL,
  harness TEXT NOT NULL,
  model TEXT NOT NULL,
  thinking TEXT NOT NULL,
  web_search INTEGER NOT NULL DEFAULT 0,
  repetition INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('unclaimed', 'running', 'success', 'failed')),
  claim_id TEXT,
  worker TEXT,
  started_at_ms INTEGER,
  finished_at_ms INTEGER,
  lease_expires_at_ms INTEGER,
  artifact_key TEXT,
  error TEXT,
  UNIQUE(workset_id, family_key, repetition),
  CHECK(
    (state = 'unclaimed' AND claim_id IS NULL AND started_at_ms IS NULL AND finished_at_ms IS NULL AND lease_expires_at_ms IS NULL) OR
    (state = 'running' AND claim_id IS NOT NULL AND started_at_ms IS NOT NULL AND finished_at_ms IS NULL AND lease_expires_at_ms IS NOT NULL) OR
    (state IN ('success', 'failed') AND claim_id IS NOT NULL AND started_at_ms IS NOT NULL AND finished_at_ms IS NOT NULL AND lease_expires_at_ms IS NULL)
  )
);

CREATE INDEX eval_tasks_claimable
  ON eval_tasks(workset_id, state, family_key, repetition);
CREATE INDEX eval_tasks_definition
  ON eval_tasks(workset_id, definition_id);
CREATE INDEX eval_tasks_worker
  ON eval_tasks(worker, state);

CREATE TABLE coordinate_results(
  coordinate_id INTEGER PRIMARY KEY REFERENCES eval_tasks(id) ON DELETE CASCADE,
  case_key TEXT,
  status TEXT,
  outcome TEXT,
  input_tokens INTEGER,
  cached_input_tokens INTEGER,
  output_tokens INTEGER,
  reasoning_output_tokens INTEGER,
  total_tokens INTEGER,
  cost_usd REAL,
  agent_duration_ms INTEGER
);

CREATE TABLE eval_attempts(
  id INTEGER PRIMARY KEY,
  workset_id INTEGER NOT NULL REFERENCES worksets(id) ON DELETE CASCADE,
  task_id INTEGER NOT NULL REFERENCES eval_tasks(id) ON DELETE CASCADE,
  claim_id TEXT NOT NULL UNIQUE,
  worker TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('running', 'passed', 'failed', 'infrastructure_failed', 'interrupted')),
  started_at_ms INTEGER NOT NULL,
  finished_at_ms INTEGER,
  artifact_key TEXT,
  error TEXT
);

CREATE INDEX eval_attempts_task ON eval_attempts(task_id, started_at_ms);
CREATE INDEX eval_attempts_worker ON eval_attempts(worker, state);
CREATE INDEX eval_attempts_recent ON eval_attempts(workset_id, finished_at_ms);

CREATE TABLE cluster_nodes(
  id TEXT PRIMARY KEY,
  observed_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  payload_json TEXT NOT NULL
);
