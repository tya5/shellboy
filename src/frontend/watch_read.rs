//! Visible Watch metadata and bounded witnesses in the caller's read snapshot.
use crate::ui::{
    live_work::{LiveWorkRow, LiveWorkSnapshot, LiveWorkTarget},
    watch::{PreparedWatchDetail, WatchAge, WatchAgeOrigin, WatchDetail},
};
#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;
#[cfg(test)]
use {
    crate::{
        control::db::thread_summary,
        protocol::{ApiError, ErrorCode},
        storage::StorageError,
    },
    rusqlite::Connection,
};

#[cfg(test)]
pub(super) struct RawWatch {
    summaries: Vec<thread_summary::RawSummary>,
    extras: BTreeMap<String, crate::control::db::thread_evidence::Extra>,
    parents: super::header::RawTitles,
    observed_at: Instant,
    wall_at: chrono::DateTime<chrono::Utc>,
}
#[cfg(test)]
pub(super) fn read_tx(db: &Connection, ids: &[String]) -> Result<RawWatch, StorageError> {
    read_shared_tx(db, ids, &[], &BTreeSet::new())
}

/// Reuse only facts observed by this caller in the same read transaction. The
/// shared parent IDs are promises to supply their prepared E titles later.
#[cfg(test)]
pub(super) fn read_shared_tx(
    db: &Connection,
    ids: &[String],
    known_summaries: &[thread_summary::RawSummary],
    known_parent_title_ids: &BTreeSet<String>,
) -> Result<RawWatch, StorageError> {
    if ids.len() > 128 || known_summaries.len() > 128 || known_parent_title_ids.len() > 128 {
        return Err(ApiError::new(ErrorCode::InputLimit, "Watch scope exceeds128 IDs").into());
    }
    if ids.is_empty() {
        return Ok(RawWatch {
            summaries: Vec::new(),
            extras: BTreeMap::new(),
            parents: super::header::read_titles_tx(db, &[])?,
            observed_at: Instant::now(),
            wall_at: chrono::Utc::now(),
        });
    }
    let known = known_summaries
        .iter()
        .map(|summary| (summary.thread_id.as_str(), summary))
        .collect::<BTreeMap<_, _>>();
    let missing = ids
        .iter()
        .filter(|id| !known.contains_key(id.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut observed = thread_summary::read_ids_tx(db, &missing)?
        .into_iter()
        .map(|summary| (summary.thread_id.clone(), summary))
        .collect::<BTreeMap<_, _>>();
    let mut summaries = Vec::new();
    let mut selected = BTreeSet::new();
    for id in ids {
        if !selected.insert(id) {
            continue;
        }
        if let Some(summary) = known.get(id.as_str()) {
            summaries.push((**summary).clone());
        } else if let Some(summary) = observed.remove(id) {
            summaries.push(summary);
        }
    }
    let extras = crate::control::db::thread_evidence::read(db, ids)?;
    let parents = extras
        .values()
        .filter(|row| !row.parent_deleted)
        .filter(|row| {
            row.parent
                .as_ref()
                .is_none_or(|id| !known_parent_title_ids.contains(id))
        })
        .filter_map(|row| Some((row.parent.clone()?, row.parent_title.clone()?)))
        .collect::<Vec<_>>();
    let parents = super::header::read_titles_tx(db, &parents)?;
    Ok(RawWatch {
        summaries,
        extras,
        parents,
        observed_at: Instant::now(),
        wall_at: chrono::Utc::now(),
    })
}

/// Runs on the existing bounded presentation worker, never in an input handler.
#[cfg(test)]
pub(super) fn prepare(raw: RawWatch) -> Vec<PreparedWatchDetail> {
    prepare_with_titles(raw, &BTreeMap::new())
}

#[cfg(test)]
pub(super) fn prepare_with_titles(
    raw: RawWatch,
    known: &BTreeMap<String, String>,
) -> Vec<PreparedWatchDetail> {
    prepare_with_language(raw, known, crate::config::UiLanguage::Ja)
}
#[cfg(test)]
pub(super) fn prepare_with_language(
    raw: RawWatch,
    known: &BTreeMap<String, String>,
    language: crate::config::UiLanguage,
) -> Vec<PreparedWatchDetail> {
    use crate::ui::i18n::local;
    let RawWatch {
        summaries,
        mut extras,
        parents,
        observed_at,
        wall_at,
    } = raw;
    let (parents, _limited) = parents.prepare();
    let saved_age = |value: Option<&str>| {
        super::live_work::age(value, wall_at).map(|elapsed_base| WatchAge {
            elapsed_base,
            observed_at,
            origin: WatchAgeOrigin::SavedTransition,
            saved_at: value
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|at| at.with_timezone(&chrono::Utc)),
        })
    };
    summaries
        .into_iter()
        .filter_map(|summary| {
            let extra = extras.remove(&summary.thread_id)?;
            let phase = thread_summary::prepare_activity(&summary.activity);
            let identity = &summary.identity;
            let state_age = match phase {
                crate::activity::ActivityPhase::Completed
                | crate::activity::ActivityPhase::Stopped
                | crate::activity::ActivityPhase::Failed => {
                    saved_age(identity.run_ended_at.as_deref())
                }
                // The saved recovery reason records when the failure happened;
                // absent (or over the read limit) it is unknown, never Turn creation.
                crate::activity::ActivityPhase::Recovery => {
                    let failed_at = (!extra.recovery_limited)
                        .then_some(extra.recovery.as_deref())
                        .flatten()
                        .and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok())
                        .and_then(|body| {
                            body.pointer("/details/last_failure_at")
                                .and_then(|value| value.as_str())
                                .map(str::to_owned)
                        });
                    saved_age(failed_at.as_deref())
                }
                // A Turn creation time does not prove when waiting began.
                _ => None,
            };
            let model_request_age = matches!(
                phase,
                crate::activity::ActivityPhase::AwaitingModel
                    | crate::activity::ActivityPhase::Receiving
            )
            .then(|| saved_age(identity.attempt_started_at.as_deref()))
            .flatten();
            let mut rows = Vec::with_capacity(2);
            if let Some(id) = extra.execution {
                rows.push(LiveWorkRow {
                    description: None,
                    target: LiveWorkTarget::Shell {
                        execution_id: id,
                        turn_id: extra.shell_turn,
                    },
                    label: super::live_work::label(
                        extra.command.as_deref(),
                        if extra.command_limited {
                            local!(
                                language,
                                "shell（コマンドは取得範囲外・詳細で確認）",
                                "shell (command outside retrieval limit; see details)"
                            )
                        } else {
                            local!(
                                language,
                                "shell（コマンドは詳細で確認）",
                                "shell (see details for command)"
                            )
                        },
                    ),
                    state: extra.shell_state.unwrap_or_default(),
                    elapsed_base: super::live_work::age(extra.shell_started.as_deref(), wall_at),
                    started_at: extra.shell_started,
                    output_age_base: super::live_work::age(extra.output_at.as_deref(), wall_at),
                    last_output_at: extra.output_at,
                    wait_info: None,
                });
            }
            let work = extra
                .run
                .filter(|_| extra.shells > 0)
                .map(|run_id| LiveWorkSnapshot {
                    run_id,
                    overflow: (extra.shells).saturating_sub(rows.len() as u64),
                    rows,
                    shells: extra.shells,

                    waits: None,
                    counts_capped: false,
                    observed_at,
                });
            let parent_title = extra.parent.as_ref().map(|id| {
                if extra.parent_deleted {
                    local!(language, "削除済みの親", "Deleted parent").into()
                } else {
                    known
                        .get(id)
                        .or_else(|| parents.get(id))
                        .cloned()
                        .unwrap_or_else(|| {
                            local!(
                                language,
                                "親の名称は取得範囲外",
                                "Parent title outside retrieval limit"
                            )
                            .into()
                        })
                }
            });
            let recovery_reason = if extra.recovery_limited {
                Some(
                    local!(
                        language,
                        "復旧理由は取得範囲外（詳細で確認）",
                        "Recovery reason outside retrieval limit (see details)"
                    )
                    .into(),
                )
            } else {
                extra.recovery.as_deref().map(|body| {
                    serde_json::from_str::<ApiError>(body)
                        .map(|error| match language {
                            crate::config::UiLanguage::Ja => crate::config_setup::tui_error(&error),
                            crate::config::UiLanguage::En => error.message,
                        })
                        .unwrap_or_else(|_| {
                            local!(
                                language,
                                "保存された復旧理由を詳細で確認してください",
                                "See details for the saved recovery reason"
                            )
                            .into()
                        })
                })
            };
            let answer_preview = if extra.answer_limited {
                Some(
                    local!(
                        language,
                        "回答本文は取得範囲外（会話を開いて確認）",
                        "Answer outside retrieval limit (open conversation)"
                    )
                    .into(),
                )
            } else {
                extra.answer.as_deref().map(|body| {
                    serde_json::from_str(body)
                        .map(|value| crate::history::conversation_excerpt(&value, 512))
                        .unwrap_or_else(|_| {
                            local!(
                                language,
                                "回答本文は読み取れません（原文で確認）",
                                "Answer cannot be read (see original)"
                            )
                            .into()
                        })
                })
            };
            let answer_preview = answer_preview.or_else(|| {
                identity.answer_id.as_ref().map(|_| {
                    local!(
                        language,
                        "回答本文は未取得（会話を開いて確認）",
                        "Answer not retrieved (open conversation)"
                    )
                    .into()
                })
            });
            let detail = WatchDetail {
                thread_id: summary.thread_id.clone(),
                latest_run_id: identity.run_id.clone(),
                answer_id: identity.answer_id.clone(),
                turn_id: identity.turn_id.clone(),
                turn_generation: identity.turn_generation,
                phase,
                parent_title,
                state_age,
                model_request_age,
                work,
                recovery_reason,
                answer_preview,
                pending_messages: extra.pending,
                revisit: thread_summary::prepare(summary, String::new(), String::new()).revisit,
            };
            Some(detail.prepare())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','Saved parent','/','','p','m','now'),('t','Visible','/','','p','m','now'); UPDATE thread SET parent_thread_id='parent' WHERE id='t';
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','2020-01-01T00:00:00Z');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','2020-01-01T00:00:00Z');
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('s','r','u','op','stopping','/','{\"command\":\"echo watch\"}','UTF-8','UTF-8','open','2020-01-01T00:00:01Z');
            INSERT INTO node(id,kind,created_at,payload_json)VALUES('pending','user','now','{\"content\":[{\"type\":\"input_text\",\"text\":\"queued\"}]}');
            INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at)VALUES('receipt','receipt-op','{}','t','pending','accepted','now');").unwrap();
        db
    }
    #[test]
    fn command_json_and_parent_labels_are_bounded_before_copy() {
        let db = fixture();
        let huge = serde_json::json!({"command":"🚀".repeat(5000)}).to_string();
        db.execute(
            "UPDATE shell_execution SET shell_json=?1 WHERE id='s'",
            [&huge],
        )
        .unwrap();
        db.execute(
            "UPDATE thread SET title=?1 WHERE id='parent'",
            ["🚀".repeat(2000)],
        )
        .unwrap();
        let raw = read_tx(&db, &["t".into()]).unwrap();
        assert!(raw.extras["t"].command.is_none());
        assert!(raw.extras["t"].command_limited);
        assert!(raw.extras["t"].parent_title.is_none());
        let prepared = prepare(raw);
        assert!(
            prepared[0].facts().work.as_ref().unwrap().rows[0]
                .label
                .contains("取得範囲外")
        );
        assert!(
            prepared[0]
                .facts()
                .parent_title
                .as_ref()
                .unwrap()
                .contains("取得範囲外")
        );
        db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE shell_execution SET shell_json='broken json' WHERE id='s'; PRAGMA ignore_check_constraints=OFF;").unwrap();
        let raw = read_tx(&db, &["t".into()]).unwrap();
        assert!(raw.extras["t"].command.is_none());
        assert!(!raw.extras["t"].command_limited);
    }
    #[test]
    fn recovery_and_answer_share_the_per_row_source_limit() {
        let db = fixture();
        let answer = serde_json::json!({"content":"a".repeat(9000)}).to_string();
        let recovery = serde_json::json!({"message":"r".repeat(9000)}).to_string();
        db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES('answer','assistant','now',?1)", [&answer]).unwrap();
        db.execute(
            "UPDATE turn SET answer_node_id='answer',recovery_reason_json=?1 WHERE id='u'",
            [&recovery],
        )
        .unwrap();
        let raw = read_tx(&db, &["t".into()]).unwrap();
        let extra = &raw.extras["t"];
        assert!(extra.answer.is_none() && extra.recovery.is_none() && extra.command.is_none());
        assert!(extra.answer_limited && extra.recovery_limited && extra.command_limited);
    }
    #[test]
    fn recovery_row_uses_saved_failure_time_only_when_the_reason_carries_it() {
        let db = fixture();
        db.execute_batch("UPDATE shell_execution SET state='exited'; UPDATE turn SET state='recovery_wait' WHERE id='u';").unwrap();
        let mut states = Vec::new();
        for body in [
            r#"{"code":"io_failed","message":"x","details":{"last_failure_at":"2026-10-31T13:05:00Z"}}"#,
            r#"{"code":"io_failed","message":"x"}"#,
        ] {
            db.execute("UPDATE turn SET recovery_reason_json=?1", [body])
                .unwrap();
            let rows = prepare(read_tx(&db, &["t".into()]).unwrap());
            let facts = rows[0].facts();
            assert_eq!(facts.phase, crate::activity::ActivityPhase::Recovery);
            states.push(facts.state_age.as_ref().and_then(|age| age.saved_at));
        }
        assert_eq!(
            states[0].map(|at| at.to_rfc3339()).as_deref(),
            Some("2026-10-31T13:05:00+00:00")
        );
        assert!(
            states[1].is_none(),
            "absent time is unknown, not Turn creation"
        );
    }
    #[test]
    fn aggregate_budget_charges_whole_command_sources_and_unicode_is_not_bytes() {
        let db = fixture();
        let body = serde_json::json!({"command":"🚀".repeat(3000)}).to_string();
        let mut ids = Vec::new();
        for n in 0..12 {
            let id = format!("visible-{n:02}");
            let run = format!("run-{n}");
            let shell = format!("shell-{n}");
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'Title','/','','p','m','now')", [&id]).unwrap();
            db.execute(
                "INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,'live','now')",
                params![run, id],
            )
            .unwrap();
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,'running','now')", params![shell,id,run]).unwrap();
            db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,?2,?1,?1,'running','/',?3,'UTF-8','UTF-8','open','2020-01-01T00:00:00Z')", params![shell,run,body]).unwrap();
            ids.push(id);
        }
        let raw = read_tx(&db, &ids).unwrap();
        let admitted = raw
            .extras
            .values()
            .filter(|row| row.command.is_some())
            .collect::<Vec<_>>();
        assert_eq!(
            admitted.len(),
            65536 / body.len(),
            "charge source JSON rather than truncated labels"
        );
        assert!(
            admitted
                .iter()
                .all(|row| row.command.as_ref().unwrap().chars().count() == 512)
        );
        assert!(
            admitted
                .iter()
                .all(|row| row.command.as_ref().unwrap().len() == 2048)
        );
        assert_eq!(
            raw.extras
                .values()
                .filter(|row| row.command_limited)
                .count(),
            12 - admitted.len()
        );
        assert!(admitted.len() * body.len() <= 65536);
    }
    #[test]
    fn saved_work_queue_and_identities_share_two_batches_without_raw_output() {
        let db = fixture();
        let raw = read_tx(&db, &["t".into()]).unwrap();
        let extra = &raw.extras["t"];
        assert_eq!(
            extra.pending,
            db.query_row(
                "SELECT count(*) FROM receipt r WHERE r.target_thread_id='t' AND r.state IN ('accepted','held')
                   AND r.path_admission<>'admitted' AND NOT EXISTS(SELECT 1 FROM thread_path p
                   WHERE p.thread_id=r.target_thread_id AND p.node_id=r.message_node_id)",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap() as u64
        );
        assert_eq!(extra.execution.as_deref(), Some("s"));
        assert_eq!(extra.shell_state.as_deref(), Some("stopping"));
        assert_eq!(raw.summaries[0].identity.run_id.as_deref(), Some("r"));
        let rows = prepare(raw);
        let row = rows[0].facts();
        assert_eq!(row.parent_title.as_deref(), Some("Saved parent"));
        assert_eq!(row.phase, crate::activity::ActivityPhase::Background);
        assert_eq!(row.work.as_ref().unwrap().rows[0].state, "stopping");
        assert!(
            row.state_age.is_none(),
            "Turn creation is not a background/recovery transition time"
        );
        assert!(row.model_request_age.is_none());
    }
    #[test]
    fn witness_budget_is_global_and_missing_or_huge_answer_is_explicit() {
        let db = fixture();
        for i in 0..8 {
            let id = format!("v{i}");
            let run = format!("r{i}");
            let turn = format!("u{i}");
            let node = format!("n{i}");
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'Title','/','','p','m','now')",[&id]).unwrap();
            db.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?2,'completed','2020-01-01T00:00:00Z','2020-01-01T00:00:01Z')",params![run,id]).unwrap();
            let body=serde_json::json!({"content":[{"type":"output_text","text":"x".repeat(if i==7 {32768}else{12000})}]}).to_string();
            db.execute(
                "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'assistant','now',?2)",
                params![node, body],
            )
            .unwrap();
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,answer_node_id,created_at,ended_at)VALUES(?1,?2,?3,'completed',?4,'2020-01-01T00:00:00Z','2020-01-01T00:00:01Z')",params![turn,id,run,node]).unwrap();
        }
        let ids = (0..8).map(|i| format!("v{i}")).collect::<Vec<_>>();
        let raw = read_tx(&db, &ids).unwrap();
        assert!(
            raw.extras
                .values()
                .map(|row| row.answer.as_ref().map_or(0, String::len)
                    + row.recovery.as_ref().map_or(0, String::len))
                .sum::<usize>()
                <= 65536
        );
        assert!(raw.extras["v7"].answer.is_none());
        assert!(raw.extras["v7"].answer_limited);
        let rows = prepare(raw);
        assert_eq!(rows.len(), 8);
        assert!(
            rows.iter()
                .find(|row| row.facts().thread_id == "v7")
                .unwrap()
                .facts()
                .answer_preview
                .as_ref()
                .unwrap()
                .contains("取得範囲外")
        );
        assert!(read_tx(&db, &vec!["t".into(); 129]).is_err());
    }
    #[test]
    fn empty_scope_reads_no_summaries() {
        let db = fixture();
        assert!(read_tx(&db, &[]).unwrap().summaries.is_empty());
    }
}

