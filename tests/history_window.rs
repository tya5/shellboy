//! #268: positional reads of the selected path. Synthetic Threads in temporary
//! databases only; costs are measured as SQLite VM steps, never wall-clock time.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use rusqlite::Connection;
use serde_json::json;
use shellboy::{
    history::{
        WINDOW_STATEMENTS_FOR_TESTS, append_node, rewind::rewind_thread, select_path, window,
    },
    protocol::{ErrorCode, HistoryWindowArgs, MAX_LIST_LIMIT},
    storage::DbWorker,
};

/// Run `test` on the worker's connection of a fresh temporary database that
/// holds one empty Thread `t`.
fn on_db(test: impl FnOnce(&mut Connection) + Send + 'static) {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let database = DbWorker::open(dir.path().join("window.db")).await.unwrap();
        database.call(move |db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,effort,updated_at) VALUES('t','/p','/p/AGENTS.md','proxy','fixture','custom','now')", [])?;
            test(db);
            Ok(())
        }).await.unwrap();
        database.shutdown().await.unwrap();
    });
}

fn append(db: &mut Connection, id: &str) {
    let tx = db.transaction().unwrap();
    append_node(&tx, "t", id, None, "user", &json!({"text":id}), "now").unwrap();
    tx.commit().unwrap();
}

fn fill(db: &mut Connection, count: usize) {
    for i in 0..count {
        append(db, &format!("n{i}"));
    }
}

fn args() -> HistoryWindowArgs {
    HistoryWindowArgs {
        thread_id: "t".into(),
        ordinal: None,
        node_id: None,
        expect_node_id: None,
        before: 2,
        after: 2,
        excerpt_chars: 512,
    }
}

fn ordinals(result: &shellboy::protocol::HistoryWindow) -> Vec<u64> {
    result.items.iter().map(|i| i.selection.ordinal).collect()
}

#[test]
fn end_middle_beginning_and_bounds() {
    on_db(|db| {
        fill(db, 10);
        let end = window(db, &args()).unwrap();
        assert_eq!((end.total, ordinals(&end)), (10, vec![7, 8, 9]));
        assert_eq!(end.items[2].node_id, "n9");
        let middle = window(
            db,
            &HistoryWindowArgs {
                ordinal: Some(5),
                ..args()
            },
        )
        .unwrap();
        assert_eq!(ordinals(&middle), vec![3, 4, 5, 6, 7]);
        let first = window(
            db,
            &HistoryWindowArgs {
                ordinal: Some(0),
                before: 5,
                ..args()
            },
        )
        .unwrap();
        assert_eq!(ordinals(&first), vec![0, 1, 2]);
        let by_node = window(
            db,
            &HistoryWindowArgs {
                node_id: Some("n4".into()),
                before: 0,
                after: 1,
                ..args()
            },
        )
        .unwrap();
        assert_eq!(ordinals(&by_node), vec![4, 5]);
        let last = window(
            db,
            &HistoryWindowArgs {
                ordinal: Some(9),
                after: 50,
                ..args()
            },
        )
        .unwrap();
        assert_eq!(ordinals(&last), vec![7, 8, 9]);
        let one = window(
            db,
            &HistoryWindowArgs {
                before: 0,
                after: 0,
                ..args()
            },
        )
        .unwrap();
        assert_eq!(ordinals(&one), vec![9]);
        let revision = end.revision;
        assert_eq!(revision, 10, "one selection revision per append");
        // Same item shape as history.list, including the zero-excerpt option.
        let bare = window(
            db,
            &HistoryWindowArgs {
                excerpt_chars: 0,
                ..args()
            },
        )
        .unwrap();
        assert!(bare.items.iter().all(|i| i.excerpt.is_empty()));

        let too_wide = window(
            db,
            &HistoryWindowArgs {
                before: 100,
                after: 100,
                ..args()
            },
        )
        .unwrap_err();
        assert_eq!(too_wide.code, ErrorCode::InvalidArgument);
        assert!(
            window(
                db,
                &HistoryWindowArgs {
                    before: 100,
                    after: 99,
                    ..args()
                }
            )
            .is_ok()
        );
        assert_eq!(MAX_LIST_LIMIT, 200);
        for bad in [
            HistoryWindowArgs {
                ordinal: Some(1),
                node_id: Some("n1".into()),
                ..args()
            },
            HistoryWindowArgs {
                expect_node_id: Some("n1".into()),
                ..args()
            },
        ] {
            assert_eq!(
                window(db, &bad).unwrap_err().code,
                ErrorCode::InvalidArgument
            );
        }
        let past = window(
            db,
            &HistoryWindowArgs {
                ordinal: Some(10),
                ..args()
            },
        )
        .unwrap_err();
        assert_eq!(past.code, ErrorCode::CursorExpired);
        let off = window(
            db,
            &HistoryWindowArgs {
                node_id: Some("missing".into()),
                ..args()
            },
        )
        .unwrap_err();
        assert_eq!(off.code, ErrorCode::CursorExpired);
        let missing = window(
            db,
            &HistoryWindowArgs {
                thread_id: "x".into(),
                ..args()
            },
        )
        .unwrap_err();
        assert_eq!(missing.code, ErrorCode::ThreadNotFound);
    });
}

