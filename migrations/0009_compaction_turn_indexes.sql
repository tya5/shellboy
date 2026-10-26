-- Narrow Turn provenance seeds before consulting replacement_source fanout.
-- Derived covering indexes only; preserve original nodes and receipt assignment.
CREATE INDEX node_turn ON node(turn_id,id);
CREATE INDEX receipt_turn_source ON receipt(turn_id,target_thread_id,message_node_id);
INSERT INTO schema_migration(version,name) VALUES(9,'0009_compaction_turn_indexes');
PRAGMA user_version=9;
