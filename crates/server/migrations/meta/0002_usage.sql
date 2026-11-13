CREATE TABLE usage_daily (
  org_id TEXT NOT NULL, project_id TEXT NOT NULL, day TEXT NOT NULL,
  spans INTEGER NOT NULL DEFAULT 0, bytes INTEGER NOT NULL DEFAULT 0,
  scores INTEGER NOT NULL DEFAULT 0,
  judge_input_tokens INTEGER NOT NULL DEFAULT 0, judge_output_tokens INTEGER NOT NULL DEFAULT 0,
  judge_cost_usd REAL NOT NULL DEFAULT 0,
  PRIMARY KEY (org_id, project_id, day)
);
