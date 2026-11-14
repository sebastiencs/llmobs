# 04 — Metadata database (SQLite)

`meta.db`, SQLite ≥ 3.45, `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`, `busy_timeout=5000`. Writes are rare; every commit is durable before the response (D136). Accessed through `sqlx` with one writer connection (serialized writes through a `Mutex`) and a read pool of 4 read-only connections. Migrations are embedded (`sqlx::migrate!`) and run in `llmobs serve`; downgrade is not supported (12 §Upgrades). `llmobs admin` commands open `meta.db` without running migrations and refuse a database at another migration version (D136).

`meta.db` belongs to `llmobs-server`. The storage engine keeps its own catalog in `catalog.db` (03 §4, D83): data files, WAL checkpoints, media index and its side of deletions. The two databases share no transaction; the only link is the deletion handoff (§ Deletion requests).

The SQL is written in the common subset of SQLite and Postgres where practical (no `AUTOINCREMENT` except where noted, `TEXT` UUIDs, timestamps as `INTEGER` ns), to keep the later Postgres port mechanical.

Conventions: `id TEXT PRIMARY KEY` (UUIDv7), `created_at`/`updated_at INTEGER` (ns), JSON columns are `TEXT` validated by the application.

## Identity & tenancy

```sql
CREATE TABLE users (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,         -- normalized: trimmed, NFC, lowercase (D136)
  name TEXT,
  password_hash TEXT,                 -- argon2id PHC string; NULL for SSO-only users
  email_verified_at INTEGER,
  is_instance_admin INTEGER NOT NULL DEFAULT 0,   -- first user; can manage instance settings
  disabled_at INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);

CREATE TABLE identities (            -- linked OAuth/OIDC accounts
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider TEXT NOT NULL,             -- 'github' | 'google' | 'oidc:<name>'
  subject TEXT NOT NULL,
  email TEXT,
  created_at INTEGER NOT NULL,
  UNIQUE(provider, subject)
);

CREATE TABLE web_sessions (
  id TEXT PRIMARY KEY,                -- SHA-256 hex of the cookie value (32 random bytes, base64url)
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  user_agent TEXT, ip TEXT
);

CREATE TABLE orgs (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, slug TEXT NOT NULL UNIQUE,
  limits TEXT NOT NULL DEFAULT '{}',  -- JSON, see 12 §Limits
  state TEXT NOT NULL DEFAULT 'active', -- 'active' | 'deleting'
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);

CREATE TABLE org_members (
  org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL,                 -- 'owner' | 'admin' | 'member' | 'viewer'
  created_at INTEGER NOT NULL,
  PRIMARY KEY (org_id, user_id)
);

CREATE TABLE projects (
  id TEXT PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
  name TEXT NOT NULL, slug TEXT NOT NULL,
  retention_days INTEGER,             -- NULL = forever
  settings TEXT NOT NULL DEFAULT '{}',-- JSON: trace_completion_timeout_s, media settings, …
  state TEXT NOT NULL DEFAULT 'active',
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  UNIQUE(org_id, slug)
);

CREATE TABLE project_members (        -- optional per-project role override
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL,                 -- 'admin' | 'member' | 'viewer' | 'none'
  PRIMARY KEY (project_id, user_id)
);

CREATE TABLE invites (
  id TEXT PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
  email TEXT NOT NULL,                -- normalized: trimmed, NFC, lowercase (D136)
  role TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  invited_by TEXT REFERENCES users(id),
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, accepted_at INTEGER
);
CREATE UNIQUE INDEX invites_pending ON invites(org_id, email) WHERE accepted_at IS NULL;

CREATE TABLE password_resets (
  token_hash TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, used_at INTEGER
);

CREATE TABLE email_verifications (
  token_hash TEXT PRIMARY KEY,        -- SHA-256 hex of a 32-byte random token
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, used_at INTEGER
);
```

A new invite for the same (org, email) replaces the pending one (D147).

## Keys & credentials

