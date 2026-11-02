-- Reverse lookups keep each retention candidate independent of history size.
CREATE INDEX node_event_retention ON node(CAST(json_extract(payload_json,'$.event_id') AS TEXT)) WHERE kind='event' AND json_valid(payload_json);
CREATE INDEX delivery_event_state ON delivery(event_id,state);
INSERT INTO schema_migration(version,name) VALUES(21,'0021_event_retention');
PRAGMA user_version=21;
