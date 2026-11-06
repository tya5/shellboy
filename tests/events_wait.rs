//! `events.wait` (#243 stage B): receipt-free long-poll over the owner's change clock,
//! plus one test per mutation family proving each commit path advances the clock.
use crate::runtime_features::{answer, incoming, owner, proxy, send, settled, thread};
use crate::test_watchdog::watchdog;
use serde_json::{Value, json};
use shellboy::{
    control::Control,
    protocol::{
        ApiError, AttachmentAddArgs, AttachmentKind, AttachmentRemoveArgs, Command, Deliver,
        ErrorCode, EventCursor, EventsWaitArgs, MessageCancelArgs, MessageReplyArgs,
        MessageSendArgs, Request, RunTarget, ThreadArchiveArgs, ThreadDeleteArgs, ThreadForkArgs,
        ThreadPruneArgs, ThreadRewindArgs, ThreadSetArgs, ThreadTarget,
    },
};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

async fn raw(
    control: &Arc<Control>,
    after: Option<&str>,
    threads: &[&str],
    timeout_ms: u64,
) -> Result<Value, ApiError> {
    let response = watchdog(
        "events.wait",
        control.request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            Command::EventsWait(EventsWaitArgs {
                after: after.map(str::to_owned),
                thread_ids: threads.iter().map(|id| (*id).to_owned()).collect(),
                timeout_ms,
                include_text: false,
            }),
        )),
    )
    .await
    .unwrap();
    if response.ok {
        Ok(response.result.unwrap())
    } else {
        Err(response.error.unwrap())
    }
}
async fn ev(
    control: &Arc<Control>,
    after: Option<&str>,
    threads: &[&str],
    timeout_ms: u64,
) -> Value {
    raw(control, after, threads, timeout_ms).await.unwrap()
}
fn cursor(value: &Value) -> String {
    value["cursor"].as_str().unwrap().to_owned()
}
fn ids(value: &Value) -> Vec<String> {
    value["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["thread_id"].as_str().unwrap().to_owned())
        .collect()
}
async fn baseline(control: &Arc<Control>) -> String {
    cursor(&ev(control, None, &[], 0).await)
}
/// Consume pending hints until a short quiet period; returns a cursor at that point.
async fn quiet(control: &Arc<Control>) -> String {
    let mut at = baseline(control).await;
    loop {
        let result = ev(control, Some(&at), &[], 300).await;
        at = cursor(&result);
        if result["status"] == "timeout" {
            return at;
        }
    }
}
/// The clock must report `thread` after `from`; waits (deadlock watchdog only).
#[track_caller]
fn advanced<'a>(
    control: &'a Arc<Control>,
    from: &'a str,
    thread: &'a str,
) -> impl std::future::Future<Output = Value> + 'a {
    let caller = std::panic::Location::caller();
    async move {
        let mut at = from.to_owned();
        loop {
            let result = ev(control, Some(&at), &[thread], 30_000).await;
            assert_ne!(
                result["status"],
                "resync_required",
                "{}:{}",
                caller.file(),
                caller.line()
            );
            assert_ne!(
                result["status"],
                "timeout",
                "no change for {thread} at {}:{}",
                caller.file(),
                caller.line()
            );
            if ids(&result).iter().any(|id| id == thread) {
                return result;
            }
            at = cursor(&result);
        }
    }
}
/// Follow hints from `from` until the saved fact (`sql` counts > 0) is visible; returns the
/// cursor reached so the next step starts exactly there. A hint must keep arriving until then; a commit with no later `changed()` ends in a failed timeout.
#[track_caller]
fn until_saved_within<'a>(
    control: &'a Arc<Control>,
    from: &'a str,
    thread: &'a str,
    sql: &'static str,
    timeout_ms: u64,
) -> impl std::future::Future<Output = String> + 'a {
    let caller = std::panic::Location::caller();
    async move {
        let mut at = from.to_owned();
        loop {
            let result = ev(control, Some(&at), &[thread], timeout_ms).await;
            assert_eq!(
                result["status"],
                "changed",
                "saved fact never announced for {thread} at {}:{}",
                caller.file(),
                caller.line()
            );
            at = cursor(&result);
            if count(control, sql, thread).await > 0 {
                return at;
            }
        }
    }
}
#[track_caller]
fn until_saved<'a>(
    control: &'a Arc<Control>,
    from: &'a str,
    thread: &'a str,
    sql: &'static str,
) -> impl std::future::Future<Output = String> + 'a {
    until_saved_within(control, from, thread, sql, 30_000)
}
async fn command(control: &Arc<Control>, command: Command) -> Result<Value, ApiError> {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    if response.ok {
        Ok(response.result.unwrap())
    } else {
        Err(response.error.unwrap())
    }
}
async fn count(control: &Arc<Control>, sql: &'static str, id: &str) -> i64 {
    let id = id.to_owned();
    control
        .store
        .database
        .call(move |db| Ok(db.query_row(sql, [id], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap()
}
async fn text(control: &Arc<Control>, sql: &'static str, id: &str) -> String {
    let id = id.to_owned();
    control
        .store
        .database
        .call(move |db| Ok(db.query_row(sql, [id], |r| r.get::<_, String>(0))?))
        .await
        .unwrap()
}
async fn fixture() -> (
    tempfile::TempDir,
    shellboy::app::DataOwner,
    Arc<Control>,
    tokio::sync::mpsc::Receiver<crate::runtime_features::Incoming>,
    tokio::task::JoinHandle<()>,
) {
    let root = tempfile::tempdir().unwrap();
    let (url, receive, server) = proxy().await;
    let owner = owner(root.path(), &url).await;
    let control = Control::new(&owner, root.path().into());
    (root, owner, control, receive, server)
}
async fn close(
    control: Arc<Control>,
    owner: shellboy::app::DataOwner,
    server: tokio::task::JoinHandle<()>,
) {
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn baseline_order_merge_resume_filter_and_timeout() {
    let (_root, owner, control, _receive, server) = fixture().await;
    let base = baseline(&control).await;
    let first = ev(&control, None, &[], 0).await;
    assert_eq!(first["status"], "baseline");
    assert_eq!(first["changes"], json!([]));
    assert_eq!(first["more"], false);
    let a = thread(&control).await;
    let b = thread(&control).await;
    command(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(a.clone()),
            title: Some("renamed".into()),
            ..Default::default()
        })),
    )
    .await
    .unwrap();
    let result = ev(&control, Some(&base), &[], 0).await;
    assert_eq!(result["status"], "changed");
    // b was last touched before a's rename: ascending by last change, a merged into one entry.
    assert_eq!(ids(&result), [b.clone(), a.clone()]);
    let seqs: Vec<u64> = result["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["seq"].as_u64().unwrap())
        .collect();
    assert!(seqs[0] < seqs[1]);
    assert_eq!(result["more"], false);
    // Resume: nothing new after the returned cursor; timeout 0 returns at once.
    let resumed = ev(&control, Some(&cursor(&result)), &[], 0).await;
    assert_eq!(resumed["status"], "timeout");
    assert_eq!(resumed["cursor"], result["cursor"]);
    // Filter.
    let only_b = ev(&control, Some(&base), &[&b], 0).await;
    assert_eq!(ids(&only_b), std::slice::from_ref(&b));
    let none = ev(&control, Some(&base), &["no-such-thread"], 0).await;
    assert_eq!(none["status"], "timeout");
    // A blocked poll wakes for a matching change only.
    let waiting = {
        let control = control.clone();
        let from = cursor(&result);
        let target = a.clone();
        tokio::spawn(async move { ev(&control, Some(&from), &[&target], 30_000).await })
    };
    command(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(b.clone()),
            title: Some("other".into()),
            ..Default::default()
        })),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !waiting.is_finished(),
        "a change to another Thread must not satisfy the filter"
    );
    command(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(a.clone()),
            title: Some("again".into()),
            ..Default::default()
        })),
    )
    .await
    .unwrap();
    let woken = watchdog("filtered long-poll", waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(woken["status"], "changed");
    assert_eq!(ids(&woken), [a]);
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn more_pages_resume_without_a_gap_and_bad_cursors_are_rejected() {
    let (_root, owner, control, _receive, server) = fixture().await;
    let base = baseline(&control).await;
    let mut created = Vec::new();
    for _ in 0..70 {
        created.push(thread(&control).await);
    }
    let first = ev(&control, Some(&base), &[], 0).await;
    assert_eq!(first["changes"].as_array().unwrap().len(), 64);
    assert_eq!(first["more"], true);
    let second = ev(&control, Some(&cursor(&first)), &[], 0).await;
    assert_eq!(second["changes"].as_array().unwrap().len(), 6);
    assert_eq!(second["more"], false);
    let mut seen = ids(&first);
    seen.extend(ids(&second));
    assert_eq!(seen, created);
    // Malformed, forged-ahead and oversize filters are invalid arguments.
    for bad in [
        raw(&control, Some("not-a-cursor"), &[], 0).await,
        raw(
            &control,
            Some(
                &EventCursor {
                    v: 1,
                    instance_id: control.store.instance_id.clone(),
                    seq: u64::MAX,
                }
                .encode(),
            ),
            &[],
            0,
        )
        .await,
        raw(&control, None, &vec!["t"; 65], 0).await,
        raw(&control, None, &[""], 0).await,
    ] {
        assert_eq!(bad.unwrap_err().code, ErrorCode::InvalidArgument);
    }
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_owner_instance_requires_resync_and_the_new_cursor_continues() {
    let (_root, owner, control, _receive, server) = fixture().await;
    let stale = EventCursor {
        v: 1,
        instance_id: "previous-owner".into(),
        seq: 3,
    }
    .encode();
    let result = ev(&control, Some(&stale), &[], 30_000).await;
    assert_eq!(result["status"], "resync_required");
    thread(&control).await;
    let next = ev(&control, Some(&cursor(&result)), &[], 0).await;
    assert_eq!(next["status"], "changed");
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_poll_ends_on_disconnect_and_on_owner_shutdown() {
    let (_root, owner, control, _receive, server) = fixture().await;
    let at = baseline(&control).await;
    let request_for = |control: &Arc<Control>| {
        Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            Command::EventsWait(EventsWaitArgs {
                after: Some(at.clone()),
                thread_ids: vec![],
                timeout_ms: 600_000,
                include_text: false,
            }),
        )
    };
    let token = CancellationToken::new();
    let pending = {
        let control = control.clone();
        let request = request_for(&control);
        let token = token.clone();
        tokio::spawn(async move { control.request_cancellable(request, token).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!pending.is_finished());
    token.cancel();
    let response = watchdog("disconnect", pending).await.unwrap().unwrap();
    assert!(!response.ok);
    // The next poll is unaffected and no receipt was kept for either.
    let held = {
        let control = control.clone();
        let request = request_for(&control);
        tokio::spawn(async move { control.request(request).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let receipts = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row(
                "SELECT (SELECT count(*) FROM operation_receipt)+(SELECT count(*) FROM app_meta WHERE key LIKE 'wait.pending.%')",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        receipts, 0,
        "events.wait must not create operation receipts"
    );
    control.shutdown().await.unwrap();
    let response = watchdog("shutdown", held).await.unwrap().unwrap();
    assert!(!response.ok, "shutdown ends the pending long-poll cleanly");
    owner.shutdown().await.unwrap();
    server.abort();
}

// ---- hint completeness: one test per mutation family ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thread_create_set_fork_rewind_prune_and_delete_advance_the_clock() {
    let (_root, owner, control, mut receive, server) = fixture().await;
    let before = baseline(&control).await;
    let t = thread(&control).await;
    advanced(&control, &before, &t).await;
    // Message + Turn so there is history to fork, rewind and prune.
    for n in 0..2 {
        send(&control, &t, &format!("task {n}")).await;
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer("done")))
            .unwrap();
        settled(&control, &t).await;
    }
    let nodes: Vec<String> = {
        let t = t.clone();
        control
            .store
            .database
            .call(move |db| {
                let mut statement = db.prepare(
                    "SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal",
                )?;
                Ok(statement
                    .query_map([t], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
            .unwrap()
    };
    assert!(nodes.len() >= 4, "{nodes:?}");

    let at = quiet(&control).await;
    command(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(t.clone()),
            title: Some("set".into()),
            ..Default::default()
        })),
    )
    .await
    .unwrap();
    advanced(&control, &at, &t).await;
    assert_eq!(
        text(&control, "SELECT title FROM thread WHERE id=?1", &t).await,
        "set"
    );

    let at = quiet(&control).await;
    let forked = command(
        &control,
        Command::ThreadFork(ThreadForkArgs {
            agents: None,
            thread_id: t.clone(),
            node_id: nodes[1].clone(),
            cwd: None,
        }),
    )
    .await
    .unwrap();
    let fork = forked["thread_id"].as_str().unwrap().to_owned();
    advanced(&control, &at, &fork).await;
    assert_eq!(
        count(&control, "SELECT count(*) FROM thread WHERE id=?1", &fork).await,
        1
    );

    let at = quiet(&control).await;
    let rewound = command(
        &control,
        Command::ThreadRewind(ThreadRewindArgs {
            thread_id: Some(t.clone()),
            node_id: nodes[1].clone(),
            exclude_node: false,
            dry_run: false,
        }),
    )
    .await
    .unwrap();
    let branch = rewound["branch_thread_id"].as_str().unwrap().to_owned();
    let seen = advanced(&control, &at, &t).await;
    // Both the rewound Thread and the preserved branch are announced.
    let mut all = ids(&seen);
    let rest = ev(&control, Some(&at), &[&branch], 0).await;
    all.extend(ids(&rest));
    assert!(all.contains(&t) && all.contains(&branch), "{all:?}");

    let at = quiet(&control).await;
    let pruned = command(
        &control,
        Command::ThreadPrune(ThreadPruneArgs {
            thread_id: Some(t.clone()),
            before_node_id: nodes[1].clone(),
            dry_run: false,
        }),
    )
    .await;
    if pruned.is_ok() {
        advanced(&control, &at, &t).await;
    } else {
        panic!("prune did not run: {pruned:?}");
    }

    let at = quiet(&control).await;
    command(
        &control,
        Command::ThreadDelete(ThreadDeleteArgs {
            thread_id: fork.clone(),
            descendants: false,
            preview_marker: None,
        }),
    )
    .await
    .unwrap();
    advanced(&control, &at, &fork).await;
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn message_run_turn_and_tool_transitions_advance_the_clock_at_each_step() {
    let (_root, owner, control, mut receive, server) = fixture().await;
    let t = thread(&control).await;
    let at = quiet(&control).await;
    // Message send: accepted message, Run and Turn are saved before the provider is asked.
    send(&control, &t, "use a tool").await;
    advanced(&control, &at, &t).await;
    let request1 = incoming(&control, &mut receive).await;
    assert_eq!(count(&control, "SELECT count(*) FROM turn WHERE thread_id=?1 AND state IN ('preparing','running','waiting')", &t).await, 1);

    // Tool start, then tool finish: each saved fact is announced after its commit.
    let at = quiet(&control).await;
    request1.reply.send((200, json!({"id":"r1","status":"completed","output":[{"type":"function_call","id":"i1","call_id":"c1","name":"shell","arguments":json!({"command":"sleep 1"}).to_string()}]}))).unwrap();
    let at = until_saved(&control, &at, &t, "SELECT count(*) FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE r.thread_id=?1").await;
    until_saved(&control, &at, &t, "SELECT count(*) FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE r.thread_id=?1 AND s.state NOT IN ('starting','running','stopping')").await;
    let request2 = incoming(&control, &mut receive).await;
    // Turn and Run completion.
    let at = quiet(&control).await;
    request2.reply.send((200, answer("finished"))).unwrap();
    advanced(&control, &at, &t).await;
    settled(&control, &t).await;
    let after = quiet(&control).await;
    assert_eq!(
        count(
            &control,
            "SELECT count(*) FROM run WHERE thread_id=?1 AND state='completed'",
            &t
        )
        .await,
        1
    );
    // Nothing further is pending.
    assert_eq!(
        ev(&control, Some(&after), &[&t], 0).await["status"],
        "timeout"
    );
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_message_cancel_and_run_stop_advance_the_clock() {
    let (_root, owner, control, mut receive, server) = fixture().await;
    let t = thread(&control).await;
    send(&control, &t, "first").await;
    let first = incoming(&control, &mut receive).await;
    let at = quiet(&control).await;
    let queued = command(
        &control,
        Command::MessageSend(MessageSendArgs {
            human: false,
            expect_reply: false,
            sender_label: None,
            thread_id: Some(t.clone()),
            text: "second".into(),
            deliver: Deliver::Complete,
        }),
    )
    .await
    .unwrap();
    advanced(&control, &at, &t).await;
    let at = quiet(&control).await;
    command(
        &control,
        Command::MessageCancel(MessageCancelArgs {
            message_ids: vec![queued["message_id"].as_str().unwrap().to_owned()],
        }),
    )
    .await
    .unwrap();
    advanced(&control, &at, &t).await;
    let run = text(&control, "SELECT id FROM run WHERE thread_id=?1", &t).await;
    let at = quiet(&control).await;
    command(&control, Command::RunStop(RunTarget { run_id: run }))
        .await
        .unwrap();
    advanced(&control, &at, &t).await;
    drop(first);
    settled(&control, &t).await;
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adopted_compaction_advances_the_clock() {
    let (_root, owner, control, mut receive, server) = fixture().await;
    let t = thread(&control).await;
    for n in 0..2 {
        send(&control, &t, &format!("task {n}")).await;
        incoming(&control, &mut receive)
            .await
            .reply
            .send((200, answer("saved answer")))
            .unwrap();
        settled(&control, &t).await;
    }
    send(&control, &t, "current task").await;
    let ordinary = incoming(&control, &mut receive).await;
    command(
        &control,
        Command::Compact(ThreadTarget {
            thread_id: Some(t.clone()),
        }),
    )
    .await
    .unwrap();
    ordinary.reply.send((200, answer("last answer"))).unwrap();
    let summary = incoming(&control, &mut receive).await;
    settled(&control, &t).await;
    let at = quiet(&control).await;
    summary
        .reply
        .send((200, answer("## goal\nSummarized")))
        .unwrap();
    until_saved(&control, &at, &t, "SELECT count(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind='summary'").await;
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachment_add_and_remove_advance_the_clock() {
    let (root, owner, control, _receive, server) = fixture().await;
    let t = thread(&control).await;
    let file = root.path().join("note.txt");
    std::fs::write(&file, "hello").unwrap();
    let at = quiet(&control).await;
    let added = command(
        &control,
        Command::AttachmentAdd(AttachmentAddArgs {
            thread_id: Some(t.clone()),
            path: file.to_string_lossy().into_owned(),
            kind: AttachmentKind::File,
        }),
    )
    .await
    .unwrap();
    advanced(&control, &at, &t).await;
    let id = added.as_array().unwrap()[0]["attachment_id"]
        .as_str()
        .or(added.as_array().unwrap()[0]["id"].as_str())
        .unwrap_or_else(|| panic!("{added}"))
        .to_owned();
    let at = quiet(&control).await;
    command(
        &control,
        Command::AttachmentRemove(AttachmentRemoveArgs {
            thread_id: Some(t.clone()),
            attachment_id: id,
        }),
    )
    .await
    .unwrap();
    advanced(&control, &at, &t).await;
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filter_of_sixty_four_returns_exactly_those_and_the_cursor_skips_other_threads() {
    let (_root, owner, control, _receive, server) = fixture().await;
    let base = baseline(&control).await;
    let mut created = Vec::new();
    for _ in 0..70 {
        created.push(thread(&control).await);
    }
    let wanted: Vec<&str> = created[..64].iter().map(String::as_str).collect();
    let result = ev(&control, Some(&base), &wanted, 0).await;
    // One entry per Thread, so a 64-ID filter can never need a second page.
    assert_eq!(ids(&result), created[..64]);
    assert_eq!(result["more"], false);
    // The cursor moved past the six unfiltered Threads: they are not reported later.
    let again = ev(&control, Some(&cursor(&result)), &[], 0).await;
    assert_eq!(again["status"], "timeout");
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_run_is_announced() {
    let (_root, owner, control, mut receive, server) = fixture().await;
    let t = thread(&control).await;
    send(&control, &t, "first").await;
    let first = incoming(&control, &mut receive).await;
    let run = text(&control, "SELECT id FROM run WHERE thread_id=?1", &t).await;
    let at = quiet(&control).await;
    command(&control, Command::RunStop(RunTarget { run_id: run }))
        .await
        .unwrap();
    // The stopping state is announced without waiting for the reply grace period.
    until_saved(
        &control,
        &at,
        &t,
        "SELECT count(*) FROM run WHERE thread_id=?1 AND state='stopping'",
    )
    .await;
    drop(first);
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reply_settlement_is_announced() {
    let (_root, owner, control, mut receive, server) = fixture().await;
    let t = thread(&control).await;
    let sent = command(
        &control,
        Command::MessageSend(MessageSendArgs {
            human: false,
            expect_reply: true,
            sender_label: None,
            thread_id: Some(t.clone()),
            text: "please reply".into(),
            deliver: Deliver::Next,
        }),
    )
    .await
    .unwrap();
    let id = sent["message_id"].as_str().unwrap().to_owned();
    let request = incoming(&control, &mut receive).await;
    // As in tests/reply.rs: mark the message admitted so that a reply is eligible.
    {
        let id = id.clone();
        control
            .store
            .database
            .call(move |db| {
                db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1", [&id])?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let at = quiet(&control).await;
    command(
        &control,
        Command::MessageReply(MessageReplyArgs {
            message_id: id.clone(),
            text: "the reply".into(),
            sender_label: None,
        }),
    )
    .await
    .unwrap();
    advanced(&control, &at, &t).await;
    request.reply.send((200, answer("done"))).unwrap();
    settled(&control, &t).await;
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_wait_and_retry_are_announced() {
    let (_root, owner, control, mut receive, server) = fixture().await;
    let t = thread(&control).await;
    let at = quiet(&control).await;
    send(&control, &t, "will fail").await;
    incoming(&control, &mut receive)
        .await
        .reply
        .send((500, json!({"error":{"message":"fixture server error"}})))
        .unwrap();
    let at = until_saved(
        &control,
        &at,
        &t,
        "SELECT count(*) FROM turn WHERE thread_id=?1 AND state='recovery_wait'",
    )
    .await;
    let run = text(&control, "SELECT id FROM run WHERE thread_id=?1", &t).await;
    command(&control, Command::RunRetry(RunTarget { run_id: run }))
        .await
        .unwrap();
    let request = incoming(&control, &mut receive).await;
    request.reply.send((200, answer("recovered"))).unwrap();
    until_saved(
        &control,
        &at,
        &t,
        "SELECT count(*) FROM turn WHERE thread_id=?1 AND state='completed'",
    )
    .await;
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_shell_exit_after_the_answer_is_announced() {
    let (_root, owner, control, mut receive, server) = fixture().await;
    let t = thread(&control).await;
    let start = quiet(&control).await;
    send(&control, &t, "start background work").await;
    incoming(&control, &mut receive).await.reply.send((200, json!({"id":"r1","status":"completed","output":[{"type":"function_call","id":"i1","call_id":"c1","name":"shell","arguments":json!({"command":"sleep 1","background":true}).to_string()}]}))).unwrap();
    incoming(&control, &mut receive)
        .await
        .reply
        .send((200, answer("started")))
        .unwrap();
    // The Turn answers while the Run stays live for the owned background shell; its exit
    // later creates a second Turn (the collector's changed() announces the committed exit).
    let at = until_saved(
        &control,
        &start,
        &t,
        "SELECT count(*) FROM turn WHERE thread_id=?1 AND state='completed'",
    )
    .await;
    until_saved(
        &control,
        &at,
        &t,
        "SELECT count(*)/2 FROM turn WHERE thread_id=?1",
    )
    .await;
    // The exit is delivered to the model as a new Turn; finish it.
    incoming(&control, &mut receive)
        .await
        .reply
        .send((200, answer("noted")))
        .unwrap();
    settled(&control, &t).await;
    close(control, owner, server).await;
}

/// F1 regression: a Turn created from queued input must be announced when `next()` commits,
/// not later at `admit_attempt`, which the provider rate gate (another Thread's 429) can delay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turn_created_from_queued_input_is_announced_while_the_rate_gate_is_held() {
    let (_root, owner, control, mut receive, server) = fixture().await;
    let a = thread(&control).await;
    let b = thread(&control).await;
    send(&control, &a, "one").await;
    let first = incoming(&control, &mut receive).await;
    command(
        &control,
        Command::MessageSend(MessageSendArgs {
            human: false,
            expect_reply: false,
            sender_label: None,
            thread_id: Some(a.clone()),
            text: "queued".into(),
            deliver: Deliver::Complete,
        }),
    )
    .await
    .unwrap();
    // Another Thread's 429 holds the shared rate gate.
    send(&control, &b, "gate").await;
    incoming(&control, &mut receive)
        .await
        .reply
        .send((429, json!({"error":{"message":"fixture rate limit"}})))
        .unwrap();
    // Wait until the 429 is processed (the gate is set before the Turn leaves running).
    watchdog("rate limit processed", async {
        while count(
            &control,
            "SELECT count(*) FROM turn WHERE thread_id=?1 AND state='recovery_wait'",
            &b,
        )
        .await
            == 0
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let at = quiet(&control).await;
    first.reply.send((200, answer("first answer"))).unwrap();
    // The gate holds the next request for tens of seconds; the Turn must be announced
    // at its commit, long before that (a guard against a delayed hint, not a latency promise).
    until_saved_within(
        &control,
        &at,
        &a,
        "SELECT count(*)/2 FROM turn WHERE thread_id=?1",
        5_000,
    )
    .await;
    drop(receive);
    close(control, owner, server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archive_unarchive_and_auto_unarchive_by_a_new_run_are_announced() {
    let (_root, owner, control, mut receive, server) = fixture().await;
    let t = thread(&control).await;
    let archive_args = || ThreadArchiveArgs {
        thread_ids: vec![t.clone()],
        descendants: false,
    };
    let archived = "SELECT count(*) FROM thread WHERE id=?1 AND archived_at IS NOT NULL";
    let at = quiet(&control).await;
    command(&control, Command::ThreadArchive(archive_args()))
        .await
        .unwrap();
    let at = until_saved(&control, &at, &t, archived).await;
    command(&control, Command::ThreadUnarchive(archive_args()))
        .await
        .unwrap();
    until_saved(
        &control,
        &at,
        &t,
        "SELECT count(*) FROM thread WHERE id=?1 AND archived_at IS NULL",
    )
    .await;
    // A new Run on an archived idle Thread unarchives it; the saved change is announced.
    let at = quiet(&control).await;
    command(&control, Command::ThreadArchive(archive_args()))
        .await
        .unwrap();
    let at = until_saved(&control, &at, &t, archived).await;
    send(&control, &t, "wake the archived thread").await;
    until_saved(
        &control,
        &at,
        &t,
        "SELECT count(*) FROM thread WHERE id=?1 AND archived_at IS NULL",
    )
    .await;
    incoming(&control, &mut receive)
        .await
        .reply
        .send((200, answer("ok")))
        .unwrap();
    settled(&control, &t).await;
    close(control, owner, server).await;
}
