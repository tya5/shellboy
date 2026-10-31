use crate::protocol::{ApiError, MessageSender};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, ApiError>;
const ANSWER: &str = "SELECT CASE WHEN octet_length(u.id)<=?2 THEN u.id END,CASE WHEN octet_length(u.answer_node_id)<=?2 THEN u.answer_node_id END,CASE WHEN octet_length(n.created_at)<=?2 THEN n.created_at END,n.payload_json IS NOT NULL,octet_length(n.payload_json),COALESCE(octet_length(n.created_at)>?2,0) FROM turn u INDEXED BY thread_answer_latest LEFT JOIN node n ON n.id=u.answer_node_id WHERE u.thread_id=?1 AND u.answer_node_id IS NOT NULL ORDER BY u.rowid DESC LIMIT 1";
const INPUT: &str = "SELECT CASE WHEN octet_length(r.message_node_id)<=?2 THEN r.message_node_id END,CASE WHEN octet_length(r.created_at)<=?2 THEN r.created_at END,CASE WHEN octet_length(r.sender_json)<=?3 THEN r.sender_json END,CASE WHEN octet_length(r.state)<=?2 THEN r.state END,n.payload_json IS NOT NULL,octet_length(n.payload_json),octet_length(r.sender_json) FROM receipt r INDEXED BY receipt_thread_order LEFT JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=?1 ORDER BY r.rowid DESC LIMIT 1";
// Only natural message content; no function arguments, reasoning, or opaque data.
const EXCERPT: &str = r"WITH target(payload) AS MATERIALIZED (
 SELECT CASE WHEN json_valid(payload_json) THEN payload_json ELSE '{}' END FROM node WHERE id=?1
)
SELECT substr(json_extract(payload,'$.content'),1,513) FROM target WHERE json_type(payload,'$.content')='text'
UNION ALL
SELECT substr(json_extract(CASE WHEN c.type='object' THEN c.value ELSE '{}' END,'$.text'),1,513) FROM target,json_each(payload,'$.content') c
 WHERE json_type(payload,'$.content')='array' AND c.type='object' AND json_extract(CASE WHEN c.type='object' THEN c.value ELSE '{}' END,'$.type') IN ('input_text','output_text','text')
UNION ALL
SELECT substr(COALESCE(json_extract(CASE WHEN c.type='object' THEN c.value ELSE '{}' END,'$.text'),json_extract(CASE WHEN c.type='object' THEN c.value ELSE '{}' END,'$.refusal')),1,513)
 FROM target,json_each(payload,'$.output') o,json_each(CASE WHEN o.type='object' THEN o.value ELSE '{}' END,'$.content') c
 WHERE o.type='object' AND json_extract(CASE WHEN o.type='object' THEN o.value ELSE '{}' END,'$.type')='message' AND c.type='object'
 AND json_extract(CASE WHEN c.type='object' THEN c.value ELSE '{}' END,'$.type') IN ('output_text','refusal') LIMIT 64";

