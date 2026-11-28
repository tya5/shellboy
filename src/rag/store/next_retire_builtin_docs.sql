-- The standalone cache is keyed by embedding space and chunk hash, not by
-- source. Retire old guide references without deleting paid vectors.
CREATE TEMP TABLE retire_builtin_docs_revisions (id TEXT PRIMARY KEY);
INSERT INTO retire_builtin_docs_revisions
SELECT revisions.id FROM revisions JOIN documents ON documents.id=revisions.document_id
JOIN sources ON sources.id=documents.source_id WHERE sources.id='@docs' AND sources.kind='builtin';
DELETE FROM chunks WHERE revision_id IN (SELECT id FROM retire_builtin_docs_revisions);
DELETE FROM blocks WHERE revision_id IN (SELECT id FROM retire_builtin_docs_revisions);
DELETE FROM revisions WHERE id IN (SELECT id FROM retire_builtin_docs_revisions);
DROP TABLE retire_builtin_docs_revisions;
DELETE FROM documents WHERE source_id IN (SELECT id FROM sources WHERE id='@docs' AND kind='builtin');
DELETE FROM source_chunk_hashes WHERE source_id IN (SELECT id FROM sources WHERE id='@docs' AND kind='builtin');
UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='generation'
    AND EXISTS(SELECT 1 FROM sources WHERE id='@docs' AND kind='builtin');
DELETE FROM sources WHERE id='@docs' AND kind='builtin';