```sql
CREATE TABLE api_keys (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,                 -- 'public' | 'secret'
  scopes TEXT NOT NULL,               -- JSON array: ['ingest'] | ['ingest','read','write'] | ['feedback']
  name TEXT,
  prefix TEXT NOT NULL,               -- type prefix + first 6 random characters, e.g. `sk-llmobs-a1B2c3` (D140)
  key_hash TEXT NOT NULL UNIQUE,      -- SHA-256 hex
  created_by TEXT REFERENCES users(id),
  created_at INTEGER NOT NULL, expires_at INTEGER, last_used_at INTEGER, revoked_at INTEGER
);

CREATE TABLE personal_tokens (        -- for CLI / MCP; act as the user, with the user's roles
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT,
  prefix TEXT NOT NULL,               -- type prefix + first 6 random characters, e.g. `pat-llmobs-a1B2c3` (D140)
  token_hash TEXT NOT NULL UNIQUE,
  scopes TEXT NOT NULL,               -- ['read'] | ['read','write']
  created_at INTEGER NOT NULL, expires_at INTEGER, last_used_at INTEGER, revoked_at INTEGER
);

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
```

Connections belong to the org (D-I-4). `secret_enc` and `extra_headers_enc` are sealed (§ Encryption of secrets); `api_key_hint` and `extra_header_names` are what the API shows.

## Deletion requests

The storage catalog (data files, WAL checkpoints, media) is in `catalog.db` (03 §4). `meta.db` keeps the deletion requests: the API creates them here together with their cascades (03 §7), then hands each one to storage (`register_deletion`, idempotent by id) and sets `handed_over_at`. At startup the server re-registers every request whose `handed_over_at` is null. It sets `status = 'applied'` when storage reports the deletion applied.

```sql
CREATE TABLE deletions (
  id TEXT PRIMARY KEY,
  org_id TEXT NOT NULL, project_id TEXT,
  kind TEXT NOT NULL,                 -- 'traces' | 'user' | 'project' | 'org'
  user_id TEXT,                       -- for kind='user'
  request TEXT NOT NULL,              -- JSON of the original request (filter etc.), for audit
  requested_by TEXT,                  -- user id or api key id
  created_at INTEGER NOT NULL,        -- cut-off
  status TEXT NOT NULL DEFAULT 'pending',  -- 'pending' | 'applied' | 'failed'
  handed_over_at INTEGER,             -- set once storage has registered it
  applied_at INTEGER, error TEXT
);
CREATE TABLE deletion_traces (deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
                              trace_id BLOB NOT NULL, PRIMARY KEY (deletion_id, trace_id));
CREATE TABLE deletion_sessions (deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
                                session_id TEXT NOT NULL, PRIMARY KEY (deletion_id, session_id));
```

`deletions` rows are kept after application (without the data) as an audit trail of erasure requests.

## Pricing

```sql
CREATE TABLE model_prices (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,                -- 'builtin' | 'org' | 'project'
  org_id TEXT, project_id TEXT,
  model_name TEXT NOT NULL,           -- display name
  match_pattern TEXT NOT NULL,        -- regex, anchored, case-insensitive
  provider TEXT,
  valid_from INTEGER NOT NULL DEFAULT 0,
  price_input REAL, price_output REAL, price_cache_read REAL, price_cache_write REAL, price_reasoning REAL,
  tiers TEXT,                         -- JSON [{min_input_tokens, price_input, …}]
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
```

Built-in rows are re-seeded at each startup from the embedded snapshot (delete `scope='builtin'` + insert, in one transaction). The compiled matcher (`RegexSet` per scope) is cached in memory and rebuilt on change.

## Scores definitions

```sql
CREATE TABLE score_definitions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  data_type TEXT NOT NULL,            -- numeric | boolean | categorical | text
  categories TEXT,                    -- JSON [{label, value?}]
  min_value REAL, max_value REAL,
  description TEXT,
  archived_at INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  UNIQUE(project_id, name)
);
```

## Saved views

```sql
CREATE TABLE saved_views (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,                 -- 'traces' | 'spans' | 'sessions' | 'users' | 'sql'
  name TEXT NOT NULL,
  state TEXT NOT NULL,                -- JSON owned by the UI: the list page's search params (range|from+to, filter, search, sort, order, eval, exp, cols) or {query, params, range|from+to} for SQL (D-H-7)
  owner_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
  shared INTEGER NOT NULL DEFAULT 0,  -- 1 = visible to the whole project
  is_default INTEGER NOT NULL DEFAULT 0,  -- one per (project, kind, owner) among private views, one per (project, kind) among shared views (D-H-6)
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE INDEX saved_views_lookup ON saved_views(project_id, kind);
CREATE INDEX saved_views_owner ON saved_views(project_id, owner_user_id);   -- the per-owner cap (D-H-6)
```

