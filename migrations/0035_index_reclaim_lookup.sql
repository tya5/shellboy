-- Bounded reclaim resumes from rows that remain, not an additional cursor table.
CREATE INDEX index_reclaim_state ON index_space(state,id);
CREATE INDEX index_reclaim_group_value ON app_meta(value,key) WHERE key>='index.group.' AND key<'index.group/';
INSERT INTO schema_migration(version,name) VALUES(35,'0035_index_reclaim_lookup');
PRAGMA user_version=35;
