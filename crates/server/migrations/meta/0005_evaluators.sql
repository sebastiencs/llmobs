CREATE TABLE evaluators (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  config TEXT NOT NULL,
  output_score TEXT NOT NULL,
  connection_id TEXT REFERENCES llm_connections(id),
  secret_enc BLOB,
  version INTEGER NOT NULL DEFAULT 1,
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(project_id, name)
);
CREATE INDEX evaluators_connection ON evaluators(connection_id) WHERE connection_id IS NOT NULL;

CREATE TABLE score_definitions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  data_type TEXT NOT NULL,
  categories TEXT,
  min_value REAL,
  max_value REAL,
  description TEXT,
  archived_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX score_definitions_live ON score_definitions(project_id, name) WHERE archived_at IS NULL;