#[test]
fn empty_path_has_zero_total_and_no_items() {
    on_db(|db| {
        let result = window(db, &args()).unwrap();
        assert_eq!((result.total, result.items.len()), (0, 0));
        assert_eq!(
            window(
                db,
                &HistoryWindowArgs {
                    ordinal: Some(0),
                    ..args()
                }
            )
            .unwrap_err()
            .code,
            ErrorCode::CursorExpired
        );
    });
}

#[test]
fn append_keeps_every_earlier_position_valid() {
    on_db(|db| {
        fill(db, 6);
        let before = window(
            db,
            &HistoryWindowArgs {
                ordinal: Some(2),
                ..args()
            },
        )
        .unwrap();
        append(db, "n6");
        append(db, "n7");
        let after = window(
            db,
            &HistoryWindowArgs {
                ordinal: Some(2),
                expect_node_id: Some("n2".into()),
                ..args()
            },
        )
        .unwrap();
        assert_eq!(after.total, 8);
        assert!(
            after.revision > before.revision,
            "revision advances on append"
        );
        let ids = |w: &shellboy::protocol::HistoryWindow| {
            w.items
                .iter()
                .map(|i| i.node_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&before), ids(&after));
        assert_eq!(ordinals(&window(db, &args()).unwrap()), vec![5, 6, 7]);
    });
}

