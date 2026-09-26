-- Events and manual observations are account-private and separate from Calendar imports.
CREATE TABLE crm_events (
 owner_id TEXT NOT NULL, id TEXT NOT NULL, title TEXT NOT NULL CHECK(length(trim(title)) BETWEEN 1 AND 512),
 description TEXT, location TEXT, start_at TEXT NOT NULL, start_ms INTEGER NOT NULL,
 end_at TEXT, end_ms INTEGER,
 origin TEXT NOT NULL CHECK(origin IN ('user','source','inferred')),
 sources TEXT NOT NULL CHECK(json_valid(sources) AND json_type(sources)='array' AND json_array_length(sources)<=50),
 confidence TEXT CHECK(confidence IN ('low','medium','high')), rationale TEXT,
 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
 PRIMARY KEY(owner_id,id), CHECK(end_ms IS NULL OR end_ms>=start_ms),
 CHECK((end_at IS NULL)=(end_ms IS NULL)),
 CHECK(origin='user' OR json_array_length(sources)>0),
 CHECK(origin!='inferred' OR (confidence IS NOT NULL AND rationale IS NOT NULL AND length(trim(rationale))>0))
);
CREATE INDEX crm_events_page ON crm_events(owner_id,start_ms,id);
CREATE TABLE crm_event_participation (
 owner_id TEXT NOT NULL, id TEXT NOT NULL, event_id TEXT NOT NULL, record_id TEXT NOT NULL, person_id TEXT,
 status TEXT NOT NULL CHECK(status IN ('invited','expected','attended','declined','unknown')),
 role TEXT NOT NULL CHECK(role IN ('attendee','organizer')),
 origin TEXT NOT NULL CHECK(origin IN ('user','source','inferred')),
 sources TEXT NOT NULL CHECK(json_valid(sources) AND json_type(sources)='array' AND json_array_length(sources)<=50),
 confidence TEXT CHECK(confidence IN ('low','medium','high')), rationale TEXT,
 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
 PRIMARY KEY(owner_id,id), UNIQUE(owner_id,event_id,record_id),
 FOREIGN KEY(owner_id,record_id) REFERENCES crm_records(owner_id,id) ON DELETE CASCADE,
 FOREIGN KEY(owner_id,event_id) REFERENCES crm_events(owner_id,id) ON DELETE CASCADE,
 FOREIGN KEY(owner_id,person_id) REFERENCES crm_records(owner_id,id) ON DELETE CASCADE,
 CHECK(origin='user' OR json_array_length(sources)>0),
 CHECK(origin!='inferred' OR (confidence IS NOT NULL AND rationale IS NOT NULL AND length(trim(rationale))>0))
);
CREATE INDEX crm_participation_person ON crm_event_participation(owner_id,person_id,created_at,id);
CREATE INDEX crm_participation_event ON crm_event_participation(owner_id,event_id,created_at,id);
CREATE TABLE crm_interactions (
 owner_id TEXT NOT NULL, id TEXT NOT NULL, person_id TEXT,
 type TEXT NOT NULL DEFAULT 'interaction', summary TEXT, precision TEXT NOT NULL DEFAULT 'datetime' CHECK(precision IN ('date','datetime')),
 event_id TEXT, meeting_id TEXT, connection_id TEXT, message_id TEXT,
 occurred_at TEXT NOT NULL, occurred_ms INTEGER NOT NULL, body TEXT NOT NULL CHECK(length(trim(body)) BETWEEN 1 AND 20000),
 origin TEXT NOT NULL CHECK(origin IN ('user','source','inferred')),
 sources TEXT NOT NULL CHECK(json_valid(sources) AND json_type(sources)='array' AND json_array_length(sources)<=50),
 confidence TEXT CHECK(confidence IN ('low','medium','high')), rationale TEXT,
 created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
 PRIMARY KEY(owner_id,id),
 FOREIGN KEY(owner_id,person_id) REFERENCES crm_records(owner_id,id),
 FOREIGN KEY(owner_id,event_id) REFERENCES crm_events(owner_id,id),
 FOREIGN KEY(owner_id,meeting_id) REFERENCES crm_meetings(owner_id,id),
 FOREIGN KEY(owner_id,connection_id,message_id) REFERENCES crm_email_imports(owner_id,connection_id,message_id),
 CHECK((connection_id IS NULL)=(message_id IS NULL)),
 CHECK(origin='user' OR json_array_length(sources)>0),
 CHECK(origin!='inferred' OR (confidence IS NOT NULL AND rationale IS NOT NULL AND length(trim(rationale))>0))
);
CREATE INDEX crm_interactions_person ON crm_interactions(owner_id,person_id,occurred_ms,id);
CREATE INDEX crm_interactions_event ON crm_interactions(owner_id,event_id,occurred_ms,id);
CREATE TRIGGER crm_events_immutable BEFORE UPDATE ON crm_events
WHEN NEW.owner_id!=OLD.owner_id OR NEW.id!=OLD.id OR NEW.origin!=OLD.origin
BEGIN SELECT RAISE(ABORT,'crm_event_immutable'); END;
CREATE TRIGGER crm_participation_immutable BEFORE UPDATE ON crm_event_participation
WHEN NEW.owner_id!=OLD.owner_id OR NEW.id!=OLD.id OR NEW.event_id!=OLD.event_id OR NEW.record_id!=OLD.record_id OR NEW.person_id IS NOT OLD.person_id OR NEW.origin!=OLD.origin
BEGIN SELECT RAISE(ABORT,'crm_event_immutable'); END;
CREATE TRIGGER crm_interactions_immutable BEFORE UPDATE ON crm_interactions
WHEN NEW.owner_id!=OLD.owner_id OR NEW.id!=OLD.id OR (NEW.person_id IS NOT OLD.person_id AND NOT (NEW.person_id IS NULL AND NOT EXISTS(SELECT 1 FROM crm_records WHERE owner_id=OLD.owner_id AND id=OLD.person_id))) OR NEW.origin!=OLD.origin
 OR (NEW.event_id IS NOT OLD.event_id AND NOT (NEW.event_id IS NULL AND NOT EXISTS(SELECT 1 FROM crm_events WHERE owner_id=OLD.owner_id AND id=OLD.event_id))) OR (NEW.meeting_id IS NOT OLD.meeting_id AND NOT (NEW.meeting_id IS NULL AND NOT EXISTS(SELECT 1 FROM crm_meetings WHERE owner_id=OLD.owner_id AND id=OLD.meeting_id)))
 OR ((NEW.connection_id IS NOT OLD.connection_id OR NEW.message_id IS NOT OLD.message_id) AND NOT (NEW.connection_id IS NULL AND NEW.message_id IS NULL AND NOT EXISTS(SELECT 1 FROM crm_email_imports WHERE owner_id=OLD.owner_id AND connection_id=OLD.connection_id AND message_id=OLD.message_id)))
