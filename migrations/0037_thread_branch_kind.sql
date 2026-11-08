ALTER TABLE thread ADD COLUMN branch_kind TEXT CHECK(branch_kind IS NULL OR branch_kind IN ('fork','rewind'));
INSERT INTO schema_migration(version,name) VALUES(37,'0037_thread_branch_kind');
PRAGMA user_version=37;
