-- Append-only provenance survives deletion of an imported note, so replay does
-- not undo a user's deletion. Composite ownership keeps accounts isolated.
CREATE TABLE crm_email_imports (
  owner_id TEXT NOT NULL,
  connection_id TEXT NOT NULL,
  message_id TEXT NOT NULL,
  record_id TEXT NOT NULL,
  note_id TEXT NOT NULL,
  imported_at INTEGER NOT NULL,
  PRIMARY KEY(owner_id,connection_id,message_id),
  FOREIGN KEY(owner_id,record_id) REFERENCES crm_records(owner_id,id) ON DELETE CASCADE
);

-- Per-envelope completion receipts include unmatched/deleted mail so bounded
-- wake retries advance even when no CRM note was produced for a message.
CREATE TABLE crm_email_push_receipts (
  owner_id TEXT NOT NULL,
  event_key TEXT NOT NULL,
  message_id TEXT NOT NULL,
  completed_at INTEGER NOT NULL,
  PRIMARY KEY(owner_id,event_key,message_id)
);
