-- Provider sync state is separate from user-authored CRM records and notes.
CREATE TABLE crm_calendar_push_sources (
  id TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  window_from INTEGER NOT NULL,
  window_to INTEGER NOT NULL,
  rebuild_at INTEGER NOT NULL,
  connection_id TEXT NOT NULL,
  calendar_id TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  sync_token TEXT,
  page_token TEXT,
  generation TEXT NOT NULL,
  dirty INTEGER NOT NULL DEFAULT 1,
  last_error TEXT,
  renewal_error TEXT,
  check_at INTEGER NOT NULL DEFAULT 0,
  renew_at INTEGER NOT NULL DEFAULT 0,
  lease TEXT,
  lease_until INTEGER NOT NULL DEFAULT 0,
  UNIQUE(owner_id, connection_id, calendar_id)
);
CREATE INDEX crm_calendar_push_due ON crm_calendar_push_sources(enabled,check_at);
CREATE TABLE crm_calendar_push_channels (
  id TEXT PRIMARY KEY,
  source_id TEXT NOT NULL REFERENCES crm_calendar_push_sources(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL,
  resource_id TEXT,
  expires_at INTEGER NOT NULL
);
CREATE INDEX crm_calendar_push_channel_source ON crm_calendar_push_channels(source_id);
CREATE TABLE crm_calendar_push_seen (
  source_id TEXT NOT NULL REFERENCES crm_calendar_push_sources(id) ON DELETE CASCADE,
  event_id TEXT NOT NULL,
  generation TEXT NOT NULL,
  PRIMARY KEY(source_id,event_id)
);
