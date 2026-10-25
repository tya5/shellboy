mod sync;
use super::*;
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    time::Duration,
};
use sync::{FileIndexPolicy, SyncCounts, collect_missing, index_derived};

const SCHEMA: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE sources(id TEXT PRIMARY KEY,name TEXT UNIQUE NOT NULL,root TEXT NOT NULL,kind TEXT NOT NULL,last_scan TEXT,error TEXT);
CREATE TABLE documents(id TEXT PRIMARY KEY,source_id TEXT NOT NULL REFERENCES sources(id),key TEXT NOT NULL,active_revision TEXT,status TEXT NOT NULL DEFAULT 'ready',error TEXT,UNIQUE(source_id,key));
CREATE TABLE revisions(id TEXT PRIMARY KEY,document_id TEXT NOT NULL REFERENCES documents(id),source_hash TEXT NOT NULL,body_hash TEXT NOT NULL,extractor TEXT NOT NULL,kind TEXT NOT NULL,title TEXT NOT NULL,provenance TEXT NOT NULL,created_at TEXT NOT NULL);
CREATE TABLE blocks(revision_id TEXT NOT NULL REFERENCES revisions(id),ordinal INTEGER NOT NULL,text TEXT NOT NULL,location TEXT NOT NULL,PRIMARY KEY(revision_id,ordinal));
CREATE TABLE chunks(id TEXT PRIMARY KEY,revision_id TEXT NOT NULL REFERENCES revisions(id),ordinal INTEGER NOT NULL,start INTEGER NOT NULL,end INTEGER NOT NULL,text TEXT NOT NULL,hash TEXT NOT NULL,location TEXT NOT NULL,UNIQUE(revision_id,ordinal));
CREATE TABLE spaces(id TEXT PRIMARY KEY,dimension INTEGER NOT NULL,profile TEXT NOT NULL,model TEXT NOT NULL,endpoint_hash TEXT NOT NULL,table_name TEXT NOT NULL);
CREATE TABLE embeddings(space_id TEXT NOT NULL REFERENCES spaces(id),hash TEXT NOT NULL,vec_rowid INTEGER NOT NULL,PRIMARY KEY(space_id,hash));
PRAGMA user_version=1;";

