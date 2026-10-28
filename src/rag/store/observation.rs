//! Read-only source summaries and bounded saved failure evidence. No HTTP.
use super::*;

pub(super) fn status(context: &search::Context) -> Result<Value> {
    let s = open(&context.database, false)?;
    let profile = search::embedding_profile(context);
    let profile_id = profile.as_ref().ok().map(|profile| profile.id.as_str());
    let semantic_configuration = match &profile {
        Ok(profile) => {
            json!({"state":"configured","connection":profile.name,"model":profile.model})
        }
        Err(error) => json!({"state":"unknown","reason":error.code}),
    };
    let count = |sql: &str| -> Result<i64> { Ok(s.db.query_row(sql, [], |row| row.get(0))?) };
    let documents = count("SELECT COUNT(*) FROM documents WHERE active_revision IS NOT NULL")?;
    let chunks =
        count("SELECT COUNT(*) FROM chunks c JOIN documents d ON d.active_revision=c.revision_id")?;
    let stale = count("SELECT COUNT(*) FROM documents WHERE status='stale'")?;
    let failed = count("SELECT COUNT(*) FROM documents WHERE status='failed'")?;
    let embeddings = count("SELECT COUNT(*) FROM embeddings")?;
    let rows = {
        let mut query = s.db.prepare("SELECT s.id,s.name,s.root,s.kind,s.last_scan,s.error,COUNT(d.id),COALESCE(SUM(d.active_revision IS NOT NULL),0),COALESCE(SUM(d.status='failed'),0),COALESCE(SUM(d.status='stale'),0),MAX(r.created_at) FROM sources s LEFT JOIN documents d ON d.source_id=s.id LEFT JOIN revisions r ON r.id=d.active_revision GROUP BY s.id ORDER BY s.name LIMIT 203")?;
        query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let sources_truncated = rows.len() > 202;
    let mut sources = Vec::new();
    for (
        id,
        name,
        root,
        kind,
        last_scan,
        error,
        documents,
        active,
        failed,
        stale,
        last_changed_at,
    ) in rows.into_iter().take(202)
    {
        let semantic_missing: Option<i64> = profile_id.map(|space| s.db.query_row("SELECT COUNT(*) FROM chunks c JOIN documents d ON d.active_revision=c.revision_id WHERE d.source_id=?1 AND NOT EXISTS(SELECT 1 FROM embeddings e WHERE e.hash=c.hash AND e.space_id=?2)",params![id,space],|row|row.get(0))).transpose()?;
        let failures = failures(&s.db, Some(&id))?;
        let state = if error.is_some() || failed + stale > 0 {
            if active > 0 {
                "partial_failure"
            } else {
                "failed"
            }
        } else if last_scan.is_some() || active > 0 {
            "ready"
        } else {
            "not_indexed"
        };
        sources.push(json!({"source_id":id,"name":name,"root":root,"kind":kind,"state":state,"last_scan":last_scan,"last_changed_at":last_changed_at,"error":error,"documents":documents,"active_documents":active,"failed":failed,"stale":stale,"semantic_missing":semantic_missing,"failures":failures.0,"failure_count":failures.1,"failures_truncated":failures.1>50}));
    }
    let source_failures = sources
        .iter()
        .filter(|source| !source["error"].is_null())
        .count();
    let any_failure = source_failures > 0 || failed + stale > 0;
    let state = if any_failure {
        if documents > 0 {
            "partial_failure"
        } else {
            "failed"
        }
    } else if documents > 0 {
        "ready"
    } else {
        "not_indexed"
    };
    let (failures, failure_count) = failures(&s.db, None)?;
    let last_changed_at: Option<String> = s.db.query_row(
        "SELECT MAX(r.created_at) FROM revisions r JOIN documents d ON d.active_revision=r.id",
        [],
        |row| row.get(0),
    )?;
    Ok(
        json!({"database_uuid":s.uuid,"state":state,"source_failure_count":source_failures,"documents":documents,"chunks":chunks,"stale":stale,"failed":failed,"cached_embeddings":embeddings,"last_changed_at":last_changed_at,"semantic_configuration":semantic_configuration,"sources":sources,"sources_limit":202,"sources_truncated":sources_truncated,"failures":failures,"failure_count":failure_count,"failures_truncated":failure_count>50,"partial_failure":any_failure}),
    )
}
fn failures(db: &Connection, source: Option<&str>) -> Result<(Vec<Value>, i64)> {
    let total:i64 = db.query_row("SELECT COUNT(*) FROM documents WHERE (?1 IS NULL OR source_id=?1) AND status IN ('failed','stale') AND error IS NOT NULL",[source],|row|row.get(0))?;
    let mut query=db.prepare("SELECT source_id,key,status,error,active_revision IS NOT NULL FROM documents WHERE (?1 IS NULL OR source_id=?1) AND status IN ('failed','stale') AND error IS NOT NULL ORDER BY source_id,key LIMIT 50")?;
    let rows=query.query_map([source],|row| {
        let saved: String = row.get(3)?;
        let detail = serde_json::from_str::<Value>(&saved).unwrap_or(Value::Null);
        Ok(json!({"source_id":row.get::<_,String>(0)?,"key":row.get::<_,String>(1)?,"state":row.get::<_,String>(2)?,"code":detail.get("code").and_then(Value::as_str).unwrap_or(&saved),"message":detail.get("message").and_then(Value::as_str).unwrap_or(&saved),"previous_revision_retained":row.get::<_,bool>(4)?}))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total))
}
