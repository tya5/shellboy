-- The replacement owner maintains the creation key alongside membership.
ALTER TABLE thread_replacement ADD COLUMN replacement_created_at TEXT;
UPDATE thread_replacement SET replacement_created_at=(SELECT created_at FROM replacement WHERE id=thread_replacement.replacement_id);
CREATE INDEX thread_replacement_created ON thread_replacement(thread_id,replacement_created_at);
CREATE TRIGGER thread_replacement_created_insert AFTER INSERT ON thread_replacement BEGIN
 UPDATE thread_replacement SET replacement_created_at=(SELECT created_at FROM replacement WHERE id=NEW.replacement_id)
 WHERE thread_id=NEW.thread_id AND replacement_id=NEW.replacement_id;
END;
INSERT INTO schema_migration(version,name) VALUES(77,'next_ordered_thread_replacement');
PRAGMA user_version=77;
