CREATE TABLE instance_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE llm_connections (
  id TEXT PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  adapter TEXT NOT NULL,
  base_url TEXT,
  secret_enc BLOB NOT NULL,
  api_key_hint TEXT NOT NULL,
  extra_headers_enc BLOB,
  extra_header_names TEXT NOT NULL DEFAULT '[]',
  default_model TEXT,
  structured_output TEXT NOT NULL DEFAULT 'native',
  max_concurrency INTEGER NOT NULL DEFAULT 4,
  requests_per_minute INTEGER NOT NULL DEFAULT 300,
  status TEXT NOT NULL DEFAULT 'ok',
  last_error TEXT,
  last_error_at INTEGER,
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(org_id, name)
);