#[cfg(test)]
mod shared_consumer_tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','','/','','p','m','now'),('child','Child','/','','p','m','now'),('other','Other','/','','p','m','now');
          UPDATE thread SET parent_thread_id='parent' WHERE id='child';
          INSERT INTO node(id,kind,created_at,payload_json)VALUES('question','user','now','{\"role\":\"user\",\"content\":\"Derived parent title\"}');
          INSERT INTO thread_path VALUES('parent',0,'question');").unwrap();
        db
    }
    #[test]
    fn shared_e_metadata_and_parent_titles_reuse_owned_evidence() {
        let db = fixture();
        let known = thread_summary::read_ids_tx(&db, &["parent".into(), "child".into()]).unwrap();
        let parent_ids = BTreeSet::from(["parent".into()]);
        let raw = read_shared_tx(&db, &["child".into()], &known, &parent_ids).unwrap();
        let titles = BTreeMap::from([("parent".into(), "Derived parent title".into())]);
        let rows = prepare_with_titles(raw, &titles);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].facts().parent_title.as_deref(),
            Some("Derived parent title")
        );
    }
    #[test]
    fn missing_summary_ids_use_one_bounded_batch_and_limited_parent_title_is_preserved() {
        let db = fixture();
        let known = thread_summary::read_ids_tx(&db, &["child".into()]).unwrap();
        let parent_ids = BTreeSet::from(["parent".into()]);
        let raw = read_shared_tx(
            &db,
            &["child".into(), "other".into(), "other".into()],
            &known,
            &parent_ids,
        )
        .unwrap();
        let limited = "名称未設定（元本文の取得範囲外）";
        let rows = prepare_with_titles(raw, &BTreeMap::from([("parent".into(), limited.into())]));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].facts().parent_title.as_deref(), Some(limited));
        assert!(read_shared_tx(&db, &[], &vec![known[0].clone(); 129], &BTreeSet::new()).is_err());
        assert!(
            read_shared_tx(&db, &[], &[], &(0..129).map(|n| format!("p{n}")).collect()).is_err()
        );
    }
    #[test]
    fn parent_fallback_is_localized_but_user_title_is_not_translated() {
        let db = fixture();
        let known = thread_summary::read_ids_tx(&db, &["child".into()]).unwrap();
        let ids = BTreeSet::from(["parent".into()]);
        for (language, expected) in [
            (
                crate::config::UiLanguage::En,
                "Parent title outside retrieval limit",
            ),
            (crate::config::UiLanguage::Ja, "親の名称は取得範囲外"),
        ] {
            let raw = read_shared_tx(&db, &["child".into()], &known, &ids).unwrap();
            let rows = prepare_with_language(raw, &BTreeMap::new(), language);
            assert_eq!(rows[0].facts().parent_title.as_deref(), Some(expected));
            let raw = read_shared_tx(&db, &["child".into()], &known, &ids).unwrap();
            let title = "親の名称は取得範囲外";
            let rows = prepare_with_language(
                raw,
                &BTreeMap::from([("parent".into(), title.into())]),
                language,
            );
            assert_eq!(rows[0].facts().parent_title.as_deref(), Some(title));
        }
    }
    #[test]
    fn missing_known_title_promise_remains_unavailable_and_deleted_parent_overrides_cached_name() {
        let db = fixture();
        let known = thread_summary::read_ids_tx(&db, &["child".into()]).unwrap();
        let ids = BTreeSet::from(["parent".into()]);
        let raw = read_shared_tx(&db, &["child".into()], &known, &ids).unwrap();
        assert_eq!(
            prepare_with_titles(raw, &BTreeMap::new())[0]
                .facts()
                .parent_title
                .as_deref(),
            Some("親の名称は取得範囲外")
        );
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='parent'", [])
            .unwrap();
        let raw = read_shared_tx(&db, &["child".into()], &known, &ids).unwrap();
        assert_eq!(
            prepare_with_titles(
                raw,
                &BTreeMap::from([("parent".into(), "Stale cached title".into())])
            )[0]
            .facts()
            .parent_title
            .as_deref(),
            Some("削除済みの親")
        );
    }
}

