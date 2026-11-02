-- Reverse lookups keep each retention candidate independent of history size.
CREATE INDEX node_event_retention ON node(json_extract(payload_json,'$.event_id')) WHERE kind='event' AND json_valid(payload_json);
CREATE INDEX delivery_event_state ON delivery(event_id,state);
INSERT INTO schema_migration(version,name) VALUES(20,'0020_event_retention');
PRAGMA user_version=20;