Names are not unique: several visible views may share a name. Visibility, sharing, moderation, the two default scopes and the limits (names 1–200 characters, state ≤ 64 KiB, 1,000 views per project and owner) are D-H-6. Preset ranges are stored relative (`range: "24h"`). A view whose filter no longer validates (column removed) still loads; the filter shows as a raw chip with the error.

## Datasets & experiments

Dataset versioning uses validity ranges: an item revision is visible in version *v* iff `valid_from_version ≤ v < coalesce(valid_to_version, ∞)`. Editing an item closes the old revision and inserts a new one in a new version.

```sql
CREATE TABLE datasets (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL, description TEXT,
  input_schema TEXT, expected_schema TEXT,    -- optional JSON Schema
  current_version INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  UNIQUE(project_id, name)
);

CREATE TABLE dataset_versions (
  dataset_id TEXT NOT NULL REFERENCES datasets(id) ON DELETE CASCADE,
  version INTEGER NOT NULL,
  description TEXT,                   -- e.g. "import data.csv (120 items)"
  created_by TEXT, created_at INTEGER NOT NULL,
  PRIMARY KEY (dataset_id, version)
);

CREATE TABLE dataset_items (
  revision_id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL,              -- stable across revisions
  dataset_id TEXT NOT NULL REFERENCES datasets(id) ON DELETE CASCADE,
  input TEXT NOT NULL,                -- JSON
  expected_output TEXT,               -- JSON
  metadata TEXT NOT NULL DEFAULT '{}',
  source_trace_id BLOB, source_span_id BLOB,
  valid_from_version INTEGER NOT NULL,
  valid_to_version INTEGER,
  created_at INTEGER NOT NULL
);
CREATE INDEX dataset_items_live ON dataset_items(dataset_id, valid_to_version);
CREATE INDEX dataset_items_source ON dataset_items(source_trace_id) WHERE source_trace_id IS NOT NULL;

CREATE TABLE experiments (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  dataset_id TEXT REFERENCES datasets(id) ON DELETE SET NULL,
  dataset_version INTEGER,
  name TEXT NOT NULL, description TEXT,
  metadata TEXT NOT NULL DEFAULT '{}',   -- model, prompt, git sha, params…
  server_evaluators TEXT NOT NULL DEFAULT '[]',  -- evaluator ids to run server-side
  status TEXT NOT NULL,               -- 'running' | 'completed' | 'failed'
  created_at INTEGER NOT NULL, finished_at INTEGER
);

CREATE TABLE experiment_items (
  experiment_id TEXT NOT NULL REFERENCES experiments(id) ON DELETE CASCADE,
  dataset_item_id TEXT NOT NULL,      -- item_id (stable)
  repetition INTEGER NOT NULL DEFAULT 0,
  trace_id BLOB,                      -- trace of the task execution
  output TEXT,                        -- JSON copy of the task output (small; full data in trace)
  error TEXT,
  latency_ns INTEGER, cost_total REAL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (experiment_id, dataset_item_id, repetition)
);
```

Experiment scores are rows in the columnar `scores` table with `experiment_id` + `dataset_item_id` set.

## Evaluation

```sql
CREATE TABLE evaluators (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,                 -- 'llm_judge' | 'check' | 'webhook'
  config TEXT NOT NULL,               -- JSON, schema per kind (06)
  output_score TEXT NOT NULL,         -- JSON {name, data_type, categories?}
  version INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  UNIQUE(project_id, name)
);

CREATE TABLE online_rules (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  evaluator_id TEXT NOT NULL REFERENCES evaluators(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  target TEXT NOT NULL,               -- 'trace' | 'span'
  filter TEXT,                        -- filter DSL expression (05)
  variable_mapping TEXT NOT NULL,     -- JSON {var: path} (06)
  sample_rate REAL NOT NULL DEFAULT 1.0,
  budget_usd REAL, budget_period TEXT,-- 'day' | 'month'
  enabled INTEGER NOT NULL DEFAULT 1,
  paused_reason TEXT,                 -- 'budget' | 'errors' | NULL
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);

CREATE TABLE eval_jobs (              -- durable work queue
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  rule_id TEXT, experiment_id TEXT, evaluator_id TEXT NOT NULL,
  trace_id BLOB, span_id BLOB, dataset_item_id TEXT,
  status TEXT NOT NULL,               -- 'queued' | 'running' | 'done' | 'failed'
  attempts INTEGER NOT NULL DEFAULT 0,
  not_before INTEGER NOT NULL,        -- scheduling / backoff
  lease_until INTEGER,
  error TEXT,
  cost_usd REAL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE INDEX eval_jobs_ready ON eval_jobs(status, not_before);

CREATE TABLE rule_spend (
  rule_id TEXT NOT NULL REFERENCES online_rules(id) ON DELETE CASCADE,
  period_start INTEGER NOT NULL,
  spent_usd REAL NOT NULL DEFAULT 0,
  PRIMARY KEY (rule_id, period_start)
);

CREATE TABLE pending_traces (         -- trace-completion detector state (06)
  project_id TEXT NOT NULL, trace_id BLOB NOT NULL,
  last_ingest_at INTEGER NOT NULL, root_ended INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (project_id, trace_id)
);
```

