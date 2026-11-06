use super::*;
use rusqlite::params;
use serde_json::json;

fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    db
}
fn thread(db: &Connection, id: &str, parent: Option<&str>, title: &str) {
    db.execute(
        "INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,?3,'/','','p','m','2026-11-01T00:00:00Z')",
        params![id, parent, title],
    )
    .unwrap();
}
fn node(db: &Connection, id: &str, kind: &str, at: &str, text: &str) {
    let payload = json!({"role": if kind == "user" {"user"} else {"assistant"},
        "content":[{"type": if kind == "user" {"input_text"} else {"output_text"}, "text": text}]});
    db.execute(
        "INSERT OR IGNORE INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,?2,?3,?4,?5)",
        params![id, kind, at, payload.to_string(), payload.to_string().len() as i64],
    )
    .unwrap();
}
/// Nodes named `u:*` are user requests, everything else is an answer.
fn path(db: &Connection, thread: &str, nodes: &[&str]) {
    for (ordinal, id) in nodes.iter().enumerate() {
        let kind = if id.starts_with("u:") {
            "user"
        } else {
            "assistant"
        };
        node(
            db,
            id,
            kind,
            &format!("2026-11-01T00:{ordinal:02}:00Z"),
            &format!("text of {id}"),
        );
        db.execute(
            "INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES(?1,?2,?3)",
            params![thread, ordinal as i64, id],
        )
        .unwrap();
    }
}
fn path_extend(db: &Connection, thread: &str, id: &str) {
    node(
        db,
        id,
        "user",
        "2026-11-01T09:00:00Z",
        &format!("text of {id}"),
    );
    db.execute(
        "INSERT INTO thread_path(thread_id,ordinal,node_id)SELECT ?1,COALESCE(MAX(ordinal)+1,0),?2 FROM thread_path WHERE thread_id=?1",
        params![thread, id],
    )
    .unwrap();
}
fn args(limit: u32, cursor: Option<String>) -> ThreadBranchesArgs {
    ThreadBranchesArgs {
        thread_id: None,
        limit,
        cursor,
    }
}
fn read(db: &Connection, id: &str) -> Value {
    branches(db, id, &args(50, None)).unwrap()
}
fn find<'a>(value: &'a Value, id: &str) -> &'a Value {
    value["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["thread_id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from {value}"))
}

#[test]
fn fork_child_diverges_at_the_last_shared_node_and_parent_continues() {
    let db = db();
    thread(&db, "root", None, "Root");
    thread(&db, "child", Some("root"), "");
    path(&db, "root", &["u:1", "a:1", "u:2", "a:2", "u:3", "a:3"]);
    path(
        &db,
        "child",
        &["u:1", "a:1", "u:2", "a:2", "u:x", "a:x", "u:y"],
    );
    let value = read(&db, "root");
    assert_eq!(value["root_thread_id"], "root");
    let root = find(&value, "root");
    assert_eq!(root["state"], "current");
    assert!(root["divergence"].is_null());
    assert_eq!(root["size"]["path_nodes"], 6);
    assert_eq!(root["size"]["bytes"], Value::Null);
    assert_eq!(root["tip"]["last_request"]["text"], "text of u:3");
    let child = find(&value, "child");
    assert_eq!(child["state"], "branch");
    assert_eq!(child["parent_thread_id"], "root");
    assert_eq!(child["divergence"]["node_id"], "a:2");
    assert_eq!(child["divergence"]["ordinal"], 3);
    assert_eq!(child["divergence"]["parent_continues"], true);
    assert_eq!(child["divergence"]["request"]["text"], "text of u:2");
    assert_eq!(child["size"]["nodes_after_divergence"], 3);
    assert_eq!(child["tip"]["last_request"]["node_id"], "u:y");
    assert!(child["title"].is_null());
}

