-- Legacy payload/reference backfill runs in this same migration transaction.
CREATE TABLE attachment_delivery(
 node_id TEXT NOT NULL REFERENCES node(id),
 thread_id TEXT NOT NULL REFERENCES thread(id),
 target_run_id TEXT REFERENCES run(id),
 status TEXT NOT NULL CHECK(status IN ('accepted','held','applied','cancelled')),
 operation_id TEXT NOT NULL, origin_json TEXT NOT NULL CHECK(json_valid(origin_json)),
 created_at TEXT NOT NULL, PRIMARY KEY(node_id,thread_id));
CREATE INDEX attachment_delivery_pending ON attachment_delivery(thread_id,status,created_at,node_id);
CREATE INDEX attachment_source ON attachment_ref(source_node_id);
CREATE TRIGGER attachment_source_required_insert BEFORE INSERT ON attachment_ref
 WHEN NEW.source_node_id IS NULL BEGIN SELECT RAISE(ABORT,'attachment source is required'); END;
CREATE TRIGGER attachment_source_required_update BEFORE UPDATE OF source_node_id ON attachment_ref
 WHEN NEW.source_node_id IS NULL BEGIN SELECT RAISE(ABORT,'attachment source is required'); END;
INSERT INTO schema_migration(version,name) VALUES(4,'0004_attachment_provenance');
PRAGMA user_version=4;
