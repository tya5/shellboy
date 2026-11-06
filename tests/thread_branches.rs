//! `thread branches` (#237) over real fork and rewind results: branches are Threads.
use rusqlite::Connection;
use serde_json::Value;
use shellboy::{
    config::Config,
    control::db::Store,
    protocol::{
        ApiError, Command, ErrorCode, Origin, Request, ThreadBranchesArgs, ThreadForkArgs,
        ThreadRewindArgs,
    },
    storage::DbWorker,
};
use std::sync::Arc;

struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
}

fn scalar(db: &Connection, sql: &str) -> rusqlite::Result<i64> {
    db.query_row(sql, [], |r| r.get(0))
}
// Thread T (title "Keep me"), one completed Run/Turn that owns every node.
const BASE: &str = "
INSERT INTO run(id,thread_id,state,created_at)VALUES('run','T','completed','2026-01-01T00:00:00');
INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','T','run','completed','2026-01-01T00:00:00');";

fn node(id: &str, kind: &str, at: u32, payload: &str) -> String {
    format!(
        "INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES('{id}','turn','{kind}','2026-01-01T00:00:{at:02}','{payload}',20);"
    )
}
const REQUESTS: [&str; 3] = ["fix the tests", "make CI pass", "update the README"];
/// A conversation n1..=count: odd nodes are user requests, even ones answers.
fn conversation(count: u32) -> String {
    (1..=count)
        .map(|i| {
            if i % 2 == 1 {
                let text = REQUESTS[(i as usize / 2) % REQUESTS.len()];
                node(
                    &format!("n{i}"),
                    "user",
                    i,
                    &format!("{{\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{text}\"}}]}}"),
                )
            } else {
                node(&format!("n{i}"), "assistant", i, "{\"role\":\"assistant\",\"content\":[]}")
            }
        })
        .collect()
}
fn path(thread: &str, ids: &[&str]) -> String {
    ids.iter()
        .enumerate()
        .map(|(i, id)| format!("INSERT INTO thread_path VALUES('{thread}',{i},'{id}');"))
        .collect()
}
fn n_path(count: u32) -> String {
    let names: Vec<String> = (1..=count).map(|i| format!("n{i}")).collect();
    path("T", &names.iter().map(String::as_str).collect::<Vec<_>>())
}

impl Fixture {
    async fn new(script: String) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("shellboy.db"))
            .await
            .unwrap();
        let config = Config::parse(
            r#"
            default_connection="test"
            default_model="fixture"
            [connections.test]
            base_url="http://127.0.0.1:1/v1"
            default_effort="custom"
        "#,
        )
        .unwrap();
        let cwd = temp.path().to_string_lossy().into_owned();
        db.call(move |db| {
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('T','Keep me',?1,?1,'p','m','now')", [cwd])?;
            db.execute_batch(BASE)?;
            db.execute_batch(&script)?;
            db.execute(
                "UPDATE thread SET cursor_node_id=(SELECT node_id FROM thread_path WHERE thread_id='T' ORDER BY ordinal DESC LIMIT 1) WHERE id='T'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let store = Store::new(
            db,
            Arc::new(config),
            temp.path().join("project"),
            "instance".into(),
        );
        Self { _temp: temp, store }
    }
    async fn query<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> R + Send + 'static,
    ) -> R {
        self.store.database.call(move |db| Ok(f(db))).await.unwrap()
    }
}

impl Fixture {
    async fn run(&self, command: Command) -> Result<Value, ApiError> {
        let request = Request::new(format!("instance:{}", uuid::Uuid::new_v4()), command);
        self.store
            .execute(request, None::<Origin>)
            .await
            .map(|result| result.result)
    }
    async fn fork(&self, thread: &str, node: &str) -> String {
        let value = self
            .run(Command::ThreadFork(ThreadForkArgs {
                agents: None,
                thread_id: thread.into(),
                node_id: node.into(),
                cwd: None,
            }))
            .await
            .unwrap();
        value["thread_id"].as_str().unwrap().to_owned()
    }
    async fn rewind_to(&self, thread: &str, node: &str) -> String {
        let value = self
            .run(Command::ThreadRewind(ThreadRewindArgs {
                thread_id: Some(thread.into()),
                node_id: node.into(),
                exclude_node: false,
            }))
            .await
            .unwrap();
        value["branch_thread_id"].as_str().unwrap().to_owned()
    }
    async fn branches(
        &self,
        thread: &str,
        limit: u32,
        cursor: Option<String>,
    ) -> Result<Value, ApiError> {
        self.run(Command::ThreadBranches(ThreadBranchesArgs {
            thread_id: Some(thread.into()),
            limit,
            cursor,
        }))
        .await
    }
}
fn item<'a>(value: &'a Value, id: &str) -> &'a Value {
    value["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["thread_id"] == id)
        .unwrap_or_else(|| panic!("{id} not listed in {value}"))
}

#[tokio::test]
async fn fork_and_rewind_children_appear_as_branches_with_their_divergence_points() {
    let f = Fixture::new([conversation(6), n_path(6)].concat()).await;
    let forked = f.fork("T", "n3").await;
    let cut = f.rewind_to("T", "n2").await;
    let value = f.branches("T", 50, None).await.unwrap();
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    assert_eq!(value["root_thread_id"], "T");
    assert_eq!(value["family_threads"], 3);
    assert_eq!(item(&value, "T")["state"], "current");
    // The fork shares n1..n3 with the path T had then; T now ends at n2, so the fork extends it.
    assert_eq!(item(&value, &forked)["divergence"]["node_id"], "n2");
    assert_eq!(
        item(&value, &forked)["divergence"]["parent_continues"],
        false
    );
    assert_eq!(item(&value, &cut)["divergence"]["node_id"], "n2");
    assert_eq!(item(&value, &cut)["size"]["path_nodes"], 6);
    assert_eq!(item(&value, &cut)["size"]["nodes_after_divergence"], 4);
    assert_eq!(item(&value, "T")["size"]["path_nodes"], 2);
    // The same family is seen from a branch, which is then the current one.
    let from_cut = f.branches(&cut, 50, None).await.unwrap();
    assert_eq!(from_cut["root_thread_id"], "T");
    assert_eq!(item(&from_cut, &cut)["state"], "current");
    assert_eq!(item(&from_cut, "T")["state"], "ancestor");
}

#[tokio::test]
async fn a_read_is_bounded_validated_and_changes_nothing() {
    let f = Fixture::new([conversation(4), n_path(4)].concat()).await;
    for node in ["n1", "n2", "n3"] {
        f.fork("T", node).await;
    }
    let before = f
        .query(|db| {
            scalar(
                db,
                "SELECT (SELECT count(*) FROM thread)*1000+(SELECT count(*) FROM thread_path)",
            )
            .unwrap()
        })
        .await;
    let first = f.branches("T", 2, None).await.unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let second = f
        .branches("T", 2, first["next_cursor"].as_str().map(str::to_owned))
        .await
        .unwrap();
    assert_eq!(second["items"].as_array().unwrap().len(), 2);
    assert!(second["next_cursor"].is_null());
    assert_eq!(first["limits"]["family_threads"], 256);
    for bad in [0u32, 201] {
        assert_eq!(
            f.branches("T", bad, None).await.unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
    assert_eq!(
        f.branches("missing", 10, None).await.unwrap_err().code,
        ErrorCode::ThreadNotFound
    );
    let after = f
        .query(|db| {
            scalar(
                db,
                "SELECT (SELECT count(*) FROM thread)*1000+(SELECT count(*) FROM thread_path)",
            )
            .unwrap()
        })
        .await;
    assert_eq!(before, after);
}
