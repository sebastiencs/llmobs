CREATE TABLE file_sources (
    traces_file_id TEXT NOT NULL REFERENCES data_files(id) ON DELETE CASCADE,
    spans_file_id TEXT NOT NULL REFERENCES data_files(id) ON DELETE CASCADE,
    PRIMARY KEY (traces_file_id, spans_file_id)
);

CREATE INDEX file_sources_by_spans ON file_sources (spans_file_id);
