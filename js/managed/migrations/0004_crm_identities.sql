-- Aliases belong to records, not to a globally unique person. Two people may
-- share an email or handle; resolution must preserve that ambiguity.
CREATE TABLE crm_identities (
  owner_id TEXT NOT NULL,
  id TEXT NOT NULL,
  record_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('email','github','x','linkedin','telegram','website','domain','aka')),
  value TEXT NOT NULL CHECK (length(value) BETWEEN 1 AND 2048),
  normalized TEXT NOT NULL CHECK (length(normalized) BETWEEN 1 AND 2048),
  origin TEXT NOT NULL CHECK (origin IN ('user','source')),
  source_ref TEXT CHECK (source_ref IS NULL OR length(source_ref) BETWEEN 1 AND 2048),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (owner_id,id),
  FOREIGN KEY (owner_id,record_id) REFERENCES crm_records(owner_id,id) ON DELETE CASCADE,
  UNIQUE (owner_id,record_id,kind,normalized),
  CHECK (origin != 'source' OR source_ref IS NOT NULL)
);
CREATE INDEX crm_identities_page ON crm_identities(owner_id,record_id,created_at,id);
CREATE INDEX crm_identities_match ON crm_identities(owner_id,kind,normalized,record_id);
CREATE TRIGGER crm_identity_immutable BEFORE UPDATE OF owner_id,id,record_id,kind,value,normalized,origin,source_ref ON crm_identities
WHEN NEW.owner_id IS NOT OLD.owner_id OR NEW.id IS NOT OLD.id OR NEW.record_id IS NOT OLD.record_id
  OR NEW.kind IS NOT OLD.kind OR NEW.value IS NOT OLD.value OR NEW.normalized IS NOT OLD.normalized
  OR NEW.origin IS NOT OLD.origin OR NEW.source_ref IS NOT OLD.source_ref
BEGIN
  SELECT RAISE(ABORT, 'crm_immutable_identity');
END;