pub(super) struct Store {
    pub db: Connection,
    pub uuid: String,
    _lock: Option<File>,
}
pub(super) fn open(path: &Path, write: bool) -> Result<Store> {
    if !path.is_file() {
        return Err(err(
            "db_not_found",
            "Knowledge database is not initialized; run shellboy rag init",
        ));
    }
    let lock = if write {
        Some(writer_lock(path)?)
    } else {
        None
    };
    crate::storage::register_vec()
        .map_err(|_| err("database_failed", "Cannot initialize sqlite-vec"))?;
    let flags = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let db = Connection::open_with_flags(path, flags)?;
    db.busy_timeout(Duration::from_secs(2))?;
    if !write {
        db.execute_batch("PRAGMA query_only=ON;")?;
    }
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 1 {
        return Err(err(
            "schema_unsupported",
            "Knowledge database schema is not supported; no migration was attempted",
        ));
    }
    let uuid: String = db
        .query_row("SELECT value FROM meta WHERE key='uuid'", [], |r| r.get(0))
        .map_err(|_| err("schema_unsupported", "Not a Shellboy document database"))?;
    if uuid
        .strip_prefix("rag-")
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .is_none()
    {
        return Err(err(
            "schema_unsupported",
            "Not a Shellboy document database",
        ));
    }
    if write {
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
    }
    Ok(Store {
        db,
        uuid,
        _lock: lock,
    })
}
fn writer_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("rag-lock"))?;
    file.try_lock_exclusive().map_err(|_| {
        err(
            "busy",
            "Another retrieval writer is active; retry after it finishes",
        )
    })?;
    Ok(file)
}
fn selection_fingerprint(backend: Option<&Path>, adapter: Option<&Path>) -> Result<String> {
    use std::io::Read;
    let mut digest = Sha256::new();
    digest.update(b"rag-extractor-selection-v1");
    for path in [backend, adapter] {
        if let Some(path) = path {
            let canonical = std::fs::canonicalize(path).map_err(|_| {
                err(
                    "extractor_failed",
                    "Explicit extractor/adapter is unavailable",
                )
            })?;
            let metadata = std::fs::metadata(&canonical)?;
            if !metadata.is_file() || metadata.len() > 256 * 1024 * 1024 {
                return Err(err(
                    "extractor_failed",
                    "Extractor inputs must be regular files of at most 256 MiB",
                ));
            }
            digest.update(canonical.to_string_lossy().as_bytes());
            let mut file = File::open(&canonical)?;
            let mut buffer = [0u8; 65536];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
        } else {
            digest.update(b"none");
        }
    }
    if let Some(manifest) = adapter {
        let value: Value = serde_json::from_slice(&bounded_read(manifest)?)
            .map_err(|_| err("extractor_failed", "Invalid extractor manifest"))?;
        if let Some(executable) = value.get("executable").and_then(Value::as_str) {
            let executable = absolute(
                Path::new(executable),
                manifest.parent().unwrap_or_else(|| Path::new(".")),
            );
            let metadata = std::fs::metadata(&executable)
                .map_err(|_| err("extractor_failed", "Adapter executable is unavailable"))?;
            if !metadata.is_file() || metadata.len() > 256 * 1024 * 1024 {
                return Err(err(
                    "extractor_failed",
                    "Adapter executable must be a regular file of at most 256 MiB",
                ));
            }
            let mut file = File::open(executable)
                .map_err(|_| err("extractor_failed", "Adapter executable is unavailable"))?;
            let mut buffer = [0u8; 65536];
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                digest.update(&buffer[..n]);
            }
        }
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn prune_embeddings(db: &Connection) -> Result<()> {
    let spaces = {
        let mut q = db.prepare("SELECT id,table_name FROM spaces")?;
        q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (space, table) in spaces {
        if space.len() != 64
            || !space.bytes().all(|c| c.is_ascii_hexdigit())
            || table != format!("rag_vec_{}", space.get(..32).unwrap_or(""))
        {
            return Err(err("schema_unsupported", "Invalid vector table identity"));
        }
        let rows = {
            let mut q=db.prepare("SELECT vec_rowid FROM embeddings e WHERE space_id=?1 AND NOT EXISTS(SELECT 1 FROM chunks c WHERE c.hash=e.hash)")?;
            q.query_map([&space], |r| r.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for row in rows {
            db.execute(&format!("DELETE FROM {table} WHERE rowid=?1"), [row])?;
            db.execute(
                "DELETE FROM embeddings WHERE space_id=?1 AND vec_rowid=?2",
                params![space, row],
            )?;
        }
    }
    Ok(())
}
pub(super) fn initialize(path: &Path) -> Result<Value> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = writer_lock(path)?;
    if path.exists() {
        drop(_lock);
        let s = open(path, false)?;
        return Ok(json!({"database_uuid":s.uuid,"already_initialized":true}));
    }
    crate::storage::register_vec()
        .map_err(|_| err("database_failed", "Cannot initialize sqlite-vec"))?;
    let mut db = Connection::open(path)?;
    db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA foreign_keys=ON;")?;
    let tx = db.transaction()?;
    tx.execute_batch(SCHEMA)?;
    let uuid = format!("rag-{}", uuid::Uuid::new_v4());
    tx.execute("INSERT INTO meta VALUES('uuid',?1)", [&uuid])?;
    tx.execute("INSERT INTO meta VALUES('generation','0')", [])?;
    tx.execute("INSERT INTO sources(id,name,root,kind) VALUES('@docs','@docs','','builtin'),('@derived','derived','','derived')",[])?;
    tx.commit()?;
    Ok(json!({"database_uuid":uuid,"already_initialized":false}))
}
pub(super) fn resolve_source(db: &Connection, id: &str) -> Result<String> {
    db.query_row("SELECT id FROM sources WHERE (id=?1 OR name=?1) AND kind!='builtin' ORDER BY CASE WHEN id=?1 THEN 0 ELSE 1 END",[id],|r|r.get(0)).optional()?.ok_or_else(||err("source_not_found","Registered source does not exist"))
}
pub(super) fn source_command(path: &Path, command: SourceCommand, cwd: &Path) -> Result<Value> {
    let write = !matches!(command, SourceCommand::List);
    let mut s = open(path, write)?;
    match command {
        SourceCommand::Add { path, name } => {
            if name.is_empty()
                || name.len() > 128
                || name.starts_with('@')
                || name.contains(':')
                || matches!(name.as_str(), "docs" | "history" | "derived")
            {
                return Err(err(
                    "invalid_argument",
                    "Source name is empty, reserved, or exceeds 128 bytes",
                ));
            }
            let root = std::fs::canonicalize(absolute(&path, cwd))
                .map_err(|_| err("source_unavailable", "Source path is unavailable"))?;
            let root = root
                .to_str()
                .ok_or_else(|| err("invalid_argument", "Source path must be Unicode"))?;
            let existing: Option<(String, String)> =
                s.db.query_row("SELECT id,root FROM sources WHERE name=?1", [&name], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            if let Some((id, previous)) = existing {
                if previous != root {
                    return Err(err(
                        "conflict",
                        "Source name already refers to another path",
                    ));
                }
                return Ok(json!({"source_id":id,"name":name,"root":root,"existing":true}));
            }
            let count: i64 = s.db.query_row(
                "SELECT COUNT(*) FROM sources WHERE kind!='builtin'",
                [],
                |r| r.get(0),
            )?;
            if count >= 200 {
                return Err(err(
                    "input_limit",
                    "Knowledge database supports at most 200 sources",
                ));
            }
            let id = uuid::Uuid::new_v4().to_string();
            s.db.execute(
                "INSERT INTO sources(id,name,root,kind) VALUES(?1,?2,?3,'file')",
                params![id, name, root],
            )?;
            Ok(json!({"source_id":id,"name":name,"root":root,"existing":false}))
        }
        SourceCommand::List => {
            let mut q=s.db.prepare("SELECT id,name,root,kind,last_scan,error FROM sources WHERE kind!='builtin' ORDER BY name LIMIT 201")?;
            let items=q.query_map([],|r|Ok(json!({"source_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"root":r.get::<_,String>(2)?,"kind":r.get::<_,String>(3)?,"last_scan":r.get::<_,Option<String>>(4)?,"error":r.get::<_,Option<String>>(5)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
            if items.len() > 200 {
                return Err(err(
                    "output_limit",
                    "More than 200 sources; use a separate knowledge database",
                ));
            }
            Ok(json!({"items":items}))
        }
        SourceCommand::Remove { source } => {
            let id = resolve_source(&s.db, &source)?;
            let tx = s.db.transaction()?;
            tx.execute("DELETE FROM chunks WHERE revision_id IN(SELECT r.id FROM revisions r JOIN documents d ON d.id=r.document_id WHERE d.source_id=?1)",[&id])?;
            tx.execute("DELETE FROM blocks WHERE revision_id IN(SELECT r.id FROM revisions r JOIN documents d ON d.id=r.document_id WHERE d.source_id=?1)",[&id])?;
            tx.execute("DELETE FROM revisions WHERE document_id IN(SELECT id FROM documents WHERE source_id=?1)",[&id])?;
            tx.execute("DELETE FROM documents WHERE source_id=?1", [&id])?;
            if id != "@derived" {
                tx.execute("DELETE FROM sources WHERE id=?1", [&id])?;
            }
            prune_embeddings(&tx)?;
            tx.execute(
                "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='generation'",
                [],
            )?;
            tx.commit()?;
            Ok(json!({"source_id":id,"removed":true,"original_files_unchanged":true}))
        }
    }
}

pub(super) struct RevisionInput<'a> {
    pub source: &'a str,
    pub key: &'a str,
    pub source_hash: &'a str,
    pub extracted: &'a extract::Extracted,
    pub kind: &'a str,
    pub title: &'a str,
    pub provenance: &'a Value,
}
pub(super) fn prepare(s: &mut Store, input: RevisionInput<'_>) -> Result<(String, String, bool)> {
    let RevisionInput {
        source,
        key,
        source_hash,
        extracted,
        kind,
        title,
        provenance,
    } = input;
    if extracted.text.len() > MAX_INPUT {
        return Err(err("input_limit", "Extracted text exceeds 16 MiB"));
    }
    let document = hash(format!("{source}\0{key}").as_bytes());
    let body_hash = hash(extracted.text.as_bytes());
    let revision = hash(
        format!(
            "{document}\0{source_hash}\0{body_hash}\0{}\0{kind}\0{}",
            extracted.extractor, provenance
        )
        .as_bytes(),
    );
    let exists: bool = s.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM revisions WHERE id=?1)",
        [&revision],
        |r| r.get(0),
    )?;
    if exists {
        return Ok((document, revision, false));
    }
    let tx = s.db.transaction()?;
    tx.execute(
        "INSERT OR IGNORE INTO documents(id,source_id,key)VALUES(?1,?2,?3)",
        params![document, source, key],
    )?;
    tx.execute(
        "INSERT INTO revisions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            revision,
            document,
            source_hash,
            body_hash,
            extracted.extractor,
            kind,
            title,
            provenance.to_string(),
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    let mut offset = 0usize;
    let mut chunk_ordinal = 0i64;
    for (ordinal, block) in extracted.blocks.iter().enumerate() {
        tx.execute(
            "INSERT INTO blocks VALUES(?1,?2,?3,?4)",
            params![
                revision,
                ordinal as i64,
                block.text,
                block.location.to_string()
            ],
        )?;
        let mut local_scalar = 0usize;
        let mut local_byte = 0usize;
        for (i, segment) in crate::index::split_text(&block.text, SPLIT, 0)
            .iter()
            .enumerate()
        {
            let id = hash(format!("{revision}:{ordinal}:{i}").as_bytes());
            let gap = segment.start.saturating_sub(local_scalar);
            let remaining = &block.text[local_byte..];
            local_byte += remaining
                .char_indices()
                .nth(gap)
                .map_or(remaining.len(), |(i, _)| i);
            let start = offset + local_byte;
            let end = start + segment.text.len();
            local_byte += segment.text.len();
            local_scalar = segment.end;
            tx.execute(
                "INSERT INTO chunks VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    id,
                    revision,
                    chunk_ordinal,
                    start as i64,
                    end as i64,
                    segment.text,
                    hash(segment.text.as_bytes()),
                    block.location.to_string()
                ],
            )?;
            chunk_ordinal += 1;
        }
        offset += block.text.len() + 1;
    }
    tx.commit()?;
    Ok((document, revision, true))
}
pub(super) fn publish(s: &mut Store, document: &str, revision: &str) -> Result<()> {
    let tx = s.db.transaction()?;
    tx.execute(
        "UPDATE documents SET active_revision=?2,status='ready',error=NULL WHERE id=?1",
        params![document, revision],
    )?;
    tx.execute(
        "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='generation'",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn collect(path: &Path, root: &Path, files: &mut Vec<(String, PathBuf)>) -> Result<()> {
    if path
        .components()
        .count()
        .saturating_sub(root.components().count())
        > 64
    {
        return Err(err(
            "input_limit",
            "Source directory nesting exceeds 64 levels",
        ));
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| {
        err(
            "source_unavailable",
            "Source traversal failed; deletion detection was skipped",
        )
    })?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        let mut entries = std::fs::read_dir(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            collect(&e.path(), root, files)?;
        }
    } else if meta.is_file() {
        if files.len() >= 10000 {
            return Err(err(
                "input_limit",
                "Source exceeds 10000 files per synchronization",
            ));
        }
        let key = path
            .strip_prefix(root)
            .ok()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| path.file_name().map(Path::new).unwrap_or(path));
        files.push((
            key.to_str()
                .ok_or_else(|| err("invalid_argument", "Source filenames must be Unicode"))?
                .to_owned(),
            path.into(),
        ));
    }
    Ok(())
}
pub(super) async fn index(
    context: &search::Context,
    source: Option<&str>,
    dry_run: bool,
    mode: IndexMode,
    backend: Option<&Path>,
    adapter: Option<&Path>,
) -> Result<Value> {
    if source.is_some_and(|id| matches!(id, "docs" | "@docs")) {
        if backend.is_some() || adapter.is_some() {
            return Err(err(
                "invalid_argument",
                "Bundled guides do not use an external extractor or adapter",
            ));
        }
        return super::builtin::index(context, dry_run, mode).await;
    }
    let mut s = open(&context.database, !dry_run)?;
    let selected = source.map(|id| resolve_source(&s.db, id)).transpose()?;
    let sources = {
        let mut q = s.db.prepare(
            "SELECT id,root FROM sources WHERE kind='file' AND (?1 IS NULL OR id=?1) ORDER BY id",
        )?;
        q.query_map([selected.as_deref()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut counts = SyncCounts::default();
    let mut removed = 0;
    let mut failures = Vec::new();
    let mut failure_count = 0usize;
    let mut breaker = ProviderBreaker::default();
    let mut deferred = 0usize;
    let profile = if !dry_run && mode == IndexMode::Semantic {
        Some(search::embedding_profile(context)?)
    } else {
        None
    };
    let selection = selection_fingerprint(backend, adapter)?;
    let policy = FileIndexPolicy {
        dry_run,
        backend,
        adapter,
        profile,
        selection,
    };
    for (source, root) in sources {
        let mut files = Vec::new();
        if let Err(error) = collect(Path::new(&root), Path::new(&root), &mut files) {
            failure_count += 1;
            if failures.len() < 50 {
                failures
                    .push(json!({"source_id":source,"code":error.code,"message":error.message}));
            }
            if !dry_run {
                s.db.execute(
                    "UPDATE sources SET error=?2 WHERE id=?1",
                    params![source, error.code],
                )?;
                s.db.execute(
                    "UPDATE documents SET status='stale',error=?2 WHERE source_id=?1",
                    params![source, error.code],
                )?;
            }
            continue;
        }
        let seen: HashSet<String> = files.iter().map(|(k, _)| k.clone()).collect();
        let mut source_failed = false;
        for (key, path) in files {
            if breaker.paused() {
                deferred += 1;
                source_failed = true;
                continue;
            }
            let result = policy
                .synchronize(&mut s, &source, &key, &path, &mut counts)
                .await;
            if let Err(error) = result {
                source_failed = true;
                breaker.failure(&error.code);
                failure_count += 1;
                if failures.len() < 50 {
                    failures.push(json!({"source_id":source,"key":key,"code":error.code,"message":error.message,"previous_revision_retained":true}));
                }
                if !dry_run {
                    s.db.execute("INSERT OR IGNORE INTO documents(id,source_id,key,status,error)VALUES(?1,?2,?3,'failed',?4)",params![hash(format!("{source}\0{key}").as_bytes()),source,key,error.code])?;
                    s.db.execute("UPDATE documents SET status='stale',error=?3 WHERE source_id=?1 AND key=?2",params![source,key,error.code])?;
                }
            } else {
                breaker.success();
            }
        }
        // Even complete traversal must not turn a source processing failure into
        // an accidental deletion sweep. Successful document revisions remain.
        removed += collect_missing(&mut s, &source, &seen, dry_run, source_failed)?;
        if !dry_run {
            s.db.execute(
                "UPDATE sources SET last_scan=?2,error=?3 WHERE id=?1",
                params![
                    source,
                    chrono::Utc::now().to_rfc3339(),
                    source_failed.then_some("partial_failure")
                ],
            )?;
        }
    }
    // Ingest documents participate in explicit semantic indexing but never in a
    // file walk; ordinary synchronization cannot overwrite their supplied body.
    if !dry_run
        && let Some(profile) = &policy.profile
        && selected.as_deref().is_none_or(|id| id == "@derived")
    {
        index_derived(
            &mut s,
            profile,
            &mut breaker,
            &mut failures,
            &mut failure_count,
            &mut deferred,
        )
        .await?;
    }
    Ok(
        json!({"dry_run":dry_run,"changed":counts.changed,"unchanged":counts.unchanged,"removed":removed,"processed":counts.processed,"failures":failures,"failure_count":failure_count,"failures_truncated":failure_count>failures.len(),"partial_failure":failure_count>0,"embedding_paused":breaker.paused(),"deferred":deferred,"retry_hint":breaker.paused().then_some("Resolve provider rate/auth/credit issue, then rerun the same index command; successful embeddings are retained")}),
    )
}

pub(super) fn ingest(
    path: &Path,
    id: &str,
    source_file: &Path,
    kind: IngestKind,
    text_file: Option<&Path>,
    stdin: bool,
    metadata_file: Option<&Path>,
) -> Result<Value> {
    if id.is_empty() || id.len() > 512 {
        return Err(err(
            "invalid_argument",
            "Ingest id must contain 1..=512 bytes",
        ));
    }
    let bytes = if stdin {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAX_INPUT as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_INPUT {
            return Err(err("input_limit", "Input exceeds 16 MiB"));
        }
        bytes
    } else {
        bounded_read(
            text_file.ok_or_else(|| err("invalid_argument", "Specify text-file or text-stdin"))?,
        )?
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| err("invalid_argument", "Derived text must be UTF-8"))?;
    let metadata = metadata_file
        .map(|p| {
            bounded_read(p).and_then(|b| {
                if b.len() > 65536 {
                    return Err(err("input_limit", "Metadata exceeds 64 KiB"));
                }
                serde_json::from_slice::<Value>(&b)
                    .map_err(|_| err("invalid_argument", "Invalid metadata JSON"))
            })
        })
        .transpose()?
        .unwrap_or(json!({}));
    if !metadata.is_object() {
        return Err(err("invalid_argument", "Metadata must be a JSON object"));
    }
    let original_hash = hash(&bounded_read(source_file)?);
    let kind = match kind {
        IngestKind::Extracted => "extracted",
        IngestKind::LlmExplanation => "llm-explanation",
        IngestKind::Summary => "summary",
    };
    let provenance = json!({"original_path":source_file.to_string_lossy(),"observed_original_hash":original_hash,"original_hash_claim":metadata.get("source_hash"),"hash_observation":"at_ingest_not_proof_of_generation_input","derived_body_hash":hash(text.as_bytes()),"metadata":metadata});
    let extracted = extract::Extracted {
        text: text.clone(),
        blocks: vec![extract::Block {
            text,
            location: json!({"kind":"document"}),
        }],
        extractor: "external-ingest-v1".into(),
    };
    let mut s = open(path, true)?;
    let (document, revision, created) = prepare(
        &mut s,
        RevisionInput {
            source: "@derived",
            key: id,
            source_hash: &original_hash,
            extracted: &extracted,
            kind,
            title: id,
            provenance: &provenance,
        },
    )?;
    publish(&mut s, &document, &revision)?;
    Ok(
        json!({"document_id":document,"revision":revision,"created":created,"source_id":"@derived","kind":kind}),
    )
}

pub(super) fn status(path: &Path) -> Result<Value> {
    let s = open(path, false)?;
    let documents: i64 = s.db.query_row(
        "SELECT COUNT(*) FROM documents WHERE active_revision IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let chunks: i64 = s.db.query_row(
        "SELECT COUNT(*) FROM chunks c JOIN documents d ON d.active_revision=c.revision_id",
        [],
        |r| r.get(0),
    )?;
    let stale: i64 = s.db.query_row(
        "SELECT COUNT(*) FROM documents WHERE status='stale'",
        [],
        |r| r.get(0),
    )?;
    let embeddings: i64 =
        s.db.query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))?;
    Ok(
        json!({"database_uuid":s.uuid,"documents":documents,"chunks":chunks,"stale":stale,"cached_embeddings":embeddings}),
    )
}
pub(super) fn inspect(path: &Path, id: &str) -> Result<Value> {
    let s = open(path, false)?;
    let source=s.db.query_row("SELECT id,name,root,kind,last_scan,error FROM sources WHERE id=?1 OR name=?1",[id],|r|Ok(json!({"source_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"root":r.get::<_,String>(2)?,"kind":r.get::<_,String>(3)?,"last_scan":r.get::<_,Option<String>>(4)?,"error":r.get::<_,Option<String>>(5)?}))).optional()?;
    if let Some(value) = source {
        return Ok(value);
    }
    s.db.query_row("SELECT d.id,d.source_id,d.key,d.active_revision,d.status,d.error,r.provenance FROM documents d LEFT JOIN revisions r ON r.id=d.active_revision WHERE d.id=?1 OR d.key=?1",[id],|r|Ok(json!({"document_id":r.get::<_,String>(0)?,"source_id":r.get::<_,String>(1)?,"key":r.get::<_,String>(2)?,"revision":r.get::<_,Option<String>>(3)?,"state":r.get::<_,String>(4)?,"error":r.get::<_,Option<String>>(5)?,"provenance":r.get::<_,Option<String>>(6)?.and_then(|s|serde_json::from_str::<Value>(&s).ok())}))).optional()?.ok_or_else(||err("not_found","Source or document does not exist"))
}
