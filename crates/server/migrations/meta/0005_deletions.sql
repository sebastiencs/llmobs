CREATE TABLE deletions (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL,
    project_id TEXT,
    kind TEXT NOT NULL,
    user_id TEXT,
    request TEXT NOT NULL,
    requested_by_kind TEXT NOT NULL,
    requested_by TEXT NOT NULL,
    status TEXT NOT NULL,
    trace_count INTEGER,
    session_count INTEGER,
    created_at INTEGER NOT NULL,
    handed_over_at INTEGER,
    applied_at INTEGER,
    error TEXT
);

CREATE INDEX deletions_project ON deletions(project_id, created_at, id);

CREATE INDEX deletions_org ON deletions(org_id, created_at, id);

CREATE INDEX deletions_open ON deletions(created_at, id) WHERE status IN ('resolving', 'pending');

CREATE TABLE deletion_traces (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    trace_id BLOB NOT NULL,
    PRIMARY KEY (deletion_id, trace_id)
);

CREATE TABLE deletion_sessions (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    PRIMARY KEY (deletion_id, session_id)
);

CREATE TABLE deletion_projects (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL,
    PRIMARY KEY (deletion_id, project_id)
);