#[test]
fn rewound_parent_is_a_prefix_so_the_child_extends_it() {
    let db = db();
    thread(&db, "t", None, "T");
    thread(&db, "cut", Some("t"), "T");
    path(&db, "t", &["u:1", "a:1"]);
    path(&db, "cut", &["u:1", "a:1", "u:2", "a:2"]);
    let value = read(&db, "t");
    let cut = find(&value, "cut");
    assert_eq!(cut["divergence"]["node_id"], "a:1");
    assert_eq!(cut["divergence"]["parent_continues"], false);
    assert_eq!(cut["size"]["nodes_after_divergence"], 2);
}

#[test]
fn nested_forks_and_ancestors_are_found_from_any_member() {
    let db = db();
    thread(&db, "r", None, "R");
    thread(&db, "c", Some("r"), "C");
    thread(&db, "g", Some("c"), "G");
    thread(&db, "s", Some("r"), "S");
    path(&db, "r", &["u:1", "a:1", "u:2", "a:2"]);
    path(&db, "c", &["u:1", "a:1", "u:c", "a:c", "u:c2", "a:c2"]);
    path(&db, "g", &["u:1", "a:1", "u:c", "a:c", "u:g", "a:g"]);
    path(&db, "s", &["u:1", "a:1", "u:2", "a:2", "u:s"]);
    let from_grandchild = read(&db, "g");
    assert_eq!(from_grandchild["root_thread_id"], "r");
    assert_eq!(from_grandchild["family_threads"], 4);
    assert_eq!(find(&from_grandchild, "g")["state"], "current");
    assert_eq!(find(&from_grandchild, "c")["state"], "ancestor");
    assert_eq!(find(&from_grandchild, "r")["state"], "ancestor");
    assert_eq!(find(&from_grandchild, "s")["state"], "branch");
    assert_eq!(find(&from_grandchild, "g")["depth"], 2);
    assert_eq!(find(&from_grandchild, "g")["divergence"]["node_id"], "a:c");
    assert_eq!(find(&from_grandchild, "c")["divergence"]["node_id"], "a:1");
    assert_eq!(find(&from_grandchild, "s")["divergence"]["node_id"], "a:2");
    // The chain comes first, so a parent always precedes its children.
    let order: Vec<&str> = from_grandchild["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["thread_id"].as_str().unwrap())
        .collect();
    assert_eq!(&order[..3], ["r", "c", "g"]);
}

#[test]
fn deleted_parent_roots_its_own_family_and_deleted_children_are_skipped() {
    let db = db();
    thread(&db, "gone", None, "Gone");
    thread(&db, "orphan", Some("gone"), "Orphan");
    thread(&db, "kid", Some("orphan"), "Kid");
    thread(&db, "dead", Some("orphan"), "Dead");
    path(&db, "orphan", &["u:1", "a:1"]);
    path(&db, "kid", &["u:1", "a:1", "u:k"]);
    path(&db, "dead", &["u:1", "a:1", "u:d"]);
    db.execute(
        "UPDATE thread SET deleted_at='x' WHERE id IN ('gone','dead')",
        [],
    )
    .unwrap();
    let value = read(&db, "kid");
    assert_eq!(value["root_thread_id"], "orphan");
    assert_eq!(value["root_parent_unavailable"], true);
    assert_eq!(value["family_threads"], 2);
    let orphan = find(&value, "orphan");
    assert!(orphan["parent_thread_id"].is_null());
    assert!(orphan["divergence"].is_null());
    assert!(
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["thread_id"] != "dead")
    );
    assert_eq!(
        branches(&db, "gone", &args(50, None)).unwrap_err().code,
        ErrorCode::ThreadNotFound
    );
}

#[test]
fn paths_with_no_shared_node_report_why_there_is_no_divergence() {
    let db = db();
    thread(&db, "p", None, "P");
    thread(&db, "c", Some("p"), "C");
    path(&db, "p", &["u:1", "a:1"]);
    path(&db, "c", &["u:9", "a:9"]);
    let value = read(&db, "p");
    let c = find(&value, "c");
    assert!(c["divergence"].is_null());
    assert_eq!(c["divergence_unknown"], "no_shared_prefix");
    assert_eq!(c["size"]["path_nodes"], 2);
}

