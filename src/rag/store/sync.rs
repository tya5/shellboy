//! One explicit file synchronization policy; no background state or new budget.
use super::*;
#[derive(Default)]
pub(super) struct SyncCounts {
    pub(super) changed: usize,
    pub(super) unchanged: usize,
    pub(super) processed: usize,
    pub(super) text_bytes: usize,
    pub(super) segments: usize,
}
pub(super) struct FileIndexPolicy {
    pub(super) dry_run: bool,
    pub(super) profile: Option<search::Profile>,
}
impl FileIndexPolicy {
    pub(super) async fn synchronize(
        &self,
        store: &mut Store,
        source: &str,
        key: &str,
        path: &Path,
        counts: &mut SyncCounts,
    ) -> Result<()> {
        let bytes = bounded_read(path)?;
        let source_hash = hash(&bytes);
        let current:Option<(String,String,String,String)>=store.db.query_row("SELECT d.id,r.id,r.source_hash,r.extractor FROM documents d JOIN revisions r ON r.id=d.active_revision WHERE d.source_id=?1 AND d.key=?2",params![source,key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((document, revision, current_hash, previous_extractor)) = &current
            && current_hash == &source_hash
            && previous_extractor == "text:utf8-v1"
        {
            counts.unchanged += 1;
            let (segments, bytes): (i64, i64) = store.db.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(text AS BLOB))),0) FROM chunks WHERE revision_id=?1", [revision], |row| Ok((row.get(0)?,row.get(1)?)))?;
            counts.segments += segments as usize;
            counts.text_bytes += bytes as usize;
            if let Some(profile) = &self.profile {
                search::embed_revision(store, revision, profile).await?;
            }
            if !self.dry_run {
                let ready: bool = store.db.query_row(
                    "SELECT status='ready' AND error IS NULL FROM documents WHERE id=?1",
                    [document],
                    |row| row.get(0),
                )?;
                if self.profile.is_some() || !ready {
                    publish(store, document, revision)?;
                }
            }
            return Ok(());
        }
        counts.changed += 1;
        if self.dry_run {
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| err("extractor_failed", "Text input must be UTF-8"))?;
            counts.text_bytes += text.len();
            counts.segments += crate::index::split_text(text, SPLIT, 0).len();
            return Ok(());
        }
        let extracted =
            extract::extract(path).map_err(|message| err("extractor_failed", message))?;
        if hash(bounded_read(path)?) != source_hash {
            return Err(err(
                "source_changed",
                "Source changed during extraction; retry synchronization",
            ));
        }
        counts.text_bytes += extracted.text.len();
        counts.segments += extracted
            .blocks
            .iter()
            .map(|block| crate::index::split_text(&block.text, SPLIT, 0).len())
            .sum::<usize>();
        let provenance = json!({"path":path.to_string_lossy(),"source_hash":source_hash,"content_kind":"extracted"});
        let (document, revision, _) = prepare(
            store,
            RevisionInput {
                source,
                key,
                source_hash: &source_hash,
                extracted: &extracted,
                kind: "extracted",
                title: key,
                provenance: &provenance,
            },
        )?;
        if let Some(profile) = &self.profile {
            search::embed_revision(store, &revision, profile).await?;
        }
        publish(store, &document, &revision)?;
        counts.processed += 1;
        Ok(())
    }
}
pub(super) fn collect_missing(
    store: &mut Store,
    source: &str,
    seen: &HashSet<String>,
    dry_run: bool,
    source_failed: bool,
) -> Result<usize> {
    let mut removed = 0;
    let existing = {
        let mut q = store.db.prepare(
            "SELECT id,key FROM documents WHERE source_id=?1 AND (active_revision IS NOT NULL OR status!='missing')",
        )?;
        q.query_map([&source], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    if !source_failed {
        for (id, key) in existing {
            if !seen.contains(&key) {
                removed += 1;
                if !dry_run {
                    let tx = store.db.transaction()?;
                    tx.execute(
                        "UPDATE documents SET active_revision=NULL,status='missing',error=NULL WHERE id=?1",
                        [id],
                    )?;
                    tx.execute(
                        "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='generation'",
                        [],
                    )?;
                    tx.commit()?;
                }
            }
        }
    }
    Ok(removed)
}
pub(super) async fn index_derived(
    store: &mut Store,
    profile: &search::Profile,
    breaker: &mut ProviderBreaker,
    failures: &mut Vec<Value>,
    failure_count: &mut usize,
    deferred: &mut usize,
) -> Result<()> {
    let revisions = {
        let mut q=store.db.prepare("SELECT id,active_revision FROM documents WHERE source_id='@derived' AND active_revision IS NOT NULL")?;
        q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (document, revision) in revisions {
        if breaker.paused() {
            *deferred += 1;
            continue;
        }
        if let Err(error) = search::embed_revision(store, &revision, profile).await {
            breaker.failure(&error.code);
            *failure_count += 1;
            if failures.len() < 50 {
                failures.push(
                    json!({"document_id":document,"code":error.code,"message":error.message}),
                );
            }
        } else {
            breaker.success();
        }
    }
    Ok(())
}
