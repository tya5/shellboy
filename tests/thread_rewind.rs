//! `thread rewind` (#236): the same Thread, cut back to a node; the removed
//! part stays as an ordinary child Thread (branches are Threads).
use rusqlite::Connection;
use serde_json::{Value, json};
use shellboy::{
    config::Config,
    control::db::Store,
    protocol::{
        ApiError, Command, Deliver, ErrorCode, MessageSendArgs, Origin, Request, ThreadRewindArgs,
        ThreadSetArgs,
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
fn text(db: &Connection, sql: &str) -> rusqlite::Result<String> {
    db.query_row(sql, [], |r| r.get(0))
}
fn ids(db: &Connection, thread: &str) -> Vec<String> {
    db.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")
        .unwrap()
        .query_map([thread], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
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
/// Plain messages n1..=count, one second apart.
fn plain(count: u32) -> String {
    (1..=count)
        .map(|i| node(&format!("n{i}"), "message", i, "{\"text\":\"x\"}"))
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
    async fn rewind(
        &self,
        thread: Option<&str>,
        node: &str,
        caller: Option<Origin>,
    ) -> Result<Value, ApiError> {
        self.rewind_with(thread, node, false, caller).await
    }
    async fn rewind_with(
        &self,
        thread: Option<&str>,
        node: &str,
        exclude_node: bool,
        caller: Option<Origin>,
    ) -> Result<Value, ApiError> {
        self.rewind_args(thread, node, exclude_node, false, caller)
            .await
    }
    async fn rewind_args(
        &self,
        thread: Option<&str>,
        node: &str,
        exclude_node: bool,
        dry_run: bool,
        caller: Option<Origin>,
    ) -> Result<Value, ApiError> {
        let request = Request::new(
            format!("instance:{}", uuid::Uuid::new_v4()),
            Command::ThreadRewind(ThreadRewindArgs {
                thread_id: thread.map(str::to_owned),
                node_id: node.into(),
                exclude_node,
                dry_run,
            }),
        );
        self.store
            .execute(request, caller)
            .await
            .map(|result| result.result)
    }
    async fn query<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> R + Send + 'static,
    ) -> R {
        self.store.database.call(move |db| Ok(f(db))).await.unwrap()
    }
}

#[tokio::test]
async fn rewind_cuts_the_path_and_keeps_the_tail_as_a_child_thread() {
    let f = Fixture::new(
        [
            plain(6),
            n_path(6),
            "UPDATE thread SET spill_override=0 WHERE id='T';".into(),
        ]
        .concat(),
    )
    .await;
    let value = f.rewind(Some("T"), "n3", None).await.unwrap();
    assert_eq!(value["thread_id"], "T");
    assert_eq!(value["node_id"], "n3");
    assert_eq!(value["previous_end_node_id"], "n6");
    assert_eq!(value["title"], "Keep me");
    assert_eq!(value["path_nodes"], json!({"before":6,"after":3}));
    assert_eq!(value["rewound_into_summarized_range"], false);
    let branch = value["branch_thread_id"].as_str().unwrap().to_owned();
    let moved = branch.clone();
    f.query(move |db| {
        let branch = moved;
        // Same Thread, same ID and title, path ends at the node.
        assert_eq!(ids(db, "T"), ["n1", "n2", "n3"]);
        assert_eq!(
            text(db, "SELECT title FROM thread WHERE id='T'").unwrap(),
            "Keep me"
        );
        assert_eq!(
            text(db, "SELECT cursor_node_id FROM thread WHERE id='T'").unwrap(),
            "n3"
        );
        assert_eq!(
            scalar(db, "SELECT selection_revision FROM thread WHERE id='T'").unwrap(),
            1
        );
        // The old full path lives on as a child (existing fork machinery).
        assert_eq!(ids(db, &branch), ["n1", "n2", "n3", "n4", "n5", "n6"]);
        assert_eq!(
            text(
                db,
                &format!("SELECT parent_thread_id FROM thread WHERE id='{branch}'")
            )
            .unwrap(),
            "T"
        );
        assert_eq!(
            text(
                db,
                &format!("SELECT cursor_node_id FROM thread WHERE id='{branch}'")
            )
            .unwrap(),
            "n6"
        );
        // No node was copied or deleted.
        assert_eq!(scalar(db, "SELECT count(*) FROM node").unwrap(), 6);
    })
    .await;
    assert_eq!(
        f.query({
            let branch = branch.clone();
            move |db| {
                db.query_row(
                    "SELECT spill_override FROM thread WHERE id=?1",
                    [&branch],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .unwrap()
            }
        })
        .await,
        Some(0)
    );
    let changed = f
        .store
        .execute(
            Request::new(
                format!("instance:{}", uuid::Uuid::new_v4()),
                Command::ThreadSet(Box::new(ThreadSetArgs {
                    thread_id: Some("T".into()),
                    spill: Some("on".into()),
                    ..Default::default()
                })),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(changed.result["spill"], true);
    assert_eq!(
        f.query({
            let branch = branch.clone();
            move |db| {
                db.query_row(
                    "SELECT spill_override FROM thread WHERE id=?1",
                    [&branch],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .unwrap()
            }
        })
        .await,
        Some(0),
        "the rewind branch keeps the raw setting it copied"
    );
    // Rewinding to the current end is refused and creates nothing.
    let error = f.rewind(Some("T"), "n3", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    // A node that is nowhere in this Thread is refused.
    let error = f.rewind(Some("T"), "n5", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(
        f.query(|db| scalar(db, "SELECT count(*) FROM thread").unwrap())
            .await,
        2
    );
    // Undo is opening the child: it can itself be rewound independently.
    let again = f.rewind(Some(&branch), "n5", None).await.unwrap();
    assert_eq!(again["thread_id"], json!(branch));
}

#[tokio::test]
async fn refused_while_the_thread_has_live_work_and_nothing_changes() {
    let f = Fixture::new(
        [
            plain(4),
            n_path(4),
            "INSERT INTO run(id,thread_id,state,created_at)VALUES('live','T','live','2026-01-02');"
                .into(),
        ]
        .concat(),
    )
    .await;
    let error = f.rewind(Some("T"), "n2", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(error.message.contains("live Run"), "{}", error.message);
    // A definitely unadmitted message can be delivered after rewinding.
    f.query(|db| {
        db.execute_batch("UPDATE run SET state='completed' WHERE id='live'; INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,path_admission)VALUES('rc','op','{\"kind\":\"human\"}','T','n4','run','turn','accepted','now','unadmitted');").unwrap();
    })
    .await;
    f.rewind(Some("T"), "n2", None).await.unwrap();
    f.query(|db| {
        assert_eq!(ids(db, "T"), ["n1", "n2"]);
        assert_eq!(scalar(db, "SELECT count(*) FROM thread").unwrap(), 2);
    })
    .await;
    let uncertain = Fixture::new([plain(4), n_path(4),
        "INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,path_admission)VALUES('unknown','unknown-op','{\"kind\":\"human\"}','T','n4','held','now','unknown');".into(),
    ].concat()).await;
    let error = uncertain.rewind(Some("T"), "n2", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(error.message.contains("unknown path admission"));
}

#[tokio::test]
async fn unadmitted_input_is_delivered_once_on_the_rewound_path() {
    let script = [
        plain(4),
        n_path(4),
        "INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,path_admission)VALUES('rc','op','{\"kind\":\"human\"}','T','n4','run','turn','accepted','now','unadmitted');".into(),
    ]
    .concat();
    let f = Fixture::new(script).await;

    // Rewind moves n3/n4 to the child. The accepted n4 receipt remains pending.
    f.rewind(Some("T"), "n2", None).await.unwrap();
    assert_eq!(f.query(|db| ids(db, "T")).await, ["n1", "n2"]);

    // A real human send starts the next Run and invokes resume recovery. It must
    // append the older accepted input to the selected path before the new prompt.
    let request = Request::new(
        format!("instance:{}", uuid::Uuid::new_v4()),
        Command::MessageSend(MessageSendArgs {
            to_human: false,
            human: true,
            resume_recovery: false,
            expect_reply: false,
            sender_label: None,
            thread_id: Some("T".into()),
            text: "continue after rewind".into(),
            deliver: Deliver::Next,
        }),
    );
    f.store.execute(request, None).await.unwrap();

    f.query(|db| {
        let path = ids(db, "T");
        assert_eq!(path, ["n1", "n2", "n4"]);
        assert_eq!(path.iter().filter(|id| id.as_str() == "n4").count(), 1);
        assert_eq!(
            scalar(db, "SELECT count(*) FROM receipt WHERE id='rc'").unwrap(),
            1
        );
        assert_eq!(
            text(db, "SELECT state FROM receipt WHERE id='rc'").unwrap(),
            "applied"
        );
        assert_eq!(
            text(db, "SELECT path_admission FROM receipt WHERE id='rc'").unwrap(),
            "admitted"
        );
        assert_eq!(
            text(db, "SELECT hold_reason FROM receipt WHERE id='rc'").unwrap(),
            "recovered_at_explicit_resume"
        );
        assert_eq!(ids(db, "T"), ["n1", "n2", "n4"]);
        let branch: String = db
            .query_row(
                "SELECT id FROM thread WHERE parent_thread_id='T'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ids(db, &branch), ["n1", "n2", "n3", "n4"]);
    })
    .await;
}

#[tokio::test]
async fn refused_inside_an_unfinished_tool_group_without_partial_writes() {
    let call = r#"{"output":[{"type":"function_call","call_id":"c1","name":"shell"}]}"#;
    let result = r#"{"item":{"type":"function_call_output","call_id":"c1","output":"ok"}}"#;
    let script = [
        node("n1", "message", 1, "{\"text\":\"x\"}"),
        node("n2", "assistant_response", 2, call),
        node("n3", "tool_result", 3, result),
        node("n4", "message", 4, "{\"text\":\"x\"}"),
        n_path(4),
    ]
    .concat();
    let f = Fixture::new(script).await;
    // n2 is the call without its result: the cut would split the group.
    let error = f.rewind(Some("T"), "n2", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    f.query(|db| {
        assert_eq!(ids(db, "T"), ["n1", "n2", "n3", "n4"]);
        assert_eq!(scalar(db, "SELECT count(*) FROM thread").unwrap(), 1);
    })
    .await;
    // After the result, the group is whole.
    f.rewind(Some("T"), "n3", None).await.unwrap();
    f.query(|db| assert_eq!(ids(db, "T"), ["n1", "n2", "n3"]))
        .await;
}

/// Path x, S(summary of a,b,c), tail; originals a,b,c hang off replacement r1.
fn summarized(state: &str) -> String {
    [
        plain(0),
        node("x", "message", 1, "{\"text\":\"x\"}"),
        node("a", "message", 2, "{\"text\":\"x\"}"),
        node("b", "message", 3, "{\"text\":\"x\"}"),
        node("c", "message", 4, "{\"text\":\"x\"}"),
        node("s", "summary", 5, "{\"text\":\"sum\"}"),
        node("tail", "message", 6, "{\"text\":\"x\"}"),
        path("T", &["x", "s", "tail"]),
        format!("INSERT INTO replacement VALUES('r1','s','k','1','1','{state}','now');"),
        "INSERT INTO replacement_source VALUES('r1',0,'a',0,NULL),('r1',1,'b',0,NULL),('r1',2,'c',0,NULL);INSERT INTO thread_replacement VALUES('T','r1');".into(),
    ]
    .concat()
}

#[tokio::test]
async fn rewinding_into_a_summarized_range_works_while_originals_exist() {
    let f = Fixture::new(summarized("completed")).await;
    let value = f.rewind(Some("T"), "b", None).await.unwrap();
    assert_eq!(value["rewound_into_summarized_range"], true);
    assert_eq!(value["path_nodes"], json!({"before":3,"after":3}));
    let branch = value["branch_thread_id"].as_str().unwrap().to_owned();
    f.query(move |db| {
        assert_eq!(ids(db, "T"), ["x", "a", "b"]);
        // The child keeps the summarized view, including its replacement.
        assert_eq!(ids(db, &branch), ["x", "s", "tail"]);
        assert_eq!(
            scalar(
                db,
                &format!("SELECT count(*) FROM thread_replacement WHERE thread_id='{branch}'")
            )
            .unwrap(),
            1
        );
    })
    .await;
}

#[tokio::test]
async fn rewinding_into_a_pruned_or_reclaimed_range_fails_clearly() {
    let f = Fixture::new(summarized("pruned")).await;
    let error = f.rewind(Some("T"), "b", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::TargetUnavailable);
    assert!(error.message.contains("pruned"), "{}", error.message);
    // Originals reclaimed by GC after a prune: bodies are gone.
    let f = Fixture::new(summarized("completed")).await;
    f.query(|db| {
        db.execute(
            "UPDATE node SET payload_json=NULL,deleted_reason='pruned_unreachable' WHERE id='b'",
            [],
        )
        .unwrap();
    })
    .await;
    let error = f.rewind(Some("T"), "b", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::TargetUnavailable);
    f.query(|db| {
        assert_eq!(ids(db, "T"), ["x", "s", "tail"]);
        assert_eq!(scalar(db, "SELECT count(*) FROM thread").unwrap(), 1);
    })
    .await;
    // The path itself (before the summary) still rewinds.
    f.rewind(Some("T"), "x", None).await.unwrap();
}

#[tokio::test]
async fn attachments_whose_source_leaves_the_path_are_not_selected() {
    let script = [
        plain(5),
        n_path(5),
        "INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('keep','n2','/p/a.txt','file','external','now'),('gone','n5','/p/b.txt','file','external','now');
         INSERT INTO attachment_selection(thread_id,attachment_id,state,reason,selected_at,selected_order)VALUES('T','keep','active',NULL,'now',0),('T','gone','active',NULL,'now',1);".into(),
    ]
    .concat();
    let f = Fixture::new(script).await;
    let value = f.rewind(Some("T"), "n3", None).await.unwrap();
    assert_eq!(value["attachment_selections_removed"], 1);
    let branch = value["branch_thread_id"].as_str().unwrap().to_owned();
    f.query(move |db| {
        let selected = |thread: &str| {
            db.prepare("SELECT attachment_id FROM attachment_selection WHERE thread_id=?1 ORDER BY attachment_id")
                .unwrap()
                .query_map([thread], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(selected("T"), ["keep"]);
        // The preserved branch still sends both.
        assert_eq!(selected(&branch), ["gone", "keep"]);
    })
    .await;
}

#[tokio::test]
async fn result_lists_shell_executions_and_child_threads_started_after_the_node() {
    let mut script = vec![plain(6), n_path(6)];
    // 25 executions after n3 (seconds 10..34) and 2 before it.
    for i in 0..27 {
        let at = if i < 2 { 1 + i } else { 8 + i };
        script.push(format!("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('sh{i:02}','run','turn','op{i}','exited','/w','{{\"command\":\"echo {i}\"}}','UTF-8','UTF-8','closed','2026-01-01T00:00:{at:02}');"));
    }
    // Children: delegated one before n3 (own turn at second 1) and one after
    // (own turn at second 5), plus a pure fork whose fork node is n5.
    script.push("INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('c-old','T','old','/w','/w','p','m','now'),('c-new','T','new','/w','/w','p','m','now');
        INSERT INTO run(id,thread_id,state,created_at)VALUES('r-old','c-old','completed','x'),('r-new','c-new','completed','x');
        INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('t-old','c-old','r-old','completed','x'),('t-new','c-new','r-new','completed','x');
        INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES('o1','t-old','message','2026-01-01T00:00:01','{}',2),('o2','t-new','message','2026-01-01T00:00:40','{}',2);".into());
    script.push("INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,cursor_node_id,updated_at)VALUES('c-fork','T','fork','/w','/w','p','m','n5','now');".into());
    let f = Fixture::new(script.concat()).await;
    let value = f.rewind(Some("T"), "n3", None).await.unwrap();
    let shells = &value["shell_executions_after_node"];
    assert_eq!(shells["count"], 25);
    assert_eq!(shells["items"].as_array().unwrap().len(), 20);
    assert_eq!(shells["truncated"], true);
    assert_eq!(shells["items"][0]["execution_id"], "sh02");
    assert_eq!(shells["items"][0]["command_excerpt"], "echo 2");
    let children = &value["child_threads_after_node"];
    assert_eq!(children["count"], 2);
    let listed: Vec<&str> = children["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["thread_id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, ["c-fork", "c-new"]);
    // The rewind's own branch is not reported as a started child.
    assert!(!listed.contains(&value["branch_thread_id"].as_str().unwrap()));
    assert!(
        value["note"]
            .as_str()
            .unwrap()
            .contains("shell side effects")
    );
    // Nothing about the shell executions changed.
    assert_eq!(
        f.query(|db| scalar(db, "SELECT count(*) FROM shell_execution").unwrap())
            .await,
        27
    );
}

fn origin(thread: &str) -> Origin {
    Origin {
        instance_id: "instance".into(),
        thread_id: thread.into(),
        run_id: "run".into(),
        turn_id: "turn".into(),
        execution_id: "execution-fixture".into(),
        token: "fixture-token".into(),
    }
}

#[tokio::test]
async fn omitted_thread_targets_the_caller_and_is_refused_while_its_run_is_live() {
    let f = Fixture::new([plain(4), n_path(4)].concat()).await;
    // External callers must name the Thread.
    let error = f.rewind(None, "n2", None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    // From inside the Thread, its own Run is live: self-rewind is refused.
    f.query(|db| {
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at)VALUES('mine','T','live','2026-01-02')",
            [],
        )
        .unwrap();
    })
    .await;
    let error = f.rewind(None, "n2", Some(origin("T"))).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(error.details.as_ref().unwrap()["run_id"], "mine");
    let hint = error.hint.as_deref().unwrap();
    assert!(
        hint.contains("thread fork") && hint.contains("/rewind"),
        "{hint}"
    );
    assert_eq!(f.query(|db| ids(db, "T").len()).await, 4);
    // Once that Run is over, the omitted --thread resolves to the caller.
    f.query(|db| {
        db.execute("UPDATE run SET state='completed' WHERE id='mine'", [])
            .unwrap();
    })
    .await;
    let value = f.rewind(None, "n2", Some(origin("T"))).await.unwrap();
    assert_eq!(value["thread_id"], "T");
    assert_eq!(f.query(|db| ids(db, "T")).await, ["n1", "n2"]);
}

#[tokio::test]
async fn exclude_node_ends_the_path_just_before_the_picked_request() {
    let f = Fixture::new([plain(5), n_path(5)].concat()).await;
    // The first node has nothing before it.
    let error = f
        .rewind_with(Some("T"), "n1", true, None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(f.query(|db| ids(db, "T").len()).await, 5);
    let value = f.rewind_with(Some("T"), "n4", true, None).await.unwrap();
    assert_eq!(value["node_id"], "n3");
    assert_eq!(f.query(|db| ids(db, "T")).await, ["n1", "n2", "n3"]);
}

#[tokio::test]
async fn exclude_node_works_inside_a_summarized_range_too() {
    let f = Fixture::new(summarized("completed")).await;
    // b excluded: the path ends at a, with the summary expanded.
    let value = f.rewind_with(Some("T"), "b", true, None).await.unwrap();
    assert_eq!(value["rewound_into_summarized_range"], true);
    assert_eq!(f.query(|db| ids(db, "T")).await, ["x", "a"]);
}

#[tokio::test]
async fn every_candidate_summary_is_tried_not_only_the_newest() {
    // A newer replacement lists b too but its output is not on the path.
    let script = [
        summarized("completed"),
        node("dead", "summary", 9, "{\"text\":\"d\"}"),
        "INSERT INTO replacement VALUES('r2','dead','k2','1','1','completed','zzzz');INSERT INTO replacement_source VALUES('r2',0,'b',0,NULL);INSERT INTO thread_replacement VALUES('T','r2');".into(),
    ]
    .concat();
    let f = Fixture::new(script).await;
    f.rewind(Some("T"), "b", None).await.unwrap();
    assert_eq!(f.query(|db| ids(db, "T")).await, ["x", "a", "b"]);
}

#[tokio::test]
async fn result_lists_unanswered_replies_to_messages_the_thread_sent() {
    let script = [
        plain(4),
        n_path(4),
        "INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('other','','/w','/w','p','m','now');
         INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,result_json,created_at)VALUES
          ('rc1','op1','{\"kind\":\"model\",\"thread_id\":\"T\"}','other','n2','applied','{\"reply\":{\"state\":\"pending\"}}','2026-01-01'),
          ('rc2','op2','{\"kind\":\"model\",\"thread_id\":\"T\"}','other','n3','applied','{\"reply\":{\"state\":\"answered\"}}','2026-01-01');".into(),
    ]
    .concat();
    let f = Fixture::new(script).await;
    let value = f.rewind(Some("T"), "n2", None).await.unwrap();
    let replies = &value["pending_replies_to_sent_messages"];
    assert_eq!(replies["count"], 1);
    assert_eq!(replies["items"][0]["message_id"], "n2");
    assert_eq!(replies["items"][0]["target_thread_id"], "other");
    assert!(value["note"].as_str().unwrap().contains("shell_ended"));
}

/// Measurement for decisions.md, not a gate (wall-clock numbers are facts, not
/// promises): `cargo test --release --test integration rewind_path_cost -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "measurement only"]
async fn rewind_path_cost() {
    for size in [10_000u32, 100_000, 200_000] {
        let f = Fixture::new(String::new()).await;
        f.query(move |db| {
            db.execute_batch(&format!(
                "WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i<{size})
                 INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) SELECT 'n'||i,'turn','message','2026-01-01T00:00:00','{{\"text\":\"x\"}}',12 FROM s;
                 INSERT INTO thread_path SELECT 'T',i-1,'n'||i FROM (WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i<{size}) SELECT i FROM s);
                 UPDATE thread SET cursor_node_id='n{size}' WHERE id='T';"
            ))
            .unwrap();
        })
        .await;
        let cut = size / 2;
        let started = std::time::Instant::now();
        f.rewind(Some("T"), &format!("n{cut}"), None).await.unwrap();
        let first = started.elapsed();
        // Rewinding again near the new end copies a short prefix + the tail child.
        let started = std::time::Instant::now();
        f.rewind(Some("T"), &format!("n{}", cut - 10), None)
            .await
            .unwrap();
        let second = started.elapsed();
        println!(
            "path {size}: rewind to the middle {first:?}; second rewind (path {cut}) {second:?}"
        );
    }
}

/// Every row of every table (app_meta, edges, thread revisions included), as
/// one comparable string.
fn dump(db: &Connection) -> String {
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut out = String::new();
    for table in tables {
        // The executor's request journal (one receipt and its quota counters
        // per request, dry run or not) is bookkeeping about the call, not state
        // of the conversation.
        if matches!(table.as_str(), "operation_receipt" | "quota_counter") {
            continue;
        }
        let mut statement = db.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let columns = statement.column_count();
        let mut rows: Vec<String> = statement
            .query_map([], |r| {
                Ok((0..columns)
                    .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        out.push_str(&format!("== {table}\n{}\n", rows.join("\n")));
    }
    out
}

#[tokio::test]
async fn dry_run_reports_the_facts_of_the_real_rewind_and_writes_nothing() {
    let f = Fixture::new([plain(6), n_path(6)].concat()).await;
    let before = f.query(dump).await;
    let preview = f
        .rewind_args(Some("T"), "n4", true, true, None)
        .await
        .unwrap();
    assert_eq!(f.query(dump).await, before, "dry run wrote something");
    assert_eq!(preview["dry_run"], true);
    assert!(preview.get("branch_thread_id").is_none());
    assert_eq!(preview["path_nodes"], json!({"before":6,"after":3}));
    assert_eq!(preview["removed"], json!({"nodes":3,"turns":1}));
    // Refusals are the real ones and also leave nothing behind.
    let error = f
        .rewind_args(Some("T"), "n6", false, true, None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(f.query(dump).await, before);
    // The real rewind then reports the same facts.
    let real = f.rewind_with(Some("T"), "n4", true, None).await.unwrap();
    for key in [
        "path_nodes",
        "removed",
        "shell_executions_after_node",
        "child_threads_after_node",
        "pending_replies_to_sent_messages",
    ] {
        assert_eq!(preview[key], real[key], "{key}");
    }
    assert!(real["branch_thread_id"].is_string());
    assert!(real.get("dry_run").is_none());
}

#[tokio::test]
async fn preview_equals_real_with_nonzero_shell_child_and_reply_counts() {
    let script = [
        plain(6),
        n_path(6),
        "INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('sh1','run','turn','op1','exited','/w','{\"command\":\"echo 1\"}','UTF-8','UTF-8','closed','2026-01-01T00:00:05'),('sh2','run','turn','op2','exited','/w','{\"command\":\"echo 2\"}','UTF-8','UTF-8','closed','2026-01-01T00:00:06');
         INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,cursor_node_id,updated_at)VALUES('c-fork','T','fork','/w','/w','p','m','n5','now');
         INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('other','','/w','/w','p','m','now');
         INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,result_json,created_at)VALUES('rc1','op9','{\"kind\":\"model\",\"thread_id\":\"T\"}','other','n2','applied','{\"reply\":{\"state\":\"pending\"}}','2026-01-01');".into(),
    ]
    .concat();
    let f = Fixture::new(script).await;
    let before = f.query(dump).await;
    let preview = f
        .rewind_args(Some("T"), "n3", true, true, None)
        .await
        .unwrap();
    assert_eq!(f.query(dump).await, before, "dry run wrote something");
    assert_eq!(preview["shell_executions_after_node"]["count"], 2);
    assert_eq!(preview["child_threads_after_node"]["count"], 1);
    assert_eq!(preview["pending_replies_to_sent_messages"]["count"], 1);
    let real = f.rewind_with(Some("T"), "n3", true, None).await.unwrap();
    for key in [
        "path_nodes",
        "removed",
        "attachment_selections_removed",
        "rewound_into_summarized_range",
        "shell_executions_after_node",
        "child_threads_after_node",
        "pending_replies_to_sent_messages",
    ] {
        assert_eq!(preview[key], real[key], "{key}");
    }
}
