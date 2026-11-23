CREATE TABLE artifact (
 id TEXT PRIMARY KEY NOT NULL,
 thread_id TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
 created_at TEXT NOT NULL,
 updated_at TEXT NOT NULL
);
CREATE INDEX artifact_thread_page ON artifact(thread_id,updated_at DESC,id);
CREATE TABLE artifact_version (
 artifact_id TEXT NOT NULL REFERENCES artifact(id) ON DELETE CASCADE,
 version INTEGER NOT NULL CHECK(version>=1),
 uri TEXT NOT NULL CHECK(length(CAST(uri AS BLOB)) BETWEEN 1 AND 2048),
 title TEXT NOT NULL CHECK(length(CAST(title AS BLOB)) BETWEEN 1 AND 256),
 mime TEXT CHECK(length(CAST(mime AS BLOB))<=128),
 created_at TEXT NOT NULL,
 PRIMARY KEY(artifact_id,version)
) WITHOUT ROWID;
INSERT INTO schema_migration(version,name) VALUES(59,'next_thread_artifacts');
PRAGMA user_version=59;