Done/failed `eval_jobs` are pruned after 7 days.

## Alerts

```sql
CREATE TABLE alert_rules (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  metric TEXT NOT NULL,               -- 'error_rate' | 'cost' | 'latency_p95' | 'score'
  config TEXT NOT NULL,               -- JSON (07)
  channels TEXT NOT NULL,             -- JSON array of channel ids
  enabled INTEGER NOT NULL DEFAULT 1,
  state TEXT NOT NULL DEFAULT 'ok',   -- 'ok' | 'firing' | 'no_data'
  last_evaluated_at INTEGER, last_fired_at INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);

CREATE TABLE notification_channels (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,                 -- 'slack' | 'webhook' | 'email'
  name TEXT NOT NULL,
  config_enc BLOB NOT NULL,           -- encrypted JSON (URL, secret, recipients)
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);

CREATE TABLE alert_events (
  id TEXT PRIMARY KEY,
  rule_id TEXT NOT NULL REFERENCES alert_rules(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,                 -- 'fired' | 'resolved'
  value REAL, threshold REAL,
  details TEXT,                       -- JSON
  created_at INTEGER NOT NULL
);

CREATE TABLE notification_deliveries (
  id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES alert_events(id) ON DELETE CASCADE,
  channel_id TEXT NOT NULL,
  status TEXT NOT NULL,               -- 'pending' | 'sent' | 'failed'
  attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
```

## Metering & instance

```sql
CREATE TABLE usage_daily (
  org_id TEXT NOT NULL, project_id TEXT NOT NULL, day TEXT NOT NULL,
  spans INTEGER NOT NULL DEFAULT 0, bytes INTEGER NOT NULL DEFAULT 0,
  scores INTEGER NOT NULL DEFAULT 0,
  judge_input_tokens INTEGER NOT NULL DEFAULT 0, judge_output_tokens INTEGER NOT NULL DEFAULT 0,
  judge_cost_usd REAL NOT NULL DEFAULT 0,
  PRIMARY KEY (org_id, project_id, day)
);

CREATE TABLE instance_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- 'encryption.check': hex HMAC-SHA256 check value of the encryption key (D-I-2)
```

`instance_settings` is created by track I (migration `0003`, D156).

Usage counters are accumulated in memory and flushed to `usage_daily` every 10 s (upsert-add).

## Encryption of secrets

The key is 32 bytes in standard base64. Source order (D-I-1): the `encryption_key` config value (`LLMOBS_ENCRYPTION_KEY`) wins and the file is neither read nor written; otherwise `<data_dir>/secret.key`; otherwise the server generates a key, but only while `instance_settings` has no `encryption.check` — once a check value exists, a missing key fails startup. The file holds the base64 of the 32 key bytes plus one trailing newline, mode 0600, written through a `.tmp` file and a rename so a crash leaves either no file or a whole one; a mode readable by group or other is warned about and still used. `llmobs admin rotate-encryption-key` re-encrypts every sealed column in one transaction, writing `secret.key.new` first; a start that finds a `secret.key.new` whose check value matches the stored one renames it over `secret.key`, finishing an interrupted rotation, and otherwise deletes it (D-I-3).

A sealed value is `0x01` ‖ key id (4 bytes) ‖ nonce (12) ‖ AES-256-GCM ciphertext and tag (D-I-2). The key id is `HMAC-SHA256(key, "llmobs.key-id.v1")[0..4]`; the associated data is `table:column:row_id`, with the row id a hyphenated lowercase UUID; the nonce is 96 random bits. `instance_settings['encryption.check']` holds the lowercase hex of `HMAC-SHA256(key, "llmobs.encryption-check.v1")`: the server records it on the first start of a data directory and refuses a different key afterwards.
