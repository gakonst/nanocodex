CREATE TABLE crm_meetings (
  owner_id TEXT NOT NULL,
  id TEXT NOT NULL,
  connection_id TEXT NOT NULL,
  calendar_id TEXT NOT NULL,
  event_id TEXT NOT NULL,
  title TEXT NOT NULL,
  description TEXT,
  location TEXT,
  html_link TEXT,
  start_time TEXT NOT NULL,
  end_time TEXT NOT NULL,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  all_day INTEGER NOT NULL CHECK (all_day IN (0,1)),
  organizer TEXT NOT NULL CHECK (json_valid(organizer)),
  status TEXT NOT NULL,
  eligible INTEGER NOT NULL CHECK (eligible IN (0,1)),
  self_declined INTEGER NOT NULL CHECK (self_declined IN (0,1)),
  attendees_complete INTEGER NOT NULL DEFAULT 1 CHECK (attendees_complete IN (0,1)),
  source_updated TEXT,
  source_revision INTEGER NOT NULL,
  import_token TEXT NOT NULL,
  skipped INTEGER NOT NULL DEFAULT 0 CHECK (skipped IN (0,1)),
  skip_reason TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (owner_id,id),
  UNIQUE (owner_id,connection_id,calendar_id,event_id)
);
CREATE INDEX crm_meetings_page ON crm_meetings(owner_id,start_ms,id);
CREATE TABLE crm_meeting_attendees (
  owner_id TEXT NOT NULL,
  meeting_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  email TEXT,
  name TEXT,
  response_status TEXT,
  person_id TEXT,
  PRIMARY KEY (owner_id,meeting_id,ordinal),
  FOREIGN KEY (owner_id,meeting_id) REFERENCES crm_meetings(owner_id,id) ON DELETE CASCADE,
  FOREIGN KEY (owner_id,person_id) REFERENCES crm_records(owner_id,id)
);
CREATE INDEX crm_meeting_people ON crm_meeting_attendees(owner_id,person_id,meeting_id);
CREATE INDEX crm_people_email ON crm_records(owner_id,kind,lower(trim(email)));
CREATE TRIGGER crm_attendee_person_insert BEFORE INSERT ON crm_meeting_attendees
WHEN NEW.person_id IS NOT NULL AND NOT EXISTS (
  SELECT 1 FROM crm_records WHERE owner_id=NEW.owner_id AND id=NEW.person_id AND kind='person'
)
BEGIN SELECT RAISE(ABORT,'crm_invalid_person'); END;
CREATE TRIGGER crm_attendee_person_update BEFORE UPDATE OF person_id ON crm_meeting_attendees
WHEN NEW.person_id IS NOT NULL AND NOT EXISTS (
  SELECT 1 FROM crm_records WHERE owner_id=NEW.owner_id AND id=NEW.person_id AND kind='person'
)
BEGIN SELECT RAISE(ABORT,'crm_invalid_person'); END;
CREATE TRIGGER crm_attendee_person_delete BEFORE DELETE ON crm_records
BEGIN UPDATE crm_meeting_attendees SET person_id=NULL WHERE owner_id=OLD.owner_id AND person_id=OLD.id; END;
CREATE TABLE crm_meeting_notes (
  owner_id TEXT NOT NULL,
  id TEXT NOT NULL,
  meeting_id TEXT NOT NULL,
  body TEXT NOT NULL CHECK (length(trim(body)) BETWEEN 1 AND 20000),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (owner_id,id),
  FOREIGN KEY (owner_id,meeting_id) REFERENCES crm_meetings(owner_id,id) ON DELETE CASCADE
);
CREATE INDEX crm_meeting_notes_page ON crm_meeting_notes(owner_id,meeting_id,created_at,id);
CREATE TRIGGER crm_meeting_note_parent BEFORE UPDATE OF meeting_id ON crm_meeting_notes
WHEN NEW.meeting_id != OLD.meeting_id
BEGIN SELECT RAISE(ABORT,'crm_immutable_meeting'); END;

-- Retain unseen cancellations so an older overlapping sync cannot resurrect them.
CREATE TABLE crm_calendar_tombstones (
  owner_id TEXT NOT NULL,
  meeting_id TEXT NOT NULL,
  source_revision INTEGER NOT NULL,
  PRIMARY KEY (owner_id,meeting_id)
);
