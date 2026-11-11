CREATE TABLE data_files (
    id TEXT PRIMARY KEY,
    table_name TEXT NOT NULL,
    project_id TEXT NOT NULL,
    day TEXT NOT NULL,
    level INTEGER NOT NULL,
    path TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    footer_size INTEGER NOT NULL,
    row_count INTEGER NOT NULL,
    min_time INTEGER NOT NULL,
    max_time INTEGER NOT NULL,
    min_ingest_time INTEGER NOT NULL,
    max_ingest_time INTEGER NOT NULL,
    fully_merged INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL DEFAULT 'active',
    created_at INTEGER NOT NULL,
    obsoleted_at INTEGER
);

CREATE INDEX data_files_lookup
    ON data_files (table_name, project_id, day)
    WHERE state = 'active';

CREATE INDEX data_files_gc
    ON data_files (obsoleted_at)
    WHERE state = 'obsolete';

CREATE TABLE staged_objects (
    path TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL
);

CREATE TABLE table_state (
    table_name TEXT PRIMARY KEY,
    flushed_seq INTEGER NOT NULL
);
