-- Structured context remains distinct by origin: research never upserts a
-- human assertion by predicate. Composite keys keep every edge account-private.
CREATE TABLE crm_facts (
  owner_id TEXT NOT NULL,
  id TEXT NOT NULL,
  record_id TEXT NOT NULL,
  predicate TEXT NOT NULL CHECK (length(predicate) BETWEEN 3 AND 128),
  value_json TEXT NOT NULL CHECK (json_valid(value_json) AND length(CAST(value_json AS BLOB)) <= 16384),
  origin TEXT NOT NULL CHECK (origin IN ('user','source','inferred')),
  sources TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(sources) AND json_type(sources) = 'array' AND json_array_length(sources) <= 50 AND length(CAST(sources AS BLOB)) <= 16384),
  confidence TEXT CHECK (confidence IN ('low','medium','high')),
  rationale TEXT CHECK (rationale IS NULL OR length(trim(rationale)) BETWEEN 1 AND 2000),
  effective_from TEXT CHECK (effective_from IS NULL OR (length(effective_from) = 10 AND date(effective_from,'+0 days') IS NOT NULL AND date(effective_from,'+0 days') = effective_from)),
  effective_to TEXT CHECK (effective_to IS NULL OR (length(effective_to) = 10 AND date(effective_to,'+0 days') IS NOT NULL AND date(effective_to,'+0 days') = effective_to)),
  state TEXT NOT NULL DEFAULT 'current' CHECK (state IN ('current','superseded')),
  created_at INTEGER NOT NULL CHECK (typeof(created_at) = 'integer' AND created_at >= 0),
  updated_at INTEGER NOT NULL CHECK (typeof(updated_at) = 'integer' AND updated_at >= 0),
  PRIMARY KEY (owner_id,id),
  FOREIGN KEY (owner_id,record_id) REFERENCES crm_records(owner_id,id) ON DELETE CASCADE,
  CHECK (origin = 'user' OR json_array_length(sources) > 0),
  CHECK (origin != 'inferred' OR (confidence IS NOT NULL AND rationale IS NOT NULL)),
  CHECK (effective_from IS NULL OR effective_to IS NULL OR effective_from <= effective_to)
);
CREATE INDEX crm_facts_page ON crm_facts(owner_id,created_at,id);
CREATE INDEX crm_facts_record ON crm_facts(owner_id,record_id,created_at,id);
CREATE INDEX crm_facts_predicate ON crm_facts(owner_id,predicate,created_at,id);
CREATE TRIGGER crm_fact_identity_immutable BEFORE UPDATE ON crm_facts
WHEN NEW.owner_id != OLD.owner_id OR NEW.id != OLD.id OR NEW.record_id != OLD.record_id OR NEW.origin != OLD.origin
BEGIN SELECT RAISE(ABORT,'crm_context_immutable'); END;

CREATE TABLE crm_relationships (
  owner_id TEXT NOT NULL,
  id TEXT NOT NULL,
  from_id TEXT NOT NULL,
  to_id TEXT NOT NULL,
  type TEXT NOT NULL CHECK (type IN ('works_at','worked_at','knows','worked_with','referred')),
  role TEXT CHECK (role IS NULL OR (type IN ('works_at','worked_at') AND length(trim(role)) BETWEEN 1 AND 512)),
  description TEXT CHECK (description IS NULL OR length(trim(description)) BETWEEN 1 AND 2000),
  origin TEXT NOT NULL CHECK (origin IN ('user','source','inferred')),
  sources TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(sources) AND json_type(sources) = 'array' AND json_array_length(sources) <= 50 AND length(CAST(sources AS BLOB)) <= 16384),
  confidence TEXT CHECK (confidence IN ('low','medium','high')),
  rationale TEXT CHECK (rationale IS NULL OR length(trim(rationale)) BETWEEN 1 AND 2000),
  effective_from TEXT CHECK (effective_from IS NULL OR (length(effective_from) = 10 AND date(effective_from,'+0 days') IS NOT NULL AND date(effective_from,'+0 days') = effective_from)),
  effective_to TEXT CHECK (effective_to IS NULL OR (length(effective_to) = 10 AND date(effective_to,'+0 days') IS NOT NULL AND date(effective_to,'+0 days') = effective_to)),
  created_at INTEGER NOT NULL CHECK (typeof(created_at) = 'integer' AND created_at >= 0),
  updated_at INTEGER NOT NULL CHECK (typeof(updated_at) = 'integer' AND updated_at >= 0),
  PRIMARY KEY (owner_id,id),
  FOREIGN KEY (owner_id,from_id) REFERENCES crm_records(owner_id,id) ON DELETE CASCADE,
  FOREIGN KEY (owner_id,to_id) REFERENCES crm_records(owner_id,id) ON DELETE CASCADE,
  CHECK (from_id != to_id),
  CHECK (origin = 'user' OR json_array_length(sources) > 0),
  CHECK (origin != 'inferred' OR (confidence IS NOT NULL AND rationale IS NOT NULL)),
  CHECK (effective_from IS NULL OR effective_to IS NULL OR effective_from <= effective_to)
);
CREATE INDEX crm_relationships_page ON crm_relationships(owner_id,created_at,id);
CREATE INDEX crm_relationships_from ON crm_relationships(owner_id,from_id,created_at,id);
CREATE INDEX crm_relationships_to ON crm_relationships(owner_id,to_id,created_at,id);
CREATE INDEX crm_relationships_type ON crm_relationships(owner_id,type,created_at,id);
CREATE TRIGGER crm_relationship_identity_immutable BEFORE UPDATE ON crm_relationships
WHEN NEW.owner_id != OLD.owner_id OR NEW.id != OLD.id OR NEW.from_id != OLD.from_id OR NEW.to_id != OLD.to_id OR NEW.type != OLD.type OR NEW.origin != OLD.origin
BEGIN SELECT RAISE(ABORT,'crm_context_immutable'); END;
CREATE TRIGGER crm_relationship_kinds BEFORE INSERT ON crm_relationships
WHEN NOT EXISTS (SELECT 1 FROM crm_records WHERE owner_id = NEW.owner_id AND id = NEW.from_id AND kind = 'person')
  OR NOT EXISTS (SELECT 1 FROM crm_records WHERE owner_id = NEW.owner_id AND id = NEW.to_id
    AND kind = CASE WHEN NEW.type IN ('works_at','worked_at') THEN 'company' ELSE 'person' END)
BEGIN SELECT RAISE(ABORT,'crm_context_kind'); END;
