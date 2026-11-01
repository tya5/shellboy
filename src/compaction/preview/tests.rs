use super::*;
use crate::compaction::edit::tests::Fixture;
use clap::Parser;

fn config() -> Config {
    let mut config = Config::default();
    config.connections.insert("p".into(), Default::default());
    config
}
fn args(
    action: Option<ContextEditAction>,
    from: Option<&str>,
    to: Option<&str>,
) -> CompactPreviewArgs {
    CompactPreviewArgs {
        thread_id: Some("t".into()),
        action,
        from_node_id: from.map(str::to_owned),
        to_node_id: to.map(str::to_owned),
        ..Default::default()
    }
}
fn forecast(f: &Fixture, args: &CompactPreviewArgs) -> Value {
    forecast_config(f, args, &config()).unwrap()
}
fn forecast_config(
    f: &Fixture,
    args: &CompactPreviewArgs,
    config: &Config,
) -> Result<Value, StorageError> {
    let mut db =
        Connection::open_with_flags(f.db.path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.pragma_update(None, "query_only", true)?;
    let changes = db.total_changes();
    let tx = db.transaction()?;
    let raw = read_tx(&tx, "t", args, config)?;
    tx.commit()?;
    assert_eq!(db.total_changes(), changes);
    drop(db);
    Ok(prepare(raw))
}
async fn plain() -> Fixture {
    let f = Fixture::new().await;
    f.text("a", "alpha").await;
    f.append("b", None, json!({"role":"assistant","content":"beta"}))
        .await;
    f.text("c", "current").await;
    f
}
async fn counts(f: &Fixture) -> Vec<i64> {
    f.db.call(|db| {
        [
            "SELECT count(*) FROM node",
            "SELECT count(*) FROM replacement",
            "SELECT count(*) FROM replacement_source",
            "SELECT count(*) FROM thread_path",
            "SELECT selection_revision FROM thread WHERE id='t'",
            "SELECT count(*) FROM app_meta",
            "SELECT count(*) FROM model_attempt",
            "SELECT count(*) FROM operation_receipt",
        ]
        .iter()
        .map(|sql| db.query_row(sql, [], |r| r.get(0)))
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn trim_and_omit_match_actual_projection_and_leave_store_untouched() {
    for action in [ContextEditAction::Trim, ContextEditAction::Omit] {
        let f = plain().await;
        let args = args(
            Some(action),
            if action == ContextEditAction::Omit {
                Some("a")
            } else {
                None
            },
            Some("b"),
        );
        let before = counts(&f).await;
        let path = f.path().await;
        let predicted = forecast(&f, &args);
        assert!(predicted["predicted_failure"].is_null(), "{predicted}");
        assert_eq!(predicted["target_nodes"]["count"], 2);
        assert_eq!(predicted["target_nodes"]["by_kind"]["message"], 2);
        assert_eq!(
            predicted["removed_original_messages"],
            json!({"user":1,"assistant":1})
        );
        assert_eq!(counts(&f).await, before);
        assert_eq!(f.path().await, path);
        let result = f
            .compactor
            .edit(
                "t",
                &crate::protocol::ContextEditArgs {
                    thread_id: args.thread_id,
                    action,
                    from_node_id: args.from_node_id,
                    to_node_id: args.to_node_id,
                    max_chars: None,
                    max_percent: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(predicted["before_characters"], result.before_characters);
        assert_eq!(predicted["after_characters"], result.after_characters);
        assert_eq!(predicted["target_nodes"]["count"], result.omitted_nodes);
        f.close().await;
    }
}
#[tokio::test]
async fn maximal_and_no_target_match_actual_projection() {
    for texts in [true, false] {
        let f = if texts {
            plain().await
        } else {
            Fixture::new().await
        };
        let args = args(Some(ContextEditAction::Maximal), None, None);
        let before = counts(&f).await;
        let predicted = forecast(&f, &args);
        assert_eq!(counts(&f).await, before);
        let result = f
            .compactor
            .edit(
                "t",
                &crate::protocol::ContextEditArgs {
                    thread_id: Some("t".into()),
                    action: ContextEditAction::Maximal,
                    from_node_id: None,
                    to_node_id: None,
                    max_chars: None,
                    max_percent: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(predicted["before_characters"], result.before_characters);
        assert_eq!(predicted["after_characters"], result.after_characters);
        assert_eq!(predicted["target_nodes"]["count"], result.omitted_nodes);
        assert_eq!(predicted["no_target"], result.compact.no_target);
        f.close().await;
    }
}
#[tokio::test]
async fn spill_matches_real_completed_tool_group_adoption() {
    let f = plain().await;
    f.tools().await;
    let predicted = forecast(
        &f,
        &args(
            Some(ContextEditAction::Spill),
            Some("g-old-call"),
            Some("g-old-result"),
        ),
    );
    assert!(predicted["predicted_failure"].is_null(), "{predicted}");
    let result = f
        .compactor
        .edit(
            "t",
            &crate::protocol::ContextEditArgs {
                thread_id: Some("t".into()),
                action: ContextEditAction::Spill,
                from_node_id: Some("g-old-call".into()),
                to_node_id: Some("g-old-result".into()),
                max_chars: None,
                max_percent: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        predicted["target_nodes"]["count"],
        result.compact.source_nodes
    );
    assert_eq!(predicted["spilled_groups"], result.compact.spilled_groups);
    assert_eq!(predicted["before_characters"], result.before_characters);
    assert_eq!(predicted["after_characters"], result.after_characters);
    f.close().await;
}
#[tokio::test]
async fn default_applies_virtual_spill_before_summary_and_does_not_request_model() {
    let f = plain().await;
    f.tools().await;
    f.text("tail", "latest request").await;
    let before = counts(&f).await;
    let p = forecast(&f, &args(None, None, None));
    assert!(p["predicted_failure"].is_null(), "{p}");
    assert_eq!(p["spilled_groups"], 1);
    assert!(p["after_characters"].is_null());
    assert!(p["summary"]["segments"].as_u64().unwrap() > 0);
    assert_eq!(p["provider_requests"], 0);
    assert_eq!(counts(&f).await, before);
    let original = f.compactor.snapshot("t", true).await.unwrap();
    let virtual_spill = spill(&original, policy::spill_ranges(&original, false))
        .unwrap()
        .0;
    f.compactor.spill("t", false).await.unwrap();
    let actual = f.compactor.snapshot("t", true).await.unwrap();
    assert_eq!(
        edit::history_characters(&virtual_spill),
        edit::history_characters(&actual)
    );
    assert_eq!(
        policy::summary_range(&virtual_spill, false),
        policy::summary_range(&actual, false)
    );
    f.close().await;
}
#[tokio::test]
async fn summary_forecasts_real_indivisible_refusal_without_http_or_adoption() {
    let f = Fixture::new().await;
    f.text("large", &"x".repeat(70_000)).await;
    f.text("tail", "current").await;
    let before = counts(&f).await;
    let p = forecast(
        &f,
        &args(
            Some(ContextEditAction::Summary),
            Some("large"),
            Some("large"),
        ),
    );
    assert!(
        p["predicted_failure"]["message"]
            .as_str()
            .unwrap()
            .contains("indivisible"),
        "{p}"
    );
    assert_eq!(p["target_nodes"]["count"], 1);
    assert_eq!(p["after_characters"], p["before_characters"]);
    assert_eq!(counts(&f).await, before);
    f.close().await;
}
#[tokio::test]
async fn large_context_trim_provenance_does_not_reintroduce_issue112() {
    let f = Fixture::new().await;
    f.text("a", "start").await;
    f.append("marker",None,json!({"context_trim":{"original_order":["z".repeat(90_000)]},"source_node_ids":["z".repeat(90_000)],"item":{"role":"developer","content":[{"type":"input_text","text":"Shellboy context notice"}]}})).await;
    f.text("c", "end").await;
    f.text("tail", "current").await;
    let p = forecast(
        &f,
        &args(Some(ContextEditAction::Summary), Some("a"), Some("c")),
    );
    assert!(p["predicted_failure"].is_null(), "{p}");
    assert!(p["summary"]["source_characters"].as_u64().unwrap() < 1000);
    assert!(p["after_characters"].is_null());
    f.close().await;
}
#[tokio::test]
async fn protected_current_and_incomplete_groups_forecast_refusal_not_empty_success() {
    let f = plain().await;
    f.tools().await;
    f.db.call(|db| {
        db.execute("UPDATE tool_group SET state='running' WHERE id='g-old'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    let p = forecast(
        &f,
        &args(
            Some(ContextEditAction::Spill),
            Some("g-old-call"),
            Some("g-old-result"),
        ),
    );
    assert!(
        p["predicted_failure"]["message"]
            .as_str()
            .unwrap()
            .contains("incomplete")
    );
    assert_eq!(p["no_target"], false);
    let p = forecast(
        &f,
        &args(
            Some(ContextEditAction::Omit),
            Some("g-new-result"),
            Some("g-new-result"),
        ),
    );
    assert!(p["predicted_failure"].is_object());
    f.close().await;
}
#[tokio::test]
async fn live_turn_ancestry_matches_canonical_resolver_after_spill() {
    let f = plain().await;
    f.tools().await;
    f.compactor.spill("t", false).await.unwrap();
    let canonical = f.compactor.snapshot("t", true).await.unwrap();
    let db = Connection::open(f.db.path()).unwrap();
    let mut budget = TEXT;
    let mut sql = 0;
    let loaded = load(&db, "t", &args(None, None, None), &mut budget, &mut sql).unwrap();
    assert_eq!(loaded.snapshot.turn_boundaries, canonical.turn_boundaries);
    assert_eq!(
        policy::summary_range(&loaded.snapshot, false),
        policy::summary_range(&canonical, false)
    );
    drop(db);
    f.close().await;
}
#[tokio::test]
async fn payload_and_turn_coverage_limits_are_unknown_not_false_empty() {
    let f = plain().await;
    let payload = json!({"role":"user","content":"x".repeat(PAYLOAD+1)}).to_string();
    f.db.call(move |db| {
        db.execute("UPDATE node SET payload_json=?1 WHERE id='a'", [payload])?;
        Ok(())
    })
    .await
    .unwrap();
    let e = forecast_config(&f, &args(None, None, None), &config())
        .err()
        .unwrap();
    assert!(e.to_string().contains("coverage"));
    f.close().await;
    let f = plain().await;
    f.db.call(|db|{db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');WITH RECURSIVE seq(n)AS(VALUES(0)UNION ALL SELECT n+1 FROM seq WHERE n<512)INSERT INTO turn(id,thread_id,run_id,state,created_at)SELECT 'u'||n,'t','r','completed','now' FROM seq;")?;Ok(())}).await.unwrap();
    assert!(forecast_config(&f, &args(None, None, None), &config()).is_err());
    f.close().await;
}
#[tokio::test]
async fn query_only_exact_statement_count_and_indexed_plans() {
    let f = plain().await;
    let db = Connection::open(f.db.path()).unwrap();
    db.pragma_update(None, "query_only", true).unwrap();
    let trace = crate::frontend::sql_trace::Statements::new(&db);
    let raw = read_tx(
        &db,
        "t",
        &args(Some(ContextEditAction::Maximal), None, None),
        &config(),
    )
    .unwrap();
    assert_eq!(trace.count(), raw.sql);
    assert!(raw.sql <= SQL_MAX);
    drop(trace);
    for (query, parameter, index) in [
        (PATH_SQL, "t", "sqlite_autoindex_thread_path_1"),
        (GROUP_SQL, "[]", "sqlite_autoindex_tool_group_1"),
        (ANCESTRY_SQL, "[]", "replacement_output_node"),
    ] {
        let mut statement = db.prepare(&format!("EXPLAIN QUERY PLAN {query}")).unwrap();
        let mut rows = if query == ANCESTRY_SQL {
            statement.query(params![parameter, ROWS as i64]).unwrap()
        } else {
            statement.query([parameter]).unwrap()
        };
        let mut plan = String::new();
        while let Some(row) = rows.next().unwrap() {
            plan.push_str(&row.get::<_, String>(3).unwrap());
        }
        assert!(plan.contains(index), "{plan}");
    }
    drop(db);
    f.close().await;
}
#[tokio::test]
async fn summary_budget_and_stopping_are_forecast_errors() {
    let f = Fixture::new().await;
    f.text("a", &"a".repeat(40_000)).await;
    f.text("b", &"b".repeat(40_000)).await;
    f.text("tail", "current").await;
    let mut c = config();
    c.compaction.summary_map_max_requests = 1;
    let p = forecast_config(
        &f,
        &args(Some(ContextEditAction::Summary), Some("a"), Some("b")),
        &c,
    )
    .unwrap();
    assert!(
        p["predicted_failure"]["message"]
            .as_str()
            .unwrap()
            .contains("budget")
    );
    assert_eq!(p["after_characters"], p["before_characters"]);
    f.db.call(|db| {
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at)VALUES('stopping','t','stopping','now')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let p = forecast(&f, &args(Some(ContextEditAction::Maximal), None, None));
    assert!(
        p["predicted_failure"]["message"]
            .as_str()
            .unwrap()
            .contains("stopping")
    );
    f.close().await;
}
#[tokio::test]
async fn cli_modes_map_to_read_only_preview_and_reject_restore_or_list() {
    let cases = [
        vec![],
        vec!["--maximal"],
        vec!["--trim", "--to", "b"],
        vec!["--omit", "--from", "a", "--to", "b"],
        vec!["--spill", "--from", "a", "--to", "b"],
        vec![
            "--summary",
            "--from",
            "a",
            "--to",
            "b",
            "--max-chars",
            "4000",
        ],
    ];
    for flags in cases {
        let cli = crate::cli::Cli::try_parse_from(
            ["shellboy", "compact", "--dry-run", "--thread", "t"]
                .into_iter()
                .chain(flags),
        )
        .unwrap();
        let command = cli
            .into_command_with_stdin(std::path::Path::new("."), &mut &b""[..])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(command.name(), "compact.preview");
        assert!(command.is_read_only());
        assert!(!command.requires_receipt());
        assert!(!command.changes_context());
    }
    assert!(
        crate::cli::Cli::try_parse_from(["shellboy", "compact", "--dry-run", "--restore"]).is_err()
    );
    let cli =
        crate::cli::Cli::try_parse_from(["shellboy", "compact", "--dry-run", "list"]).unwrap();
    assert!(
        cli.into_command_with_stdin(std::path::Path::new("."), &mut &b""[..])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn summary_source_count_and_cached_preflight_match_real_loopback_execution() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
            assert!(request.len() < 1_048_576);
            if let Some(i) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&request[..i]);
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|s| s.trim().parse().unwrap())
                    })
                    .unwrap();
                if request.len() >= i + 4 + length {
                    break;
                }
            }
        }
        let body=json!({"id":"summary-fixture","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"short summary"}]}],"usage":{"input_tokens":20,"output_tokens":3,"total_tokens":23}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        1usize
    });
    let mut f = Fixture::new().await;
    f.text("a", &"a".repeat(5000)).await;
    f.text("b", &"b".repeat(5000)).await;
    f.text("tail", "current").await;
    let mut c = config();
    let connection = c.connections.get_mut("p").unwrap();
    connection.base_url = format!("http://{address}/v1");
    connection.api_key_env = None;
    connection.response_max_retries = 0;
    let p = forecast_config(
        &f,
        &args(Some(ContextEditAction::Summary), Some("a"), Some("b")),
        &c,
    )
    .unwrap();
    assert_eq!(p["summary"]["uncached_segments"], 1);
    assert!(p["after_characters"].is_null());
    f.compactor = Compactor::new(f.db.clone(), Arc::new(c));
    let result = f
        .compactor
        .edit(
            "t",
            &crate::protocol::ContextEditArgs {
                thread_id: Some("t".into()),
                action: ContextEditAction::Summary,
                from_node_id: Some("a".into()),
                to_node_id: Some("b".into()),
                max_chars: None,
                max_percent: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(p["target_nodes"]["count"], result.compact.source_nodes);
    assert_eq!(p["before_characters"], result.before_characters);
    assert_eq!(
        p["summary"]["source_characters"],
        result.compact.source_characters.unwrap()
    );
    assert_eq!(server.await.unwrap(), 1);
    f.close().await;
}

#[tokio::test]
async fn hidden_marker_after_multi_source_tool_groups_uses_group_not_node_ordinal() {
    let f = Fixture::new().await;
    f.tools().await;
    f.text("x", "original x").await;
    f.text("y", "original y").await;
    f.text("z", "current").await;
    let result = f
        .compactor
        .edit(
            "t",
            &crate::protocol::ContextEditArgs {
                thread_id: Some("t".into()),
                action: ContextEditAction::Omit,
                from_node_id: Some("x".into()),
                to_node_id: Some("y".into()),
                max_chars: None,
                max_percent: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let marker = result.marker_node_id.unwrap();
    let p = forecast(
        &f,
        &args(
            Some(ContextEditAction::Summary),
            Some(&marker),
            Some(&marker),
        ),
    );
    assert!(p["predicted_failure"].is_null(), "{p}");
    assert_eq!(p["target_nodes"]["count"], 2);
    assert!(p["after_characters"].is_null());
    f.close().await;
}
