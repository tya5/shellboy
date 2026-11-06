-- An index space may store int8 vectors (#253). NULL = float32 (every existing
-- space); otherwise the fixed multiplier from unit-length float to int8, chosen
-- from that space's data and immutable for its lifetime. Logical byte accounting
-- follows the element size. No existing row changes.
ALTER TABLE index_space ADD COLUMN int8_scale REAL CHECK(int8_scale IS NULL OR int8_scale>0);
-- embedding_chunk.job_id references embedding_job: without this index, deleting one
-- job of a collected space scans every chunk of the spaces that remain (quadratic).
CREATE INDEX chunks_job ON embedding_chunk(job_id);
DROP TRIGGER IF EXISTS logical_index_embedding_chunk_insert;
DROP TRIGGER IF EXISTS logical_index_embedding_chunk_delete;
DROP TRIGGER IF EXISTS logical_index_embedding_chunk_update;
DROP TRIGGER IF EXISTS logical_index_space_insert;
DROP TRIGGER IF EXISTS logical_index_space_delete;
DROP TRIGGER IF EXISTS logical_index_space_update;
CREATE TRIGGER logical_index_embedding_chunk_insert AFTER INSERT ON embedding_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE((SELECT s.dimension*(CASE WHEN s.int8_scale IS NULL THEN 4 ELSE 1 END) FROM index_space s WHERE s.id=NEW.space_id),0)),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER logical_index_embedding_chunk_delete AFTER DELETE ON embedding_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+-(COALESCE((SELECT s.dimension*(CASE WHEN s.int8_scale IS NULL THEN 4 ELSE 1 END) FROM index_space s WHERE s.id=OLD.space_id),0)),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER logical_index_embedding_chunk_update AFTER UPDATE OF space_id ON embedding_chunk BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(COALESCE((SELECT s.dimension*(CASE WHEN s.int8_scale IS NULL THEN 4 ELSE 1 END) FROM index_space s WHERE s.id=NEW.space_id),0))-(COALESCE((SELECT s.dimension*(CASE WHEN s.int8_scale IS NULL THEN 4 ELSE 1 END) FROM index_space s WHERE s.id=OLD.space_id),0)),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER logical_index_space_insert AFTER INSERT ON index_space BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(SELECT count(*) FROM embedding_chunk c WHERE c.space_id=NEW.id)*NEW.dimension*(CASE WHEN NEW.int8_scale IS NULL THEN 4 ELSE 1 END),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER logical_index_space_delete AFTER DELETE ON index_space BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes-(SELECT count(*) FROM embedding_chunk c WHERE c.space_id=OLD.id)*OLD.dimension*(CASE WHEN OLD.int8_scale IS NULL THEN 4 ELSE 1 END),revision=revision+1 WHERE category='logical.index';
END;
CREATE TRIGGER logical_index_space_update AFTER UPDATE OF id,dimension ON index_space BEGIN
 UPDATE quota_counter SET logical_bytes=logical_bytes+(SELECT count(*) FROM embedding_chunk c WHERE c.space_id=NEW.id)*NEW.dimension*(CASE WHEN NEW.int8_scale IS NULL THEN 4 ELSE 1 END)-(SELECT count(*) FROM embedding_chunk c WHERE c.space_id=OLD.id)*OLD.dimension*(CASE WHEN OLD.int8_scale IS NULL THEN 4 ELSE 1 END),revision=revision+1 WHERE category='logical.index';
END;
INSERT INTO schema_migration(version,name) VALUES(33,'0033_index_int8_scale');
PRAGMA user_version=33;
