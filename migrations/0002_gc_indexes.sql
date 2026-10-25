-- Reverse references used by bounded reachability marking and raw reclamation.
CREATE INDEX receipt_message_node ON receipt(message_node_id);
CREATE INDEX replacement_output_node ON replacement(output_node_id);
CREATE INDEX tool_result_node ON tool_result(node_id);
CREATE INDEX output_item_node ON output_item(node_id);
INSERT INTO schema_migration(version,name) VALUES(2,'0002_gc_indexes');
PRAGMA user_version=2;