pub(super) fn prepare_native(
    row: &crate::protocol::ThreadSummary,
    evidence: crate::protocol::ThreadListDetail,
    language: crate::config::UiLanguage,
) -> PreparedWatchDetail {
    use crate::activity::ActivityPhase;
    use crate::ui::i18n::local;
    let observed_at = Instant::now();
    let now = chrono::Utc::now();
    let age = |value: Option<&str>| {
        super::live_work::age(value, now).map(|elapsed_base| WatchAge {
            elapsed_base,
            observed_at,
            origin: WatchAgeOrigin::SavedTransition,
            saved_at: value
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&chrono::Utc)),
        })
    };
    let activity = row.activity_detail.as_ref();
    let turn = activity.and_then(|a| a.turn.as_ref());
    let run = activity.map(|a| &a.run);
    let completed = matches!(
        row.activity,
        ActivityPhase::Completed | ActivityPhase::Stopped | ActivityPhase::Failed
    );
    let state_at = if completed {
        run.and_then(|r| r.ended_at.as_deref())
    } else if row.activity == ActivityPhase::Recovery {
        evidence
            .recovery_error
            .as_ref()
            .and_then(|e| e.details.as_ref())
            .and_then(|d| d.get("last_failure_at"))
            .and_then(serde_json::Value::as_str)
    } else {
        None
    };
    let model_request_age = matches!(
        row.activity,
        ActivityPhase::AwaitingModel | ActivityPhase::Receiving
    )
    .then(|| {
        age(activity
            .and_then(|a| a.current_attempt.as_ref())
            .and_then(|a| a.started_at.as_deref()))
    })
    .flatten();
    let work = run.filter(|r| r.owned_shells > 0).map(|r| {
        let rows: Vec<_> = evidence
            .shell
            .into_iter()
            .map(|s| LiveWorkRow {
                description: None,
                target: LiveWorkTarget::Shell {
                    execution_id: s.execution_id,
                    turn_id: s.turn_id,
                },
                label: super::live_work::label(
                    s.command_excerpt.as_deref(),
                    if s.command_limited {
                        local!(
                            language,
                            "shell（コマンドは取得範囲外・詳細で確認）",
                            "shell (command outside retrieval limit; see details)"
                        )
                    } else {
                        local!(
                            language,
                            "shell（コマンドは詳細で確認）",
                            "shell (see details for command)"
                        )
                    },
                ),
                state: s.state,
                elapsed_base: super::live_work::age(s.started_at.as_deref(), now),
                started_at: s.started_at,
                output_age_base: super::live_work::age(s.last_output_at.as_deref(), now),
                last_output_at: s.last_output_at,
                wait_info: None,
            })
            .collect();
        LiveWorkSnapshot {
            run_id: r.run_id.clone(),
            overflow: r.owned_shells.saturating_sub(rows.len() as u64),
            rows,
            shells: r.owned_shells,
            waits: None,
            counts_capped: r.owned_shells >= 128,
            observed_at,
        }
    });
    let parent_title = row.parent_thread_id.as_ref().map(|_| {
        if evidence.parent_deleted {
            local!(language, "削除済みの親", "Deleted parent").into()
        } else {
            evidence.parent_title.unwrap_or_else(|| {
                local!(
                    language,
                    "親の名称は取得範囲外",
                    "Parent title outside retrieval limit"
                )
                .into()
            })
        }
    });
    let answer_id = turn.and_then(|t| t.answer_node_id.clone());
    let answer_preview = if let Some(excerpt) = evidence.answer_excerpt {
        Some(if evidence.answer_truncated {
            format!("{excerpt}…")
        } else {
            excerpt
        })
    } else if evidence.answer_truncated {
        Some(
            local!(
                language,
                "回答本文は取得範囲外（会話を開いて確認）",
                "Answer outside retrieval limit (open conversation)"
            )
            .into(),
        )
    } else {
        answer_id.as_ref().map(|_| {
            local!(
                language,
                "回答本文は未取得（会話を開いて確認）",
                "Answer not retrieved (open conversation)"
            )
            .into()
        })
    };
    let state_age = age(state_at);
    let recovery_reason = if evidence.recovery_limited {
        Some(
            local!(
                language,
                "復旧理由は取得範囲外（詳細で確認）",
                "Recovery reason outside retrieval limit (see details)"
            )
            .into(),
        )
    } else {
        evidence.recovery_error.map(|error| match language {
            crate::config::UiLanguage::Ja => crate::config_setup::tui_error(&error),
            crate::config::UiLanguage::En => error.message,
        })
    };
    WatchDetail {
        thread_id: row.thread_id.clone(),
        latest_run_id: run.map(|r| r.run_id.clone()),
        answer_id,
        turn_id: turn.map(|t| t.turn_id.clone()),
        turn_generation: turn.and_then(|t| u64::try_from(t.generation).ok()),
        phase: row.activity,
        parent_title,
        state_age,
        model_request_age,
        work,
        recovery_reason,
        answer_preview,
        pending_messages: evidence.pending_messages,
        revisit: row.revisit.clone(),
    }
    .prepare()
}
