ALTER TABLE node ADD COLUMN spill_tools_json TEXT CHECK(spill_tools_json IS NULL OR octet_length(spill_tools_json)<=16384);
CREATE INDEX IF NOT EXISTS node_spill_tools ON node(id,spill_tools_json);
INSERT INTO schema_migration(version,name) VALUES(36,'0036_spill_tool_metadata');
PRAGMA user_version=36;