#[test]
fn compaction_shifts_positions_and_a_stale_expectation_is_reported() {
    on_db(|db| {
        fill(db, 6);
        db.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('sum','summary','now','{\"text\":\"s\"}')", []).unwrap();
        let revision: i64 = db
            .query_row(
                "SELECT selection_revision FROM thread WHERE id='t'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let tx = db.transaction().unwrap();
        let path: Vec<String> = ["sum", "n3", "n4", "n5"].map(String::from).to_vec();
        select_path(&tx, "t", revision as u64, &path, "now").unwrap();
        tx.commit().unwrap();
        let result = window(
            db,
            &HistoryWindowArgs {
                ordinal: Some(0),
                ..args()
            },
        )
        .unwrap();
        assert_eq!((result.total, ordinals(&result)), (4, vec![0, 1, 2]));
        assert_eq!(result.items[0].node_id, "sum");
        assert!(result.revision > revision as u64);
        // The caller last saw n3 at position 3; it moved to 1 and position 3 is now n5.
        let error = window(
            db,
            &HistoryWindowArgs {
                ordinal: Some(3),
                expect_node_id: Some("n3".into()),
                ..args()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::CursorExpired);
        assert_eq!(error.details.as_ref().unwrap()["total"], 4);
        let error = window(
            db,
            &HistoryWindowArgs {
                ordinal: Some(5),
                ..args()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::CursorExpired);
        // Node anchors follow the node to its new position.
        let moved = window(
            db,
            &HistoryWindowArgs {
                node_id: Some("n3".into()),
                before: 0,
                after: 0,
                ..args()
            },
        )
        .unwrap();
        assert_eq!(ordinals(&moved), vec![1]);
        // Ordinals stay contiguous from 0.
        let (min, max, count): (i64, i64, i64) = db
            .query_row(
                "SELECT min(ordinal),max(ordinal),count(*) FROM thread_path WHERE thread_id='t'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((min, max + 1, count), (0, 4, 4));
    });
}

#[test]
fn rewind_shortens_the_path_and_old_tail_positions_expire() {
    on_db(|db| {
        fill(db, 8);
        let seen = window(db, &args()).unwrap();
        assert_eq!(seen.total, 8);
        let tx = db.transaction().unwrap();
        rewind_thread(&tx, "t", "n3", false, false, "branch", "later").unwrap();
        tx.commit().unwrap();
        let now = window(db, &args()).unwrap();
        assert_eq!((now.total, ordinals(&now)), (4, vec![1, 2, 3]));
        assert!(now.revision > seen.revision);
        let error = window(
            db,
            &HistoryWindowArgs {
                ordinal: Some(7),
                expect_node_id: Some("n7".into()),
                ..args()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::CursorExpired);
        assert_eq!(error.details.as_ref().unwrap()["revision"], now.revision);
        let gone = window(
            db,
            &HistoryWindowArgs {
                node_id: Some("n7".into()),
                ..args()
            },
        )
        .unwrap_err();
        assert_eq!(gone.code, ErrorCode::CursorExpired);
    });
}

fn plan(db: &Connection, sql: &str) -> String {
    let probe: Vec<rusqlite::types::Value> = (1..=sql.matches('?').count().max(4))
        .map(|i| {
            if i == 1 {
                "t".to_owned().into()
            } else {
                1_i64.into()
            }
        })
        .collect();
    let probe = &probe[..sql_params(sql)];
    db.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(rusqlite::params_from_iter(probe), |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join(" | ")
}
fn sql_params(sql: &str) -> usize {
    (1..=9).filter(|i| sql.contains(&format!("?{i}"))).count()
}

#[test]
fn every_statement_is_a_search_on_the_primary_key_or_unique_index() {
    on_db(|db| {
        fill(db, 3);
        for sql in WINDOW_STATEMENTS_FOR_TESTS {
            let plan = plan(db, sql);
            assert!(plan.contains("SEARCH"), "{sql}\n{plan}");
            assert!(!plan.contains("SCAN"), "{sql}\n{plan}");
            assert!(!plan.contains("TEMP B-TREE"), "{sql}\n{plan}");
        }
        let total = plan(db, WINDOW_STATEMENTS_FOR_TESTS[0]);
        assert!(
            total.contains("sqlite_autoindex_thread_path_1") || total.contains("PRIMARY KEY"),
            "{total}"
        );
        let ranged = plan(db, WINDOW_STATEMENTS_FOR_TESTS[3]);
        assert!(
            ranged.contains("ordinal>? AND ordinal<?") || ranged.contains("ordinal>"),
            "{ranged}"
        );
    });
}

/// Add synthetic nodes and path rows for ordinals `from..to` (no per-row writer calls).
fn synthetic(db: &Connection, from: i64, to: i64) {
    db.execute(
        "WITH RECURSIVE s(i) AS (SELECT ?1 UNION ALL SELECT i+1 FROM s WHERE i<?2-1) INSERT INTO node(id,kind,created_at,payload_json) SELECT 'n'||i,'user','now','{\"text\":\"x\"}' FROM s",
        [from, to],
    ).unwrap();
    db.execute(
        "WITH RECURSIVE s(i) AS (SELECT ?1 UNION ALL SELECT i+1 FROM s WHERE i<?2-1) INSERT INTO thread_path(thread_id,ordinal,node_id) SELECT 't',i,'n'||i FROM s",
        [from, to],
    ).unwrap();
    db.execute(
        "UPDATE thread SET cursor_node_id=?1,selection_revision=1",
        [format!("n{}", to - 1)],
    )
    .unwrap();
}

/// SQLite VM instructions executed by one `window` call (progress handler, period 1).
fn steps(db: &Connection, request: &HistoryWindowArgs) -> u64 {
    let counter = Arc::new(AtomicU64::new(0));
    let seen = counter.clone();
    let _ = db.progress_handler(
        1,
        Some(move || {
            seen.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    window(db, request).unwrap();
    let _ = db.progress_handler(1, None::<fn() -> bool>);
    counter.load(Ordering::Relaxed)
}

#[test]
fn read_cost_does_not_grow_with_path_length() {
    on_db(|db| {
        let measure = |db: &Connection, count: i64| {
            let wide = HistoryWindowArgs {
                before: 20,
                after: 20,
                ..args()
            };
            [
                steps(db, &wide),
                steps(
                    db,
                    &HistoryWindowArgs {
                        ordinal: Some(count as u64 / 2),
                        ..wide.clone()
                    },
                ),
                steps(
                    db,
                    &HistoryWindowArgs {
                        node_id: Some(format!("n{}", count / 2)),
                        ..wide.clone()
                    },
                ),
                steps(
                    db,
                    &HistoryWindowArgs {
                        ordinal: Some(0),
                        ..wide
                    },
                ),
            ]
        };
        synthetic(db, 0, 100);
        let small = measure(db, 100);
        synthetic(db, 100, 100_000);
        let large = measure(db, 100_000);
        eprintln!(
            "history_window vm_steps [end, middle ordinal, middle node, start] len=100: {small:?} len=100000: {large:?}"
        );
        // Same window sizes at the same anchor kinds: cost may differ by index depth
        // only (a few steps), never by the 1000x path length.
        for (a, b) in small.iter().zip(&large) {
            assert!(*b <= a + a / 10 + 50, "cost grew with length: {a} -> {b}");
        }
    });
}

#[test]
fn spill_tool_sources_are_identical_in_list_and_window_even_without_excerpts() {
    on_db(|db| {
        let payload = json!({"output":[],"spill_version":1,"spill_tools":{"status":"known","items":[{"call_id":"c","execution_id":"e","original_node_id":"source"}],"truncated":false}});
        let tx = db.transaction().unwrap();
        append_node(&tx, "t", "spill", None, "spill", &payload, "now").unwrap();
        append_node(
            &tx,
            "t",
            "legacy",
            None,
            "spill",
            &json!({"output":[],"spill_version":1}),
            "now",
        )
        .unwrap();
        tx.commit().unwrap();
        let mut list = shellboy::protocol::HistoryListArgs {
            thread_id: "t".into(),
            excerpt_chars: 0,
            limit: 200,
            cursor: None,
            run_id: None,
            since: None,
            state: None,
        };
        list.excerpt_chars = 0;
        let listed = shellboy::history::list_nodes(db, &list).unwrap();
        let mut request = args();
        request.excerpt_chars = 0;
        let positional = window(db, &request).unwrap();
        assert_eq!(
            serde_json::to_value(&listed.items).unwrap(),
            serde_json::to_value(&positional.items).unwrap()
        );
        let sources = listed.items[0].spill_tools.as_ref().unwrap();
        assert_eq!(sources.items[0].call_id, "c");
        assert_eq!(sources.items[0].execution_id.as_deref(), Some("e"));
        assert_eq!(
            listed.items[1].spill_tools.as_ref().unwrap().status,
            "unknown"
        );
        assert_eq!(listed.items[0].execution_count, 0);
        assert!(listed.items[0].executions.is_empty());
    });
}

#[test]
fn spill_tool_sources_window_vm_steps_do_not_grow_with_sixteen_mib_payload() {
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    on_db(|db| {
        let tx=db.transaction().unwrap();
        append_node(&tx,"t","spill",None,"spill",&json!({"output":[],"spill_tools":{"status":"known","items":[{"call_id":"c"}],"truncated":false}}),"now").unwrap();
        tx.commit().unwrap();
        let steps=Arc::new(AtomicUsize::new(0));let observed=steps.clone();
        db.progress_handler(1,Some(move ||{observed.fetch_add(1,Ordering::Relaxed);false}));
        let mut request=args();request.excerpt_chars=0;
        let mut measured=Vec::new();
        for size in [16,16*1024*1024] {
            db.execute("UPDATE node SET payload_json=?1 WHERE id='spill'",[json!({"output":"x".repeat(size)}).to_string()]).unwrap();
            steps.store(0,Ordering::Relaxed);
            let page=window(db,&request).unwrap();
            measured.push(steps.load(Ordering::Relaxed));
            assert_eq!(page.items[0].spill_tools.as_ref().unwrap().items[0].call_id,"c");
        }
        db.progress_handler(0,None::<fn()->bool>);
        assert_eq!(measured[0],measured[1],"{measured:?}");
        assert!(measured[0]<1000,"{measured:?}");
    });
}
