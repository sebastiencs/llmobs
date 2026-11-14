CREATE TABLE notification_channels (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  config_enc BLOB NOT NULL,
  target_hint TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX notification_channels_name ON notification_channels(project_id, name);

CREATE TABLE alert_rules (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  metric TEXT NOT NULL,
  config TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  cooldown_s INTEGER NOT NULL DEFAULT 3600,
  notify_while_firing INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL DEFAULT 'ok',
  clear_streak INTEGER NOT NULL DEFAULT 0,
  last_value REAL,
  last_count INTEGER,
  last_evaluated_at INTEGER,
  last_fired_at INTEGER,
  last_notified_at INTEGER,
  last_error TEXT,
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX alert_rules_name ON alert_rules(project_id, name);
CREATE INDEX alert_rules_due ON alert_rules(enabled, last_evaluated_at);

CREATE TABLE alert_rule_channels (
  rule_id TEXT NOT NULL REFERENCES alert_rules(id) ON DELETE CASCADE,
  channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
  PRIMARY KEY (rule_id, channel_id)
);
CREATE INDEX alert_rule_channels_channel ON alert_rule_channels(channel_id);

CREATE TABLE alert_events (
  id TEXT PRIMARY KEY,
  rule_id TEXT NOT NULL REFERENCES alert_rules(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  value REAL,
  count INTEGER,
  threshold REAL NOT NULL,
  window_from INTEGER NOT NULL,
  window_to INTEGER NOT NULL,
  details TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX alert_events_rule ON alert_events(rule_id, created_at);
CREATE INDEX alert_events_created ON alert_events(created_at);

CREATE TABLE notification_deliveries (
  id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES alert_events(id) ON DELETE CASCADE,
  channel_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
  status TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER,
  last_error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX notification_deliveries_due ON notification_deliveries(status, next_attempt_at);
CREATE INDEX notification_deliveries_event ON notification_deliveries(event_id);
CREATE INDEX notification_deliveries_channel ON notification_deliveries(channel_id, updated_at);
