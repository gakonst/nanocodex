-- Model-grounded research is independent of manual CRM fields and meeting notes.
CREATE TABLE crm_research (
  owner_id TEXT NOT NULL,
  record_id TEXT NOT NULL,
  summary TEXT NOT NULL CHECK (length(trim(summary)) BETWEEN 1 AND 20000),
  company TEXT CHECK (company IS NULL OR length(trim(company)) BETWEEN 1 AND 512),
  title TEXT CHECK (title IS NULL OR length(trim(title)) BETWEEN 1 AND 512),
  website TEXT CHECK (website IS NULL OR length(website) BETWEEN 1 AND 2048),
  sources TEXT NOT NULL CHECK (json_valid(sources) AND json_type(sources) = 'array' AND json_array_length(sources) <= 50),
  status TEXT NOT NULL CHECK (status IN ('complete', 'needs_review')),
  checked_at INTEGER NOT NULL CHECK (typeof(checked_at) = 'integer' AND checked_at >= 0),
  PRIMARY KEY (owner_id, record_id),
  FOREIGN KEY (owner_id, record_id) REFERENCES crm_records(owner_id, id) ON DELETE CASCADE,
  CHECK (status != 'complete' OR json_array_length(sources) > 0)
);
