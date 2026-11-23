//! Metadata-only Thread artifacts. All mutations borrow the existing owner transaction.
use crate::protocol::{
    ARTIFACT_PAGE_LIMIT, ApiError, ArtifactAddArgs, ArtifactGetArgs, ArtifactGetResult,
    ArtifactHistory, ArtifactListArgs, ArtifactSelected, ArtifactSummary, ArtifactUpdateArgs,
    ArtifactVersion, ArtifactVersionStamp, ErrorCode, Page, PageCursor,
};
use rusqlite::{OptionalExtension, Row, Transaction, params};

type Result<T> = std::result::Result<T, ApiError>;
const LIST: &str = "artifact.list";
const VERSIONS: &str = "artifact.versions";

fn sql(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("artifact database error: {error}"))
}
fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}
fn missing() -> ApiError {
    ApiError::new(
        ErrorCode::TargetUnavailable,
        "Artifact or version not found",
    )
}
fn check_thread(tx: &Transaction<'_>, thread: &str) -> Result<()> {
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM thread WHERE id=? AND deleted_at IS NULL)",
            [thread],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if !exists {
        return Err(ApiError::new(ErrorCode::ThreadNotFound, "Thread not found"));
    }
    Ok(())
}
fn check_fields(uri: &str, title: &str, mime: Option<&str>) -> Result<()> {
    for (field, value, min, max) in [
        ("uri", uri, 1, 2048),
        ("title", title, 1, 256),
        ("mime", mime.unwrap_or_default(), 0, 128),
    ] {
        if !(min..=max).contains(&value.len()) {
            return Err(invalid(&format!(
                "artifact {field} must contain {min}-{max} UTF-8 bytes"
            )));
        }
    }
    Ok(())
}
fn version(r: &Row<'_>, i: usize) -> rusqlite::Result<ArtifactVersion> {
    Ok(ArtifactVersion {
        version: r.get(i)?,
        uri: r.get(i + 1)?,
        title: r.get(i + 2)?,
        mime: r.get(i + 3)?,
        created_at: r.get(i + 4)?,
    })
}
fn summary(r: &Row<'_>) -> rusqlite::Result<ArtifactSummary> {
    Ok(ArtifactSummary {
        artifact_id: r.get(0)?,
        thread_id: r.get(1)?,
        created_at: r.get(2)?,
        updated_at: r.get(3)?,
        latest: version(r, 4)?,
    })
}
fn selected(item: ArtifactSummary) -> ArtifactSelected {
    ArtifactSelected {
        artifact_id: item.artifact_id,
        thread_id: item.thread_id,
        created_at: item.created_at,
        updated_at: item.updated_at,
        version: item.latest,
    }
}
const LATEST_JOIN: &str = "JOIN artifact_version v ON v.artifact_id=a.id AND v.version=(SELECT max(version) FROM artifact_version WHERE artifact_id=a.id)";
const FIELDS: &str =
    "a.id,a.thread_id,a.created_at,a.updated_at,v.version,v.uri,v.title,v.mime,v.created_at";
