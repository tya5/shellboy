DROP INDEX artifact_thread_page;
CREATE INDEX artifact_thread_page ON artifact(thread_id,updated_at DESC,id);
INSERT INTO schema_migration(version,name) VALUES(58,'0058_artifact_list_order');
PRAGMA user_version=58;