#[test]
fn state_flags_show_running_and_pruned_originals() {
    let db = db();
    thread(&db, "p", None, "P");
    thread(&db, "c", Some("p"), "C");
    path(&db, "p", &["u:1", "a:1"]);
    path(&db, "c", &["u:1", "a:1", "u:2"]);
    db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('r','c','live','2026-11-01T00:00:00Z')", []).unwrap();
    db.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES('rep','a:1','k','1','1','pruned','2026-11-01T00:00:00Z')", []).unwrap();
    db.execute(
        "INSERT INTO thread_replacement(thread_id,replacement_id)VALUES('p','rep')",
        [],
    )
    .unwrap();
    let value = read(&db, "p");
    assert_eq!(find(&value, "c")["running"], true);
    assert_eq!(find(&value, "p")["running"], false);
    assert_eq!(find(&value, "p")["pruned_originals"], true);
    assert_eq!(find(&value, "c")["pruned_originals"], false);
}

#[test]
fn excerpts_are_single_line_bounded_and_skip_oversized_or_collected_payloads() {
    let db = db();
    thread(&db, "t", None, "T");
    path(&db, "t", &["u:1", "a:1"]);
    let long = format!("line one\nline two {}", "x".repeat(400));
    node(&db, "u:long", "user", "2026-11-01T01:00:00Z", &long);
    db.execute(
        "INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('t',2,'u:long')",
        [],
    )
    .unwrap();
    let value = read(&db, "t");
    let request = &find(&value, "t")["tip"]["last_request"];
    let text = request["text"].as_str().unwrap();
    assert!(text.starts_with("line one line two xxx"));
    assert_eq!(text.chars().count(), EXCERPT_CHARS);
    assert_eq!(request["truncated"], true);
    db.execute("UPDATE node SET payload_json=NULL WHERE id='u:long'", [])
        .unwrap();
    let value = read(&db, "t");
    let request = &find(&value, "t")["tip"]["last_request"];
    assert_eq!(request["node_id"], "u:long");
    assert!(request["text"].is_null());
    db.execute(
        "UPDATE node SET payload_json='{}',payload_bytes=?1 WHERE id='u:long'",
        [PAYLOAD_BYTES + 1],
    )
    .unwrap();
    assert!(find(&read(&db, "t"), "t")["tip"]["last_request"]["text"].is_null());
}

#[test]
fn last_request_search_stops_after_the_scan_bound() {
    let db = db();
    thread(&db, "t", None, "T");
    let mut nodes = vec!["u:first".to_owned()];
    nodes.extend((0..SCAN_ROWS + 5).map(|i| format!("a:{i}")));
    let refs: Vec<&str> = nodes.iter().map(String::as_str).collect();
    path(&db, "t", &refs);
    let value = read(&db, "t");
    assert!(find(&value, "t")["tip"]["last_request"].is_null());
    assert_eq!(find(&value, "t")["size"]["path_nodes"], SCAN_ROWS + 6);
}

