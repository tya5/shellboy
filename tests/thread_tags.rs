//! Thread tags and the cwd filter (#222 stage 3): organization metadata only.
//! Many tags per Thread, AND filtering with paging in every archived mode, never
//! a change to `updated_at`, deleted with their Thread, replay-safe by operation ID.
use serde_json::{Value, json};
use shellboy::{config::Config, control::db::Store, protocol::*, storage::DbWorker};
use std::sync::Arc;
use tempfile::TempDir;

async fn store() -> (TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open(directory.path().join("db")).await.unwrap();
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
    let cwd = directory.path().join("project");
    (
        directory,
        Store::new(database, Arc::new(config), cwd, "instance".into()),
    )
}
fn request(command: Command) -> Request {
    Request::new(format!("instance:{}", uuid::Uuid::new_v4()), command)
}
async fn run(store: &Store, command: Command) -> Result<Value, ApiError> {
    store
        .execute(request(command), None)
        .await
        .map(|result| result.result)
}
async fn create_in(store: &Store, parent: Option<&str>, cwd: Option<&str>) -> String {
    let value = run(
        store,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: None,
            cwd: cwd.map(str::to_owned),
            model: None,
            effort: None,
            parent_id: parent.map(str::to_owned),
        }),
    )
    .await
    .unwrap();
    value["thread_id"].as_str().unwrap().to_owned()
}
async fn create(store: &Store) -> String {
    create_in(store, None, None).await
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}
fn tag_command(ids: &[&str], add: &[&str], remove: &[&str]) -> Command {
    Command::ThreadTag(ThreadTagArgs {
        thread_ids: strings(ids),
        add: strings(add),
        remove: strings(remove),
    })
}
async fn tag(
    store: &Store,
    ids: &[&str],
    add: &[&str],
    remove: &[&str],
) -> Result<Value, ApiError> {
    run(store, tag_command(ids, add, remove)).await
}
/// Fixed, distinct update times so list order is checkable; the last is newest.
async fn stamp(store: &Store, ids: &[String]) {
    let ids = ids.to_vec();
    store
        .database
        .call(move |db| {
            for (i, id) in ids.iter().enumerate() {
                db.execute(
                    "UPDATE thread SET updated_at=?2 WHERE id=?1",
                    rusqlite::params![id, format!("2026-11-01T00:00:{i:02}Z")],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
}
async fn list_with(
    store: &Store,
    tags: &[&str],
    cwd: Option<&str>,
    archived: ArchivedFilter,
    limit: u32,
    cursor: Option<String>,
) -> Result<Page<Value>, ApiError> {
    run(
        store,
        Command::ThreadList(ThreadListArgs {
            limit,
            cursor,
            query: None,
            archived,
            tags: strings(tags),
            cwd: cwd.map(str::to_owned),
        }),
    )
    .await
    .map(|value| serde_json::from_value(value).unwrap())
}
async fn list(store: &Store, tags: &[&str]) -> Vec<String> {
    ids(
        &list_with(store, tags, None, ArchivedFilter::Exclude, 50, None)
            .await
            .unwrap(),
    )
}
fn ids(page: &Page<Value>) -> Vec<String> {
    page.items
        .iter()
        .map(|item| item["thread_id"].as_str().unwrap().to_owned())
        .collect()
}
async fn names(store: &Store, limit: u32, cursor: Option<String>) -> Page<String> {
    serde_json::from_value(
        run(store, Command::ThreadTags(ThreadTagsArgs { limit, cursor }))
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn text(store: &Store, id: &str, sql: &'static str) -> String {
    let id = id.to_owned();
    store
        .database
        .call(move |db| {
            Ok(db.query_row(sql, [id], |row| {
                row.get::<_, rusqlite::types::Value>(0)
                    .map(|v| format!("{v:?}"))
            })?)
        })
        .await
        .unwrap()
}
async fn revision(store: &Store, id: &str) -> String {
    text(store, id, "SELECT revision FROM thread WHERE id=?1").await
}
async fn stored(store: &Store, id: &str) -> Vec<String> {
    let id = id.to_owned();
    store
        .database
        .call(move |db| {
            Ok(db
                .prepare("SELECT tag FROM thread_tag WHERE thread_id=?1 ORDER BY tag")?
                .query_map([id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn tagging_changes_only_the_tags_and_never_updated_at() {
    let (_dir, store) = store().await;
    let a = create(&store).await;
    stamp(&store, std::slice::from_ref(&a)).await;
    let before = revision(&store, &a).await;
    let outcome = store
        .execute(
            request(tag_command(&[&a], &["  customer A ", "bug hunt"], &[])),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        outcome.result,
        json!({"changed":[a],"unchanged":[],"not_found":[],"add":["customer A","bug hunt"],"remove":[]})
    );
    assert_eq!(outcome.changed_threads, std::slice::from_ref(&a));
    assert_eq!(stored(&store, &a).await, ["bug hunt", "customer A"]);
    assert_eq!(
        text(&store, &a, "SELECT updated_at FROM thread WHERE id=?1").await,
        "Text(\"2026-11-01T00:00:00Z\")",
        "organization changes must not move the list order"
    );
    assert_ne!(revision(&store, &a).await, before);
    // Adding what is there and removing what is not there change nothing.
    let after = revision(&store, &a).await;
    let same = store
        .execute(
            request(tag_command(&[&a], &["bug hunt"], &["absent"])),
            None,
        )
        .await
        .unwrap();
    assert_eq!(same.result["unchanged"], json!([a]));
    assert_eq!(same.result["changed"], json!([]));
    assert!(same.changed_threads.is_empty());
    assert_eq!(revision(&store, &a).await, after);
    // Add and remove in one call.
    tag(&store, &[&a], &["x"], &["bug hunt"]).await.unwrap();
    assert_eq!(stored(&store, &a).await, ["customer A", "x"]);
    // Tags are case sensitive.
    tag(&store, &[&a], &["X"], &[]).await.unwrap();
    assert_eq!(stored(&store, &a).await, ["X", "customer A", "x"]);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn tag_text_is_validated_like_a_title_and_bounded() {
    let (_dir, store) = store().await;
    let a = create(&store).await;
    let bad = |result: Result<Value, ApiError>, code: ErrorCode| {
        assert_eq!(result.unwrap_err().code, code);
    };
    bad(
        tag(&store, &[&a], &["   "], &[]).await,
        ErrorCode::InvalidArgument,
    );
    bad(
        tag(&store, &[&a], &["two\nlines"], &[]).await,
        ErrorCode::InvalidArgument,
    );
    bad(
        tag(&store, &[&a], &["tab\there"], &[]).await,
        ErrorCode::InvalidArgument,
    );
    bad(
        tag(&store, &[&a], &["sep\u{2028}arator"], &[]).await,
        ErrorCode::InvalidArgument,
    );
    bad(
        tag(&store, &[&a], &[], &[]).await,
        ErrorCode::InvalidArgument,
    );
    bad(
        tag(&store, &[&a], &["same"], &[" same "]).await,
        ErrorCode::InvalidArgument,
    );
    // 128 Unicode scalars are accepted (multi-byte ones count once); 129 are refused.
    let longest: String = "あ".repeat(128);
    tag(&store, &[&a], &[&longest], &[]).await.unwrap();
    assert_eq!(stored(&store, &a).await, std::slice::from_ref(&longest));
    bad(
        tag(&store, &[&a], &[&"あ".repeat(129)], &[]).await,
        ErrorCode::InvalidArgument,
    );
    // Call bounds: 200 IDs, 64 tags to add, 64 to remove, 64 filters.
    let ids: Vec<String> = (0..201).map(|n| format!("t{n}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    bad(tag(&store, &refs, &["x"], &[]).await, ErrorCode::InputLimit);
    let many: Vec<String> = (0..65).map(|n| format!("tag{n}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    bad(tag(&store, &[&a], &many, &[]).await, ErrorCode::InputLimit);
    bad(tag(&store, &[&a], &[], &many).await, ErrorCode::InputLimit);
    bad(
        list_with(&store, &many, None, ArchivedFilter::Exclude, 5, None)
            .await
            .map(|_| Value::Null),
        ErrorCode::InputLimit,
    );
    bad(
        list_with(&store, &["  "], None, ArchivedFilter::Exclude, 5, None)
            .await
            .map(|_| Value::Null),
        ErrorCode::InvalidArgument,
    );
    // Exactly 200 IDs are accepted (missing ones are reported, not an error while one exists).
    let mut exact: Vec<&str> = vec![a.as_str()];
    let filler: Vec<String> = (0..199).map(|n| format!("missing{n}")).collect();
    exact.extend(filler.iter().map(String::as_str));
    let result = tag(&store, &exact, &["y"], &[]).await.unwrap();
    assert_eq!(result["changed"], json!([a]));
    assert_eq!(result["not_found"].as_array().unwrap().len(), 199);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_thread_carries_at_most_64_tags_and_a_refused_call_changes_nothing() {
    let (_dir, store) = store().await;
    let (a, b) = (create(&store).await, create(&store).await);
    let first: Vec<String> = (0..64).map(|n| format!("t{n:02}")).collect();
    let first: Vec<&str> = first.iter().map(String::as_str).collect();
    tag(&store, &[&a], &first, &[]).await.unwrap();
    assert_eq!(stored(&store, &a).await.len(), 64);
    let before = revision(&store, &a).await;
    // b is fine, a would reach 65: the whole call is refused and b stays untagged.
    let error = tag(&store, &[&b, &a], &["one-more"], &[])
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    assert!(stored(&store, &b).await.is_empty());
    assert_eq!(stored(&store, &a).await.len(), 64);
    assert_eq!(revision(&store, &a).await, before);
    // Removing in the same call makes room.
    tag(&store, &[&a], &["one-more"], &["t00"]).await.unwrap();
    assert_eq!(stored(&store, &a).await.len(), 64);
    // Adding an existing tag at the limit is not growth.
    let again = tag(&store, &[&a], &["t01"], &[]).await.unwrap();
    assert_eq!(again["unchanged"], json!([a]));
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_deleted_and_default_targets() {
    let (_dir, store) = store().await;
    let (a, b) = (create(&store).await, create(&store).await);
    let mixed = tag(&store, &[&a, "missing", &a], &["x"], &[])
        .await
        .unwrap();
    assert_eq!(mixed["changed"], json!([a]), "a repeated ID is one Thread");
    assert_eq!(mixed["not_found"], json!(["missing"]));
    assert_eq!(
        tag(&store, &["missing"], &["x"], &[])
            .await
            .unwrap_err()
            .code,
        ErrorCode::ThreadNotFound
    );
    run(
        &store,
        Command::ThreadDelete(ThreadDeleteArgs {
            preview_marker: None,
            thread_id: b.clone(),
            descendants: false,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        tag(&store, &[&b], &["x"], &[]).await.unwrap_err().code,
        ErrorCode::ThreadNotFound
    );
    // No ID and no calling Thread: the same refusal as archive.
    assert!(tag(&store, &[], &["x"], &[]).await.is_err());
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_same_operation_id_replays_without_a_second_change() {
    let (_dir, store) = store().await;
    let a = create(&store).await;
    let op = request(tag_command(&[&a], &["x"], &[]));
    let first = store.execute(op.clone(), None).await.unwrap();
    assert_eq!(first.changed_threads, std::slice::from_ref(&a));
    // A later change must not be undone or redone by the replay.
    tag(&store, &[&a], &[], &["x"]).await.unwrap();
    let revision_after = revision(&store, &a).await;
    let replay = store.execute(op, None).await.unwrap();
    assert_eq!(replay.result, first.result);
    assert!(
        replay.changed_threads.is_empty(),
        "a replay publishes nothing"
    );
    assert!(stored(&store, &a).await.is_empty());
    assert_eq!(revision(&store, &a).await, revision_after);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_filter_is_and_with_paging_in_every_archived_mode_and_cursors_are_bound() {
    let (_dir, store) = store().await;
    let mut all = Vec::new();
    for _ in 0..8 {
        all.push(create(&store).await);
    }
    stamp(&store, &all).await; // all[7] is newest
    // a: 0..=6, b: even, c: 0 and 4
    for (i, id) in all.iter().enumerate() {
        if i < 7 {
            tag(&store, &[id], &["a"], &[]).await.unwrap();
        }
        if i % 2 == 0 {
            tag(&store, &[id], &["b"], &[]).await.unwrap();
        }
    }
    tag(&store, &[&all[0], &all[4]], &["c"], &[]).await.unwrap();
    let pick =
        |indexes: &[usize]| -> Vec<String> { indexes.iter().map(|i| all[*i].clone()).collect() };
    assert_eq!(list(&store, &["a"]).await, pick(&[6, 5, 4, 3, 2, 1, 0]));
    assert_eq!(list(&store, &["a", "b"]).await, pick(&[6, 4, 2, 0]));
    assert_eq!(
        list(&store, &["b", "a"]).await,
        pick(&[6, 4, 2, 0]),
        "AND is symmetric"
    );
    assert_eq!(list(&store, &["a", "b", "c"]).await, pick(&[4, 0]));
    assert_eq!(
        list(&store, &["a", "a", " b "]).await,
        pick(&[6, 4, 2, 0]),
        "repeats and spaces are normalized"
    );
    assert!(list(&store, &["nobody"]).await.is_empty());
    assert!(list(&store, &["c", "nobody"]).await.is_empty());
    // Items carry their tags; an untagged Thread has no field at all.
    let page = list_with(&store, &["c"], None, ArchivedFilter::Exclude, 50, None)
        .await
        .unwrap();
    assert_eq!(page.items[0]["tags"], json!(["a", "b", "c"]));
    let untagged = list_with(&store, &[], None, ArchivedFilter::Exclude, 50, None)
        .await
        .unwrap();
    assert!(untagged.items[0].get("tags").is_none());
    assert_eq!(untagged.items[1]["tags"], json!(["a", "b"]));
    // Paging does not skip or repeat.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = list_with(&store, &["a"], None, ArchivedFilter::Exclude, 2, cursor)
            .await
            .unwrap();
        assert!(page.items.len() <= 2);
        seen.extend(ids(&page));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, pick(&[6, 5, 4, 3, 2, 1, 0]));
    // Archive modes: 2 and 5 are archived.
    run(
        &store,
        Command::ThreadArchive(ThreadArchiveArgs {
            thread_ids: vec![all[2].clone(), all[5].clone()],
            descendants: false,
        }),
    )
    .await
    .unwrap();
    for (mode, expected) in [
        (ArchivedFilter::Exclude, pick(&[6, 4, 3, 1, 0])),
        (ArchivedFilter::Only, pick(&[5, 2])),
        (ArchivedFilter::Include, pick(&[6, 5, 4, 3, 2, 1, 0])),
    ] {
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = list_with(&store, &["a"], None, mode, 2, cursor)
                .await
                .unwrap();
            seen.extend(ids(&page));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen, expected, "{mode:?}");
    }
    let and_only = list_with(&store, &["a", "b"], None, ArchivedFilter::Only, 50, None)
        .await
        .unwrap();
    assert_eq!(ids(&and_only), pick(&[2]));
    // A cursor belongs to its tags, cwd and archived mode.
    let first = list_with(&store, &["a"], None, ArchivedFilter::Exclude, 1, None)
        .await
        .unwrap();
    let cursor = first.next_cursor.unwrap();
    for (tags, cwd, mode) in [
        (vec!["a", "b"], None, ArchivedFilter::Exclude),
        (vec!["b"], None, ArchivedFilter::Exclude),
        (vec![], None, ArchivedFilter::Exclude),
        (vec!["a"], Some("/x"), ArchivedFilter::Exclude),
        (vec!["a"], None, ArchivedFilter::Include),
    ] {
        let error = list_with(&store, &tags, cwd, mode, 1, Some(cursor.clone()))
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::CursorExpired,
            "{tags:?} {cwd:?} {mode:?}"
        );
    }
    // The same filter in another tag order is the same query.
    let ab = list_with(&store, &["a", "b"], None, ArchivedFilter::Exclude, 1, None)
        .await
        .unwrap();
    list_with(
        &store,
        &["b", "a"],
        None,
        ArchivedFilter::Exclude,
        1,
        ab.next_cursor,
    )
    .await
    .unwrap();
    // Tagging does not move the order.
    assert_eq!(list(&store, &[]).await[0], all[7]);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_cwd_filter_is_exact_and_combines_with_tags_and_the_text_query() {
    let (dir, store) = store().await;
    let (one, two) = (
        dir.path().join("one").to_string_lossy().into_owned(),
        dir.path().join("two").to_string_lossy().into_owned(),
    );
    let a = create_in(&store, None, Some(&one)).await;
    let b = create_in(&store, None, Some(&one)).await;
    let c = create_in(&store, None, Some(&two)).await;
    stamp(&store, &[a.clone(), b.clone(), c.clone()]).await;
    tag(&store, &[&a, &c], &["x"], &[]).await.unwrap();
    let in_cwd = |cwd: &str, tags: &'static [&'static str]| {
        let cwd = cwd.to_owned();
        let store = &store;
        async move {
            ids(
                &list_with(store, tags, Some(&cwd), ArchivedFilter::Exclude, 50, None)
                    .await
                    .unwrap(),
            )
        }
    };
    assert_eq!(in_cwd(&one, &[]).await, [b.clone(), a.clone()]);
    assert_eq!(in_cwd(&two, &[]).await, std::slice::from_ref(&c));
    assert_eq!(in_cwd(&one, &["x"]).await, std::slice::from_ref(&a));
    assert!(in_cwd("/no/such", &[]).await.is_empty());
    // Lexical cleanup like saved cwds: trailing slash, `.` and `..` find the same Threads.
    assert_eq!(
        in_cwd(&format!("{one}/"), &[]).await,
        [b.clone(), a.clone()]
    );
    assert_eq!(
        in_cwd(&format!("{one}/./sub/.."), &[]).await,
        [b.clone(), a.clone()]
    );
    // A relative or empty path is refused, never silently empty.
    for bad in ["../one", "one", "", "."] {
        let error = list_with(&store, &[], Some(bad), ArchivedFilter::Exclude, 5, None)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{bad:?}");
    }
    // A prefix is not a match.
    assert!(in_cwd(&dir.path().to_string_lossy(), &[]).await.is_empty());
    // The text query still narrows inside the SQL filter.
    let page = run(
        &store,
        Command::ThreadList(ThreadListArgs {
            limit: 10,
            cursor: None,
            query: Some(a.clone()),
            archived: ArchivedFilter::Exclude,
            tags: vec![],
            cwd: Some(one.clone()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    // The cwd page walks its archived modes too.
    run(
        &store,
        Command::ThreadArchive(ThreadArchiveArgs {
            thread_ids: vec![b.clone()],
            descendants: false,
        }),
    )
    .await
    .unwrap();
    assert_eq!(in_cwd(&one, &[]).await, std::slice::from_ref(&a));
    let only = list_with(&store, &[], Some(&one), ArchivedFilter::Only, 50, None)
        .await
        .unwrap();
    assert_eq!(ids(&only), [b]);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_get_lists_tags_and_new_threads_do_not_inherit_them() {
    let (_dir, store) = store().await;
    let parent = create(&store).await;
    tag(&store, &[&parent], &["zeta", "alpha"], &[])
        .await
        .unwrap();
    let details = run(
        &store,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(parent.clone()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(details["tags"], json!(["alpha", "zeta"]));
    let child = create_in(&store, Some(&parent), None).await;
    assert!(stored(&store, &child).await.is_empty());
    let child_details = run(
        &store,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(child),
        }),
    )
    .await
    .unwrap();
    assert!(child_details.get("tags").is_none());
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn deleting_a_thread_removes_its_tags_in_the_same_transaction() {
    let (_dir, store) = store().await;
    let root = create(&store).await;
    let child = create_in(&store, Some(&root), None).await;
    let keep = create(&store).await;
    tag(&store, &[&root, &child], &["gone", "shared"], &[])
        .await
        .unwrap();
    tag(&store, &[&keep], &["shared"], &[]).await.unwrap();
    assert_eq!(names(&store, 50, None).await.items, ["gone", "shared"]);
    run(
        &store,
        Command::ThreadDelete(ThreadDeleteArgs {
            preview_marker: None,
            thread_id: root.clone(),
            descendants: true,
        }),
    )
    .await
    .unwrap();
    assert!(stored(&store, &root).await.is_empty());
    assert!(stored(&store, &child).await.is_empty());
    assert_eq!(stored(&store, &keep).await, ["shared"]);
    assert_eq!(names(&store, 50, None).await.items, ["shared"]);
    assert_eq!(list(&store, &["shared"]).await, [keep]);
    assert!(list(&store, &["gone"]).await.is_empty());
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn tag_names_are_distinct_ordered_by_bytes_and_paged() {
    let (_dir, store) = store().await;
    let (a, b) = (create(&store).await, create(&store).await);
    assert!(names(&store, 50, None).await.items.is_empty());
    tag(&store, &[&a, &b], &["b", "a", "B", "日本"], &[])
        .await
        .unwrap();
    tag(&store, &[&b], &["c", "a"], &[]).await.unwrap();
    let all = names(&store, 50, None).await;
    assert_eq!(
        all.items,
        ["B", "a", "b", "c", "日本"],
        "byte order, each name once"
    );
    assert!(all.next_cursor.is_none());
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = names(&store, 2, cursor).await;
        assert!(page.items.len() <= 2);
        seen.extend(page.items);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, ["B", "a", "b", "c", "日本"]);
    // Removing the last user of a name removes the name.
    tag(&store, &[&a, &b], &[], &["c", "B"]).await.unwrap();
    assert_eq!(names(&store, 50, None).await.items, ["a", "b", "日本"]);
    // Bounds and cursor scope.
    for limit in [0, 201] {
        let error = run(
            &store,
            Command::ThreadTags(ThreadTagsArgs {
                limit,
                cursor: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }
    let first = names(&store, 1, None).await;
    let error = run(
        &store,
        Command::ThreadList(ThreadListArgs {
            limit: 1,
            cursor: first.next_cursor,
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::CursorExpired);
    // 200 names fit in one page; one more makes a second.
    for i in 0..4 {
        let t = create(&store).await;
        let batch: Vec<String> = (0..64).map(|n| format!("m{i}-{n:03}")).collect();
        let batch: Vec<&str> = batch.iter().map(String::as_str).collect();
        tag(&store, &[&t], &batch, &[]).await.unwrap();
    }
    let page = names(&store, 200, None).await;
    assert_eq!(page.items.len(), 200);
    assert!(page.next_cursor.is_some());
    store.database.shutdown().await.unwrap();
}

async fn cwds(store: &Store, limit: u32, cursor: Option<String>) -> Result<Page<String>, ApiError> {
    run(store, Command::ThreadCwds(ThreadCwdsArgs { limit, cursor }))
        .await
        .map(|value| serde_json::from_value(value).unwrap())
}

#[tokio::test]
async fn cwd_names_are_distinct_ordered_paged_and_include_archived_but_not_deleted() {
    let (dir, store) = store().await;
    let at = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
    assert!(cwds(&store, 50, None).await.unwrap().items.is_empty());
    let (b1, b2) = (
        create_in(&store, None, Some(&at("b"))).await,
        create_in(&store, None, Some(&at("b"))).await,
    );
    let a = create_in(&store, None, Some(&at("a"))).await;
    let c = create_in(&store, None, Some(&at("c"))).await;
    assert_eq!(
        cwds(&store, 50, None).await.unwrap().items,
        [at("a"), at("b"), at("c")]
    );
    // Paging by one does not skip or repeat.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = cwds(&store, 1, cursor).await.unwrap();
        seen.extend(page.items);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, [at("a"), at("b"), at("c")]);
    // A directory holding only archived Threads is still listed.
    run(
        &store,
        Command::ThreadArchive(ThreadArchiveArgs {
            thread_ids: vec![a.clone()],
            descendants: false,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        cwds(&store, 50, None).await.unwrap().items,
        [at("a"), at("b"), at("c")]
    );
    // A directory disappears with its last non-deleted Thread.
    let delete = |id: &str| {
        run(
            &store,
            Command::ThreadDelete(ThreadDeleteArgs {
                preview_marker: None,
                thread_id: id.to_owned(),
                descendants: false,
            }),
        )
    };
    delete(&c).await.unwrap();
    delete(&b1).await.unwrap();
    assert_eq!(
        cwds(&store, 50, None).await.unwrap().items,
        [at("a"), at("b")]
    );
    delete(&b2).await.unwrap();
    assert_eq!(cwds(&store, 50, None).await.unwrap().items, [at("a")]);
    // Bounds and cursor scope.
    for limit in [0, 201] {
        assert_eq!(
            cwds(&store, limit, None).await.unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
    tag(&store, &[&a], &["x", "y"], &[]).await.unwrap();
    let tag_page = names(&store, 1, None).await;
    assert_eq!(
        cwds(&store, 1, tag_page.next_cursor)
            .await
            .unwrap_err()
            .code,
        ErrorCode::CursorExpired
    );
    store.database.shutdown().await.unwrap();
}
