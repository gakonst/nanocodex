-- CRM is private to the authenticated account, including every relationship.
CREATE TABLE crm_records (
  owner_id TEXT NOT NULL,
  id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('person', 'company')),
  name TEXT NOT NULL,
  email TEXT,
  phone TEXT,
  website TEXT,
  title TEXT,
  company_id TEXT,
  tags TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tags) AND json_type(tags) = 'array'),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (owner_id, id),
  FOREIGN KEY (owner_id, company_id) REFERENCES crm_records(owner_id, id),
  CHECK (company_id IS NULL OR kind = 'person')
);
CREATE INDEX crm_records_page ON crm_records(owner_id, created_at, id);
CREATE INDEX crm_records_company ON crm_records(owner_id, company_id);

CREATE TABLE crm_notes (
  owner_id TEXT NOT NULL,
  id TEXT NOT NULL,
  record_id TEXT NOT NULL,
  body TEXT NOT NULL,
  source_url TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (owner_id, id),
  FOREIGN KEY (owner_id, record_id) REFERENCES crm_records(owner_id, id) ON DELETE CASCADE
);
CREATE INDEX crm_notes_page ON crm_notes(owner_id, record_id, created_at, id);

-- A composite FK proves ownership. These triggers additionally prove kind and
-- immutability in the same transaction as the write, including racing requests.
CREATE TRIGGER crm_company_insert BEFORE INSERT ON crm_records
WHEN NEW.company_id IS NOT NULL AND NOT EXISTS (
  SELECT 1 FROM crm_records WHERE owner_id = NEW.owner_id AND id = NEW.company_id AND kind = 'company'
)
BEGIN
  SELECT RAISE(ABORT, 'crm_invalid_company');
END;
CREATE TRIGGER crm_company_update BEFORE UPDATE OF company_id ON crm_records
WHEN NEW.company_id IS NOT NULL AND NOT EXISTS (
  SELECT 1 FROM crm_records WHERE owner_id = NEW.owner_id AND id = NEW.company_id AND kind = 'company'
)
BEGIN
  SELECT RAISE(ABORT, 'crm_invalid_company');
END;
CREATE TRIGGER crm_kind_immutable BEFORE UPDATE OF kind ON crm_records
WHEN NEW.kind != OLD.kind
BEGIN
  SELECT RAISE(ABORT, 'crm_immutable_kind');
END;
CREATE TRIGGER crm_note_record_immutable BEFORE UPDATE OF record_id ON crm_notes
WHEN NEW.record_id != OLD.record_id
BEGIN
  SELECT RAISE(ABORT, 'crm_immutable_record');
END;
