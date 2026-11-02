-- Index-source GC (#164): find payload-less/deleted nodes that still own
-- embedding jobs or chunks by seek instead of a correlated embedding_job scan
-- per node. Derived indexes only; IF NOT EXISTS keeps downgrade fixtures simple.
CREATE INDEX IF NOT EXISTS jobs_source ON embedding_job(source_node_id);
CREATE INDEX IF NOT EXISTS nodes_index_gc ON node(id) WHERE payload_json IS NULL OR deleted_reason IS NOT NULL;
INSERT INTO schema_migration(version,name) VALUES(20,'0020_gc_index_source');
PRAGMA user_version=20;