fn find(tx: &Transaction<'_>, id: &str, requested: Option<i64>) -> Result<ArtifactSummary> {
    let exact = "JOIN artifact_version v ON v.artifact_id=a.id AND v.version=?2";
    let join = if requested.is_some() {
        exact
    } else {
        LATEST_JOIN
    };
    let query = format!(
        "SELECT {FIELDS} FROM artifact a {join} JOIN thread t ON t.id=a.thread_id AND t.deleted_at IS NULL WHERE a.id=?1"
    );
    let mut stmt = tx.prepare(&query).map_err(sql)?;
    let row = match requested {
        Some(n) => stmt.query_row(params![id, n], summary),
        None => stmt.query_row([id], summary),
    };
    row.optional().map_err(sql)?.ok_or_else(missing)
}
fn append(tx: &Transaction<'_>, id: &str, version: &ArtifactVersion) -> Result<()> {
    tx.execute("INSERT INTO artifact_version(artifact_id,version,uri,title,mime,created_at) VALUES(?,?,?,?,?,?)", params![id,version.version,version.uri,version.title,version.mime,version.created_at]).map_err(sql)?;
    Ok(())
}
pub(crate) fn add(
    tx: &Transaction<'_>,
    a: &ArtifactAddArgs,
    now: &str,
) -> Result<ArtifactSelected> {
    check_fields(&a.uri, &a.title, a.mime.as_deref())?;
    check_thread(tx, &a.thread_id)?;
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO artifact(id,thread_id,created_at,updated_at) VALUES(?1,?2,?3,?3)",
        params![id, a.thread_id, now],
    )
    .map_err(sql)?;
    let version = ArtifactVersion {
        version: 1,
        uri: a.uri.clone(),
        title: a.title.clone(),
        mime: a.mime.clone(),
        created_at: now.into(),
    };
    append(tx, &id, &version)?;
    Ok(ArtifactSelected {
        artifact_id: id,
        thread_id: a.thread_id.clone(),
        created_at: now.into(),
        updated_at: now.into(),
        version,
    })
}
pub(crate) fn update(
    tx: &Transaction<'_>,
    a: &ArtifactUpdateArgs,
    now: &str,
) -> Result<ArtifactSelected> {
    let mut item = find(tx, &a.artifact_id, None)?;
    let title = a.title.as_deref().unwrap_or(&item.latest.title);
    check_fields(&a.uri, title, a.mime.as_deref())?;
    let number = item
        .latest
        .version
        .checked_add(1)
        .ok_or_else(|| invalid("artifact version number exceeds storage range"))?;
    item.latest = ArtifactVersion {
        version: number,
        uri: a.uri.clone(),
        title: title.into(),
        mime: a.mime.clone(),
        created_at: now.into(),
    };
    append(tx, &a.artifact_id, &item.latest)?;
    tx.execute(
        "UPDATE artifact SET updated_at=?2 WHERE id=?1",
        params![a.artifact_id, now],
    )
    .map_err(sql)?;
    item.updated_at = now.into();
    Ok(selected(item))
}
pub(crate) fn remove(tx: &Transaction<'_>, id: &str) -> Result<String> {
    tx.query_row("DELETE FROM artifact WHERE id=?1 AND thread_id IN (SELECT id FROM thread WHERE id=artifact.thread_id AND deleted_at IS NULL) RETURNING thread_id",[id],|r|r.get(0)).optional().map_err(sql)?.ok_or_else(missing)
}
fn page_limit(limit: u32) -> Result<usize> {
    if !(1..=ARTIFACT_PAGE_LIMIT).contains(&limit) {
        return Err(invalid("artifact page limit must be 1-32"));
    }
    Ok(limit as usize)
}
fn seek(token: Option<&str>, scope: &str, target: &str) -> Result<Option<String>> {
    token
        .map(|s| PageCursor::decode(s, scope, target, 0, None).map(|c| c.last_sort_key))
        .transpose()
}
fn page<T>(
    mut items: Vec<T>,
    limit: usize,
    scope: &str,
    target: &str,
    key: impl Fn(&T) -> String,
) -> Result<Page<T>> {
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        Some(
            PageCursor {
                version: 1,
                scope: scope.into(),
                filter: target.into(),
                last_sort_key: key(&items[limit - 1]),
                watermark: 0,
                revision: 0,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}
pub(crate) fn list(tx: &Transaction<'_>, a: &ArtifactListArgs) -> Result<Page<ArtifactSummary>> {
    let limit = page_limit(a.limit)?;
    let after = seek(a.cursor.as_deref(), LIST, &a.thread_id)?.unwrap_or_default();
    check_thread(tx, &a.thread_id)?;
    let query = format!(
        "SELECT {FIELDS} FROM artifact a {LATEST_JOIN} WHERE a.thread_id=?1 AND a.id>?2 ORDER BY a.id LIMIT ?3"
    );
    let items = tx
        .prepare(&query)
        .map_err(sql)?
        .query_map(params![a.thread_id, after, limit as i64 + 1], summary)
        .map_err(sql)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql)?;
    page(items, limit, LIST, &a.thread_id, |a| a.artifact_id.clone())
}
pub(crate) fn get(tx: &Transaction<'_>, a: &ArtifactGetArgs) -> Result<ArtifactGetResult> {
    if let Some(n) = a.version {
        if n < 1 || a.cursor.is_some() || a.limit.is_some() {
            return Err(invalid(
                "artifact version must be positive and cannot be combined with cursor or limit",
            ));
        }
        return Ok(ArtifactGetResult::Selected(selected(find(
            tx,
            &a.artifact_id,
            Some(n),
        )?)));
    }
    let limit = page_limit(a.limit.unwrap_or(ARTIFACT_PAGE_LIMIT))?;
    let artifact = find(tx, &a.artifact_id, None)?;
    let before = seek(a.cursor.as_deref(), VERSIONS, &a.artifact_id)?
        .map(|key| {
            key.parse::<i64>().ok().filter(|n| *n > 0).ok_or_else(|| {
                ApiError::new(ErrorCode::CursorExpired, "invalid artifact version cursor")
            })
        })
        .transpose()?;
    let mut stmt = tx.prepare(if before.is_some() {"SELECT version,created_at FROM artifact_version WHERE artifact_id=?1 AND version<?2 ORDER BY version DESC LIMIT ?3"} else {"SELECT version,created_at FROM artifact_version WHERE artifact_id=?1 ORDER BY version DESC LIMIT ?3"}).map_err(sql)?;
    let items = stmt
        .query_map(params![a.artifact_id, before, limit as i64 + 1], |r| {
            Ok(ArtifactVersionStamp {
                version: r.get(0)?,
                created_at: r.get(1)?,
            })
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql)?;
    let versions = page(items, limit, VERSIONS, &a.artifact_id, |v| {
        v.version.to_string()
    })?;
    Ok(ArtifactGetResult::History(ArtifactHistory {
        artifact,
        versions,
    }))
}
pub(crate) fn copy_thread(
    tx: &Transaction<'_>,
    source: &str,
    target: &str,
) -> rusqlite::Result<()> {
    let mut stmt =
        tx.prepare("SELECT id,created_at,updated_at FROM artifact WHERE thread_id=?1 ORDER BY id")?;
    let mut rows = stmt.query([source])?;
    while let Some(row) = rows.next()? {
        let old: String = row.get(0)?;
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO artifact(id,thread_id,created_at,updated_at) VALUES(?,?,?,?)",
            params![
                id,
                target,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?
            ],
        )?;
        tx.execute("INSERT INTO artifact_version(artifact_id,version,uri,title,mime,created_at) SELECT ?1,version,uri,title,mime,created_at FROM artifact_version WHERE artifact_id=?2",params![id,old])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
