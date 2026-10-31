use super::*;
use crate::protocol::PageCursor;
use sha2::{Digest, Sha256};
pub(super) const SCAN_LIMIT: usize = 2000;
fn command_sql() -> &'static str {
    "COALESCE(json_extract(s.shell_json,'$.command'),json_extract(s.shell_json,'$.request.command'))"
}
pub(super) fn query(thread: bool) -> String {
    let sec = time::seconds("s.started_at");
    let nano = time::nanos("s.started_at");
    let command = command_sql();
    let index = if thread {
        "shell_owner_started_readback"
    } else {
        "shell_started_readback"
    };
    let scope = if thread {
        "s.owner_run_id IN (SELECT id FROM run INDEXED BY run_thread_created WHERE thread_id=?1)"
    } else {
        "?1 IS NULL"
    };
    format!(
        "WITH candidates AS MATERIALIZED (SELECT s.id,{sec} sec,{nano} nano FROM shell_execution s INDEXED BY {index} WHERE {scope} AND ({sec})>=?2 AND ({sec})<=min(?4,?6) AND ({sec},{nano})>=(?2,?3) AND ({sec},{nano})<(?4,?5) AND ({sec},{nano},s.id)<(?6,?7,?8) AND s.rowid<=?9 AND (?14=0 OR ({sec})>-9223372036854775808) ORDER BY {sec} DESC,{nano} DESC,s.id DESC LIMIT {}), window AS MATERIALIZED (SELECT * FROM candidates ORDER BY sec DESC,nano DESC,id DESC LIMIT {SCAN_LIMIT}), matched AS MATERIALIZED (SELECT w.* FROM window w JOIN shell_execution s ON s.id=w.id WHERE (?10 IS NULL OR s.state=?10) AND (?11 IS NULL OR s.exit_code=?11) AND (?12 IS NULL OR instr({command},?12)>0) ORDER BY w.sec DESC,w.nano DESC,w.id DESC LIMIT ?13+1) SELECT json_object('scanned',(SELECT count(*) FROM window),'candidate_more',(SELECT count(*) FROM candidates)>{SCAN_LIMIT},'tail',(SELECT json_array(sec,nano,id) FROM window ORDER BY sec,nano,id LIMIT 1),'items',json((SELECT json_group_array(json_object('execution_id',s.id,'run_id',s.owner_run_id,'thread_id',r.thread_id,'turn_id',s.turn_id,'started_at',s.started_at,'ended_at',s.ended_at,'state',s.state,'exit_code',s.exit_code,'sec',m.sec,'nano',m.nano,'raw_command',readback_prefix({command}),'command_characters',readback_chars({command})) ORDER BY m.sec DESC,m.nano DESC,m.id DESC) FROM matched m JOIN shell_execution s ON s.id=m.id JOIN run r ON r.id=s.owner_run_id)))",
        SCAN_LIMIT + 1
    )
}
pub(crate) fn list(db: &Connection, a: &ShellListArgs, instance: &str) -> Result<Value, ApiError> {
    a.validate()?;
    super::register(db)?;
    let window = Window::new(a.since.as_deref(), a.until.as_deref())?;
    let digest = Sha256::digest(
        serde_json::to_vec(&(
            instance,
            a.thread_id.as_deref(),
            window.json(),
            a.command_contains.as_deref(),
            a.state.as_deref(),
            a.exit_code,
        ))
        .unwrap(),
    );
    let filter = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let cursor = a
        .cursor
        .as_ref()
        .map(|c| PageCursor::decode(c, "shell.list", &filter, 0, None))
        .transpose()?;
    let watermark = match &cursor {
        Some(c) => i64::try_from(c.watermark).map_err(|_| invalid("invalid cursor watermark"))?,
        None => db
            .query_row(
                "SELECT COALESCE(max(rowid),0) FROM shell_execution",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(error)?,
    };
    let (cs, cn, ci): (i64, u32, String) = cursor
        .as_ref()
        .map(|c| {
            serde_json::from_str(&c.last_sort_key)
                .map_err(|_| ApiError::new(ErrorCode::CursorExpired, "malformed shell cursor"))
        })
        .transpose()?
        .unwrap_or((i64::MAX, u32::MAX, String::new()));
    let (ls, ln) = window.lower();
    let (us, un) = window.upper();
    let raw: String = db
        .query_row(
            &query(a.thread_id.is_some()),
            params![
                a.thread_id,
                ls,
                ln,
                us,
                un,
                cs,
                cn,
                ci,
                watermark,
                a.state,
                a.exit_code,
                a.command_contains,
                a.limit,
                window.active()
            ],
            |r| r.get(0),
        )
        .map_err(error)?;
    let mut page: Value =
        serde_json::from_str(&raw).map_err(|_| invalid("invalid saved shell page"))?;
    let all = page["items"]
        .as_array_mut()
        .ok_or_else(|| invalid("invalid saved shell rows"))?;
    let more_matches = all.len() > a.limit as usize;
    all.truncate(a.limit as usize);
    let last = all
        .last()
        .map(|v| json!([v["sec"], v["nano"], v["execution_id"]]));
    let mut items = Vec::with_capacity(all.len());
    for mut value in std::mem::take(all) {
        let raw = value["raw_command"].as_str();
        let (excerpt, redacted) = raw
            .map(snippet)
            .map(|(s, r)| (Some(s), r))
            .unwrap_or((None, false));
        let truncated = value["command_characters"]
            .as_u64()
            .is_some_and(|n| n > 512);
        let valid = value["sec"].as_i64().is_some_and(|s| s != i64::MIN);
        let obj = value.as_object_mut().unwrap();
        obj.remove("raw_command");
        obj.remove("sec");
        obj.remove("nano");
        obj.insert("command_excerpt".into(), json!(excerpt));
        obj.insert("command_excerpt_redacted".into(), json!(redacted));
        obj.insert("command_excerpt_truncated".into(), json!(truncated));
        obj.insert("started_at_known".into(), json!(valid));
        items.push(value);
    }
    let candidate_more = page["candidate_more"].as_i64() == Some(1);
    let next = if more_matches {
        last
    } else if candidate_more {
        Some(page["tail"].clone())
    } else {
        None
    };
    let next_cursor = next
        .map(|k| {
            PageCursor {
                version: 1,
                scope: "shell.list".into(),
                filter,
                last_sort_key: k.to_string(),
                watermark: watermark.max(0) as u64,
                revision: 0,
                encoding: None,
            }
            .encode()
        })
        .transpose()?;
    Ok(
        json!({"items":items,"next_cursor":next_cursor,"scanned":page["scanned"],"scan_limit":SCAN_LIMIT,"scan_budget_reached":candidate_more&&!more_matches,"period":window.json(),"ordering":"saved start instant descending, then execution ID; unknown starts last","redaction":"conservative obvious credential shapes only; not a security boundary"}),
    )
}
pub(super) fn snippet(raw: &str) -> (String, bool) {
    let cleaned:String=raw.chars().filter(|c|!c.is_control() && !matches!(c,'\u{200b}'..='\u{200f}'|'\u{2028}'..='\u{202e}'|'\u{2066}'..='\u{2069}'|'\u{feff}')).collect();
    let lower = cleaned.to_ascii_lowercase();
    let compact: String = lower
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '\'' | '"' | '\\'))
        .collect();
    let obvious = [
        "authorization:",
        "authorization=",
        "bearer ",
        "password=",
        "password:",
        "passwd=",
        "api_key=",
        "api_key:",
        "apikey=",
        "api-key=",
        "api-key:",
        "token=",
        "token:",
        "secret=",
        "secret:",
        "secret_access_key=",
        "access_key_id=",
        "private_key:",
        "convertto-securestring",
        "--password",
        "--api-key",
    ]
    .iter()
    .any(|p| lower.contains(p) || compact.contains(p));
    let tokens = lower
        .split_whitespace()
        .map(|t| t.trim_matches(['\'', '"', '(', ')', ',', ';']));
    let shaped = tokens.clone().any(|t| {
        [
            "--token",
            "--access-token",
            "--auth-token",
            "--client-secret",
            "--api_key",
            "--passwd",
        ]
        .contains(&t)
            || t == "-u"
            || t == "--user"
            || t.starts_with("--user=")
            || (t.starts_with("-u") && t.contains(':'))
            || ["sk-", "ghp_", "github_pat_", "akia", "aiza"]
                .iter()
                .any(|p| t.starts_with(p) && t.len() >= 12)
            || (t.starts_with("eyj") && t.matches('.').count() >= 2 && t.len() > 24)
            || (t.contains("://")
                && t.split("://")
                    .nth(1)
                    .is_some_and(|s| s.split('/').next().is_some_and(|s| s.contains('@'))))
    });
    if obvious || shaped {
        ("[redacted: credential-like command]".into(), true)
    } else {
        (cleaned.chars().take(512).collect(), false)
    }
}