#[test]
fn pages_are_bounded_stable_and_expire_when_the_tree_changes() {
    let db = db();
    thread(&db, "r", None, "R");
    path(&db, "r", &["u:1", "a:1"]);
    for i in 0..5 {
        let id = format!("c{i}");
        thread(&db, &id, Some("r"), &id);
        path(&db, &id, &["u:1", "a:1", &format!("u:{id}")]);
    }
    let first = branches(&db, "r", &args(2, None)).unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["family_threads"], 6);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let second = branches(&db, "r", &args(2, Some(cursor.clone()))).unwrap();
    let third = branches(
        &db,
        "r",
        &args(2, second["next_cursor"].as_str().map(str::to_owned)),
    )
    .unwrap();
    assert!(third["next_cursor"].is_null());
    let all: Vec<&str> = [&first, &second, &third]
        .iter()
        .flat_map(|page| page["items"].as_array().unwrap())
        .map(|i| i["thread_id"].as_str().unwrap())
        .collect();
    assert_eq!(all, ["r", "c0", "c1", "c2", "c3", "c4"]);
    // A different Thread cannot reuse the cursor, and a changed tree expires it.
    assert_eq!(
        branches(&db, "c0", &args(2, Some(cursor.clone())))
            .unwrap_err()
            .code,
        ErrorCode::CursorExpired
    );
    // An append in any Thread bumps root_revision but does not change the tree.
    db.execute(
        "UPDATE app_meta SET value='1' WHERE key='root_revision'",
        [],
    )
    .unwrap();
    path_extend(&db, "c4", "u:later");
    assert!(branches(&db, "r", &args(2, Some(cursor.clone()))).is_ok());
    // A created Thread changes the tree and expires the cursor.
    thread(&db, "c5", Some("r"), "c5");
    assert_eq!(
        branches(&db, "r", &args(2, Some(cursor))).unwrap_err().code,
        ErrorCode::CursorExpired
    );
    assert_eq!(
        branches(&db, "r", &args(0, None)).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        branches(&db, "r", &args(MAX_LIST_LIMIT + 1, None))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn a_wide_family_is_capped_but_keeps_the_requested_thread_and_its_ancestors() {
    let db = db();
    thread(&db, "r", None, "R");
    path(&db, "r", &["u:1"]);
    for i in 0..FAMILY_LIMIT + 20 {
        let id = format!("c{i:04}");
        thread(&db, &id, Some("r"), "");
    }
    // The last (in id order) child is beyond the cap yet must stay listed.
    thread(&db, "zz", Some("r"), "");
    thread(&db, "zz-child", Some("zz"), "");
    let value = read(&db, "zz-child");
    assert_eq!(value["family_truncated"], true);
    assert_eq!(value["family_threads"], FAMILY_LIMIT);
    let ids: Vec<&str> = value["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["thread_id"].as_str().unwrap())
        .collect();
    assert_eq!(&ids[..3], ["r", "zz", "zz-child"]);
    assert_eq!(value["limits"]["family_threads"], FAMILY_LIMIT);
}

#[test]
fn a_deep_chain_stops_following_ancestors_at_the_bound() {
    let db = db();
    let mut parent: Option<String> = None;
    for i in 0..ANCESTOR_LIMIT + 8 {
        let id = format!("d{i:03}");
        thread(&db, &id, parent.as_deref(), "");
        parent = Some(id);
    }
    let deepest = parent.unwrap();
    let value = read(&db, &deepest);
    assert_eq!(value["ancestors_truncated"], true);
    assert_eq!(value["family_threads"], ANCESTOR_LIMIT + 1);
    assert_eq!(find(&value, &deepest)["state"], "current");
}

#[test]
fn cyclic_parents_terminate() {
    let db = db();
    db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    thread(&db, "a", Some("b"), "A");
    thread(&db, "b", Some("a"), "B");
    let value = read(&db, "a");
    assert!(value["family_threads"].as_u64().unwrap() <= 2);
}

#[test]
fn markers_group_alternatives_per_divergence_point_and_follow_the_viewed_lineage() {
    let db = db();
    thread(&db, "r", None, "R");
    thread(&db, "c", Some("r"), "C");
    thread(&db, "d", Some("r"), "D");
    path(&db, "r", &["u:1", "a:1", "u:2", "a:2"]);
    path(&db, "c", &["u:1", "a:1", "u:c", "a:c"]);
    path(&db, "d", &["u:1", "a:1", "u:d", "a:d"]);
    let nodes = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let on_r = markers(&db, "r", &nodes(&["u:1", "a:1", "u:2"])).unwrap();
    assert_eq!(on_r.len(), 1);
    assert_eq!(on_r[0].anchor_node_id, "a:1");
    assert_eq!(on_r[0].alternatives.len(), 3);
    let followed = &on_r[0].alternatives[on_r[0].current];
    assert_eq!(followed.thread_id, "r");
    assert_eq!(followed.excerpt.as_deref(), Some("text of u:2"));
    let on_c = markers(&db, "c", &nodes(&["u:1", "a:1", "u:c"])).unwrap();
    assert_eq!(on_c[0].alternatives[on_c[0].current].thread_id, "c");
    // Only anchors on the loaded page get a marker.
    assert!(markers(&db, "c", &nodes(&["u:c"])).unwrap().is_empty());
    // A Thread with no relatives reads nothing further.
    thread(&db, "lonely", None, "L");
    path(&db, "lonely", &["u:1", "a:1"]);
    assert!(markers(&db, "lonely", &nodes(&["a:1"])).unwrap().is_empty());
}

#[test]
fn a_rewound_thread_shows_itself_ending_at_the_point_and_the_cut_part() {
    let db = db();
    thread(&db, "t", None, "T");
    thread(&db, "cut", Some("t"), "T");
    path(&db, "t", &["u:1", "a:1"]);
    path(&db, "cut", &["u:1", "a:1", "u:2", "a:2"]);
    let marks = markers(&db, "t", &["a:1".to_string()]).unwrap();
    assert_eq!(marks.len(), 1);
    let ids: Vec<&str> = marks[0]
        .alternatives
        .iter()
        .map(|a| a.thread_id.as_str())
        .collect();
    // The Thread that ends here sorts before the part that continues.
    assert_eq!(ids, ["t", "cut"]);
    assert_eq!(marks[0].current, 0);
}

#[test]
fn bisection_uses_logarithmic_lookups_on_a_long_path() {
    let db = db();
    thread(&db, "p", None, "P");
    thread(&db, "c", Some("p"), "C");
    let shared: Vec<String> = (0..4000).map(|i| format!("n{i}")).collect();
    let refs: Vec<&str> = shared.iter().map(String::as_str).collect();
    path(&db, "p", &refs);
    let mut child = shared[..3000].to_vec();
    child.push("own".into());
    let refs: Vec<&str> = child.iter().map(String::as_str).collect();
    path(&db, "c", &refs);
    let value = read(&db, "p");
    assert_eq!(find(&value, "c")["divergence"]["ordinal"], 2999);
    assert_eq!(find(&value, "c")["divergence"]["parent_continues"], true);
}

#[test]
fn the_followed_alternative_shows_the_viewed_threads_own_last_request() {
    let db = db();
    thread(&db, "p", None, "P");
    thread(&db, "s", Some("p"), "S");
    thread(&db, "t", Some("p"), "T");
    path(&db, "p", &["u:1", "a:1", "u:2", "a:2", "u:p-late"]);
    path(&db, "s", &["u:1", "a:1", "u:s", "a:s"]);
    path(&db, "t", &["u:1", "a:1", "u:2", "a:2", "u:t-own"]);
    let marks = markers(&db, "t", &["a:1".to_string(), "a:2".to_string()]).unwrap();
    let first = marks.iter().find(|m| m.anchor_node_id == "a:1").unwrap();
    // At a:1 P and S diverge; T follows P's shared prefix, so P is the followed alternative.
    let followed = &first.alternatives[first.current];
    assert_eq!(followed.thread_id, "p");
    assert_eq!(followed.excerpt.as_deref(), Some("text of u:t-own"));
    let other = first
        .alternatives
        .iter()
        .find(|a| a.thread_id == "s")
        .unwrap();
    assert_eq!(other.excerpt.as_deref(), Some("text of u:s"));
}

#[test]
fn markers_ignore_points_that_are_not_on_the_page() {
    let db = db();
    thread(&db, "r", None, "R");
    thread(&db, "c", Some("r"), "C");
    path(&db, "r", &["u:1", "a:1", "u:2"]);
    path(&db, "c", &["u:1", "a:1", "u:c"]);
    // Full items read the run table; a page without a point must not need it.
    db.execute_batch("DROP TABLE run").unwrap();
    assert!(markers(&db, "r", &["u:2".to_string()]).unwrap().is_empty());
    assert!(markers(&db, "r", &["a:1".to_string()]).is_err());
}
