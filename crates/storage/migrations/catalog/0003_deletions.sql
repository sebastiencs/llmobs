ALTER TABLE data_files ADD COLUMN deletions_applied_seq INTEGER NOT NULL DEFAULT 0;

CREATE TABLE deletions (
    id TEXT PRIMARY KEY,
    deletion_seq INTEGER NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    cutoff INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    registered_at INTEGER NOT NULL,
    applied_at INTEGER
);

CREATE INDEX deletions_pending ON deletions (deletion_seq) WHERE status = 'pending';

CREATE TABLE deletion_traces (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    trace_id BLOB NOT NULL,
    PRIMARY KEY (deletion_id, trace_id)
);

CREATE TABLE deletion_users (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    PRIMARY KEY (deletion_id, user_id)
);

CREATE TABLE deletion_sessions (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    PRIMARY KEY (deletion_id, session_id)
);
