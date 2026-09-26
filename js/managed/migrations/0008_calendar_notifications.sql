-- Existing watches establish a silent snapshot baseline on their next full scan.
ALTER TABLE crm_calendar_push_sources ADD COLUMN notifications_initialized INTEGER NOT NULL DEFAULT 0;
UPDATE crm_calendar_push_sources SET sync_token=NULL,page_token=NULL,check_at=0;
CREATE TABLE calendar_notification_snapshots (
 source_id TEXT NOT NULL REFERENCES crm_calendar_push_sources(id) ON DELETE CASCADE,
 event_id TEXT NOT NULL,
 version TEXT NOT NULL,
 snapshot TEXT NOT NULL,
 PRIMARY KEY(source_id,event_id)
);
CREATE TABLE calendar_notification_outbox (
 id TEXT PRIMARY KEY,
 source_id TEXT NOT NULL REFERENCES crm_calendar_push_sources(id) ON DELETE CASCADE,
 input TEXT NOT NULL,
 created_at INTEGER NOT NULL
);
CREATE INDEX calendar_notification_pending ON calendar_notification_outbox(source_id,created_at,id);