fn error(e: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("Latest Thread message observation failed: {e}"))
}
// Canonical wrappers use unlimited source/scalar bounds. Display callers share
// one finite allowance for both payload walks and the copied sender JSON.
struct Budget {
    scalar: i64,
    remaining: i64,
}
impl Budget {
    fn canonical() -> Self {
        Self {
            scalar: i64::MAX,
            remaining: i64::MAX,
        }
    }
    fn reserve(&mut self, bytes: Option<i64>) -> bool {
        let bytes = bytes.unwrap_or(0);
        if bytes < 0 || bytes > self.remaining {
            return false;
        }
        self.remaining -= bytes;
        true
    }
}
fn limited() -> ApiError {
    ApiError::new(
        crate::protocol::ErrorCode::InputLimit,
        "Latest Thread message scalar exceeds its512-byte display limit",
    )
}
fn required(value: Option<String>) -> Result<String> {
    value.ok_or_else(limited)
}
struct RawMessage {
    node: String,
    available: bool,
    excerpt: Option<Vec<String>>,
    metadata: Metadata,
}
enum Metadata {
    Answer {
        turn: String,
        at: Option<String>,
    },
    Input {
        at: String,
        state: String,
        sender: Option<String>,
    },
}
fn excerpt_parts(db: &Connection, node: &str) -> Result<Vec<String>> {
    let mut stmt = db.prepare(EXCERPT).map_err(error)?;
    let mut rows = stmt.query([node]).map_err(error)?;
    let mut parts = Vec::new();
    let mut remaining = 512usize;
    while remaining > 0 {
        let Some(row) = rows.next().map_err(error)? else {
            break;
        };
        let Some(part) = row.get::<_, Option<String>>(0).map_err(error)? else {
            continue;
        };
        if part.is_empty() {
            continue;
        }
        remaining = remaining.saturating_sub(usize::from(!parts.is_empty()));
        remaining -= part.chars().take(remaining).count();
        parts.push(part);
    }
    Ok(parts)
}
fn prepare_excerpt(parts: Vec<String>) -> String {
    let text = parts.join("\n").chars().take(512).collect::<String>();
    crate::presentation::sanitize_plain(&text, 0).0
}
#[cfg(test)]
fn excerpt(db: &Connection, node: &str) -> Result<String> {
    excerpt_parts(db, node).map(prepare_excerpt)
}
fn read_answer(db: &Connection, thread: &str, budget: &mut Budget) -> Result<Option<RawMessage>> {
    let row = db
        .query_row(ANSWER, rusqlite::params![thread, budget.scalar], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })
        .optional()
        .map_err(error)?;
    let Some((turn, node, at, available, bytes, at_limited)) = row else {
        return Ok(None);
    };
    if at_limited {
        return Err(limited());
    }
    let turn = required(turn)?;
    let node = required(node)?;
    let excerpt = if !available {
        Some(Vec::new())
    } else if budget.reserve(bytes) {
        Some(excerpt_parts(db, &node)?)
    } else {
        None
    };
    Ok(Some(RawMessage {
        node,
        available,
        excerpt,
        metadata: Metadata::Answer { turn, at },
    }))
}
fn read_input(db: &Connection, thread: &str, budget: &mut Budget) -> Result<Option<RawMessage>> {
    let row = db
        .query_row(
            INPUT,
            rusqlite::params![thread, budget.scalar, budget.remaining],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(error)?;
    let Some((node, at, sender, state, available, bytes, sender_bytes)) = row else {
        return Ok(None);
    };
    let at = required(at)?;
    let state = required(state)?;
    if sender_bytes.is_none() {
        return Err(crate::storage::save_error(
            "Latest Thread sender is missing",
        ));
    }
    let sender = if budget.reserve(sender_bytes) {
        sender
    } else {
        None
    };
    let node = required(node)?;
    let excerpt = if !available {
        Some(Vec::new())
    } else if budget.reserve(bytes) {
        Some(excerpt_parts(db, &node)?)
    } else {
        None
    };
    Ok(Some(RawMessage {
        node,
        available,
        excerpt,
        metadata: Metadata::Input { at, state, sender },
    }))
}
fn prepare_message(raw: RawMessage, prefix: &str, limited_fields: &mut Vec<String>) -> Value {
    let excerpt = match raw.excerpt {
        Some(parts) => Value::String(prepare_excerpt(parts)),
        None => {
            limited_fields.push(format!("{prefix}.excerpt"));
            Value::Null
        }
    };
    match raw.metadata {
        Metadata::Answer { turn, at } => {
            json!({"turn_id":turn,"node_id":raw.node,"at":at,"excerpt":excerpt,"body_available":raw.available})
        }
        Metadata::Input { at, state, sender } => {
            let sender = match sender {
                Some(body) => json!(MessageSender::from_saved(Some(&body))),
                None => {
                    limited_fields.push(format!("{prefix}.sender"));
                    Value::Null
                }
            };
            json!({"message_id":raw.node,"node_id":raw.node,"at":at,"state":state,"sender":sender,"excerpt":excerpt,"body_available":raw.available})
        }
    }
}
pub(crate) fn last_answer(db: &Connection, thread: &str) -> Result<Option<Value>> {
    Ok(read_answer(db, thread, &mut Budget::canonical())?
        .map(|raw| prepare_message(raw, "last_answer", &mut Vec::new())))
}
pub(crate) fn last_input(db: &Connection, thread: &str) -> Result<Option<Value>> {
    Ok(read_input(db, thread, &mut Budget::canonical())?
        .map(|raw| prepare_message(raw, "last_input", &mut Vec::new())))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest_query_plans_seek_one_thread_without_sorting_history() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        for (query, index) in [
            (ANSWER, "thread_answer_latest"),
            (INPUT, "receipt_thread_order"),
        ] {
            let plan = db
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap()
                .query_map(
                    rusqlite::params_from_iter(
                        std::iter::once(rusqlite::types::Value::Text("t".into())).chain(
                            std::iter::repeat_n(
                                rusqlite::types::Value::Integer(i64::MAX),
                                if query == ANSWER { 1 } else { 2 },
                            ),
                        ),
                    ),
                    |r| r.get::<_, String>(3),
                )
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join("\n");
            assert!(plan.contains(index) && plan.contains("SEARCH"), "{plan}");
            assert!(
                !plan.contains("SCAN") && !plan.contains("TEMP B-TREE"),
                "{plan}"
            );
        }
    }
    #[test]
    fn excerpt_plan_selects_one_node_before_bounded_json_fragments() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {EXCERPT}"))
            .unwrap()
            .query_map(["node"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(
            plan.contains("SEARCH node USING INDEX sqlite_autoindex_node_1 (id=?)"),
            "{plan}"
        );
        assert!(
            !plan.contains("SCAN node") && !plan.contains("TEMP B-TREE"),
            "{plan}"
        );
    }
    #[test]
    fn excerpts_keep_natural_text_and_do_not_copy_tools_reasoning_or_credentials() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('n','assistant_response','now',?1)",[json!({"output":[{"type":"reasoning","summary":[{"text":"HIDDEN"}]},{"type":"function_call","arguments":"SECRET"},{"type":"message","content":[{"type":"output_text","text":"answer-日本語"}]}]}).to_string()]).unwrap();
        assert_eq!(excerpt(&db, "n").unwrap(), "answer-日本語");
        db.execute("UPDATE node SET payload_json=?1 WHERE id='n'",[json!({"role":"user","content":[{"type":"input_text","text":"x".repeat(20000)},{"type":"input_image","image_url":"SECRET"}]}).to_string()]).unwrap();
        assert_eq!(excerpt(&db, "n").unwrap(), "x".repeat(512));
        db.execute("UPDATE node SET payload_json=NULL WHERE id='n'", [])
            .unwrap();
        assert!(excerpt(&db, "n").unwrap().is_empty());
    }
    #[test]
    fn later_received_input_does_not_replace_the_saved_final_answer() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','now');INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','0','1');INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','0','1');") .unwrap();
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('a','u','assistant_response','answer-at',?1)",[json!({"output":[{"type":"message","content":[{"type":"output_text","text":"old answer"}]}]}).to_string()]).unwrap();
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json)VALUES('m','user','input-at',?1)",
            [json!({"role":"user","content":"new request"}).to_string()],
        )
        .unwrap();
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES('receipt','op',?1,'t','m','held','input-at')",[json!({"kind":"external_cli","label":"untrusted-name","token":"DO_NOT_COPY"}).to_string()]).unwrap();
        db.execute("UPDATE turn SET answer_node_id='a' WHERE id='u'", [])
            .unwrap();
        let answer = last_answer(&db, "t").unwrap().unwrap();
        let input = last_input(&db, "t").unwrap().unwrap();
        assert_eq!(answer["excerpt"], "old answer");
        assert_eq!(answer["turn_id"], "u");
        assert_eq!(answer["at"], "answer-at");
        assert_eq!(input["excerpt"], "new request");
        assert_eq!(input["state"], "held");
        assert_eq!(input["sender"]["label"], "untrusted-name");
        assert!(!input.to_string().contains("DO_NOT_COPY"));
        db.execute("UPDATE node SET payload_json=NULL WHERE id='a'", [])
            .unwrap();
        let answer = last_answer(&db, "t").unwrap().unwrap();
        assert_eq!(answer["body_available"], false);
        assert_eq!(answer["excerpt"], "");
        assert_eq!(answer["node_id"], "a");
    }
}
