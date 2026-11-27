-- Optional exact names belong to one Thread's services (#462).
ALTER TABLE schedule ADD COLUMN name TEXT CHECK(name IS NULL OR kind='service');
CREATE UNIQUE INDEX service_thread_name ON schedule(thread_id,name) WHERE name IS NOT NULL;
INSERT INTO schema_migration(version,name) VALUES(71,'0071_service_name');
PRAGMA user_version=71;
