ALTER TABLE node ADD COLUMN spill_tools_json TEXT CHECK(spill_tools_json IS NULL OR octet_length(spill_tools_json)<=16384);
CREATE INDEX IF NOT EXISTS node_spill_tools ON node(id,spill_tools_json);
