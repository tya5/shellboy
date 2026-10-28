//! Explicit bundled guide indexing and read-only semantic queries.
use super::search::{Context, Hit, embed_revision, embedding_profile, material_semantic};
use super::*;
use rusqlite::OptionalExtension;
use std::collections::BTreeMap;

pub(super) async fn index(context: &Context, dry_run: bool, mode: IndexMode) -> Result<Value> {
    let mut s = store::open(&context.database, !dry_run)?;
    let docs = crate::docs::documents();
    let bytes: usize = docs.iter().map(|d| d.text.len()).sum();
    let segments: usize = docs
        .iter()
        .map(|d| crate::index::split_text(&d.text, SPLIT, 0).len())
        .sum();
    let mut result = json!({"source":"docs","dry_run":dry_run,"documents":docs.len(),"text_bytes":bytes,"segments":segments,"embedding_may_charge":mode==IndexMode::Semantic,"billing":"Explicit semantic indexing may call the configured embedding API; cached successful content is reused. Unit price is not known.","partial_failure":false});
    if dry_run {
        return Ok(result);
    }
    let profile = if mode == IndexMode::Semantic {
        Some(embedding_profile(context)?)
    } else {
        None
    };
    let mut map = BTreeMap::new();
    let mut changed = 0;
    for doc in docs {
        let body_hash = hash(doc.text.as_bytes());
        let extracted = extract::Extracted {
            text: doc.text.clone(),
            blocks: vec![extract::Block {
                text: doc.text.clone(),
                location: json!({"document_id":doc.id}),
            }],
            extractor: "bundled-guide-v1".into(),
        };
        let provenance = json!({"id":doc.id,"parent_id":doc.parent_id});
        let (document, revision, created) = store::prepare(
            &mut s,
            store::RevisionInput {
                source: "@docs",
                key: &doc.id,
                source_hash: &body_hash,
                extracted: &extracted,
                kind: "guide",
                title: &doc.title,
                provenance: &provenance,
            },
        )?;
        if let Some(profile) = &profile {
            embed_revision(&mut s, &revision, profile).await?;
        }
        store::publish(&mut s, &document, &revision)?;
        changed += usize::from(created);
        map.insert(revision, (doc.id, body_hash));
    }
    let obsolete = {
        let mut q=s.db.prepare("SELECT id,active_revision FROM documents WHERE source_id='@docs' AND active_revision IS NOT NULL")?;
        q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut removed = 0;
    for (document, revision) in obsolete {
        if !map.contains_key(&revision) {
            let tx = s.db.unchecked_transaction()?;
            tx.execute(
                "UPDATE documents SET active_revision=NULL WHERE id=?1",
                [document],
            )?;
            tx.execute(
                "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='generation'",
                [],
            )?;
            tx.commit()?;
            removed += 1;
        }
    }
    result["changed"] = json!(changed);
    result["removed"] = json!(removed);
    Ok(result)
}

/// Diagnose only missing current catalogue/coverage, without publishing or embedding.
pub(super) fn preparation(context: &Context) -> Result<Option<search::Preparation>> {
    let profile = embedding_profile(context)?;
    let docs = crate::docs::documents();
    let s = match store::open(&context.database, false) {
        Ok(s) => s,
        Err(error) if error.code == "db_not_found" => {
            return Ok(Some(search::Preparation {
                reason: "store_not_initialized",
                missing_documents: Some(docs.len() as i64),
                missing_chunks: None,
            }));
        }
        Err(error) => return Err(error),
    };
    let before = s.db.unchecked_transaction()?;
    let mut missing = 0i64;
    for doc in &docs {
        let present: bool = before.query_row(
            "SELECT EXISTS(SELECT 1 FROM documents d JOIN revisions r ON r.id=d.active_revision WHERE d.source_id='@docs' AND d.key=?1 AND r.body_hash=?2 AND r.extractor='bundled-guide-v1')",
            rusqlite::params![doc.id, hash(doc.text.as_bytes())], |r| r.get(0))?;
        missing += i64::from(!present);
    }
    let active: i64 = before.query_row(
        "SELECT COUNT(*) FROM documents WHERE source_id='@docs' AND active_revision IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    before.commit()?;
    if missing > 0 || active != docs.len() as i64 {
        return Ok(Some(search::Preparation {
            reason: "guide_catalogue_not_indexed",
            missing_documents: Some(missing),
            missing_chunks: None,
        }));
    }
    search::material_preparation(&s, "@docs", &profile)
}

pub(super) async fn semantic(context: &Context, query: &str) -> Result<Vec<Hit>> {
    let profile = embedding_profile(context)?;
    let s = store::open(&context.database, false).map_err(|error| {
        if error.code == "db_not_found" {
            err("rag_semantic_store_missing", error.message)
        } else {
            error
        }
    })?;
    let before = s.db.unchecked_transaction()?;
    let generation: String =
        before.query_row("SELECT value FROM meta WHERE key='generation'", [], |r| {
            r.get(0)
        })?;
    let mut map = BTreeMap::new();
    for doc in crate::docs::documents() {
        let body_hash = hash(doc.text.as_bytes());
        let revision:Option<String>=before.query_row("SELECT r.id FROM documents d JOIN revisions r ON r.id=d.active_revision WHERE d.source_id='@docs' AND d.key=?1 AND r.body_hash=?2 AND r.extractor='bundled-guide-v1'",rusqlite::params![doc.id,body_hash],|r|r.get(0)).optional()?;
        let Some(revision) = revision else {
            return Err(err(
                "rag_semantic_guides_missing",
                "Bundled guides are not indexed for this binary; run shellboy rag init, then shellboy rag index --source docs --dry-run --mode semantic and shellboy rag index --source docs --mode semantic",
            ));
        };
        map.insert(revision, (doc.id, body_hash));
    }
    let active: i64 = before.query_row(
        "SELECT COUNT(*) FROM documents WHERE source_id='@docs' AND active_revision IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if active as usize != map.len() {
        return Err(err(
            "rag_semantic_guides_missing",
            "Bundled guide catalogue changed; explicitly run shellboy rag index --source docs --mode semantic",
        ));
    }
    before.commit()?;
    let mut hits = material_semantic(&s, "@docs", query, &profile).await.map_err(|error| {
        if matches!(error.code.as_str(), "rag_semantic_profile_missing" | "rag_semantic_coverage_missing") {
            err(&error.code, "Bundled guides lack embeddings for this profile; run shellboy rag index --source docs --dry-run --mode semantic, then shellboy rag index --source docs --mode semantic")
        } else { error }
    })?;
    let current: String =
        s.db.query_row("SELECT value FROM meta WHERE key='generation'", [], |r| {
            r.get(0)
        })?;
    if generation != current {
        return Err(err(
            "source_changed",
            "Guide publication changed during search; retry search",
        ));
    }
    for hit in &mut hits {
        if let Some((id, body_hash)) = map.get(&hit.reference.revision) {
            hit.reference = Reference {
                version: 1,
                store: "builtin-docs".into(),
                kind: "docs".into(),
                id: id.clone(),
                revision: body_hash.clone(),
                hash: body_hash.clone(),
                witness: None,
            };
            hit.scopes = vec!["docs".into()];
        }
    }
    Ok(hits)
}