BEGIN SELECT RAISE(ABORT,'crm_event_immutable'); END;
CREATE TRIGGER crm_participation_person BEFORE INSERT ON crm_event_participation
WHEN NOT EXISTS(SELECT 1 FROM crm_records WHERE owner_id=NEW.owner_id AND id=NEW.record_id
 AND ((kind='person' AND NEW.person_id IS NEW.record_id) OR (kind='company' AND NEW.person_id IS NULL AND NEW.role='organizer')))
BEGIN SELECT RAISE(ABORT,'crm_invalid_person'); END;

CREATE TRIGGER crm_event_detach_interactions AFTER DELETE ON crm_events
BEGIN UPDATE crm_interactions SET event_id=NULL WHERE owner_id=OLD.owner_id AND event_id=OLD.id; END;
CREATE TRIGGER crm_company_organizer_update BEFORE UPDATE OF role ON crm_event_participation
WHEN NEW.person_id IS NULL AND NEW.role!='organizer'
BEGIN SELECT RAISE(ABORT,'crm_invalid_person'); END;

CREATE TRIGGER crm_meeting_detach_interactions AFTER DELETE ON crm_meetings
BEGIN UPDATE crm_interactions SET meeting_id=NULL WHERE owner_id=OLD.owner_id AND meeting_id=OLD.id; END;
CREATE TRIGGER crm_email_detach_interactions AFTER DELETE ON crm_email_imports
BEGIN UPDATE crm_interactions SET connection_id=NULL,message_id=NULL WHERE owner_id=OLD.owner_id AND connection_id=OLD.connection_id AND message_id=OLD.message_id; END;

ALTER TABLE crm_email_imports ADD COLUMN received_ms INTEGER;

CREATE TABLE crm_interaction_participants (
 owner_id TEXT NOT NULL, interaction_id TEXT NOT NULL, record_id TEXT NOT NULL,
 role TEXT CHECK(role IS NULL OR length(trim(role)) BETWEEN 1 AND 128),
 PRIMARY KEY(owner_id,interaction_id,record_id),
 FOREIGN KEY(owner_id,interaction_id) REFERENCES crm_interactions(owner_id,id) ON DELETE CASCADE,
 FOREIGN KEY(owner_id,record_id) REFERENCES crm_records(owner_id,id) ON DELETE CASCADE
);
CREATE INDEX crm_interaction_participant_record ON crm_interaction_participants(owner_id,record_id,interaction_id);
CREATE TRIGGER crm_person_detach_interactions AFTER DELETE ON crm_records
BEGIN UPDATE crm_interactions SET person_id=NULL WHERE owner_id=OLD.owner_id AND person_id=OLD.id; END;

-- Deleting the final participant intentionally retains the independent global observation.
CREATE TRIGGER crm_interaction_participant_immutable BEFORE UPDATE ON crm_interaction_participants
WHEN NEW.owner_id!=OLD.owner_id OR NEW.interaction_id!=OLD.interaction_id OR NEW.record_id!=OLD.record_id OR NEW.role IS NOT OLD.role
BEGIN SELECT RAISE(ABORT,'crm_event_immutable'); END;
