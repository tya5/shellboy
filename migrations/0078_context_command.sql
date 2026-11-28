CREATE TABLE context_command (
 id INTEGER PRIMARY KEY,
 thread_id TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
 name TEXT NOT NULL,
 command TEXT NOT NULL,
 timeout_ms INTEGER NOT NULL CHECK(timeout_ms BETWEEN 1 AND 4294967295),
 UNIQUE(thread_id,name)
);
CREATE INDEX context_command_order ON context_command(thread_id,id);
INSERT INTO schema_migration(version,name) VALUES(78,'0078_context_command');
PRAGMA user_version=78;
