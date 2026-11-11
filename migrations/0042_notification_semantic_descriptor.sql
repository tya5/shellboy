ALTER TABLE event ADD COLUMN semantic_json TEXT;
ALTER TABLE node ADD COLUMN notification_event_id TEXT REFERENCES event(id) ON DELETE NO ACTION;
CREATE INDEX node_notification_event ON node(notification_event_id) WHERE notification_event_id IS NOT NULL;
INSERT INTO schema_migration(version,name) VALUES(42,'0042_notification_semantic_descriptor');
PRAGMA user_version=42;
