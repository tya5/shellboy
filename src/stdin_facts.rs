//! Persistence of owner-confirmed pipe facts, without interpreting child activity.
use rusqlite::{Connection, params};

pub(crate) struct StdinFact {
    pub open: bool,
    /// Only a nonempty, completely successful write supplies this actor timestamp.
    pub successful_write_at: Option<String>,
}

pub(crate) fn save(db: &Connection, execution: &str, fact: &StdinFact) -> rusqlite::Result<()> {
    let changed=db.execute(
        "UPDATE shell_execution SET stdin_state=CASE WHEN state IN ('starting','running','stopping') AND ?2 THEN stdin_state ELSE 'closed' END, shell_json=CASE WHEN ?3 IS NULL THEN json_set(shell_json,'$.stdin_open',json(CASE WHEN state IN ('starting','running','stopping') AND ?2 AND json_type(shell_json,'$.stdin_open') IS NOT 'false' THEN 'true' ELSE 'false' END)) ELSE json_set(shell_json,'$.stdin_open',json(CASE WHEN state IN ('starting','running','stopping') AND ?2 AND json_type(shell_json,'$.stdin_open') IS NOT 'false' THEN 'true' ELSE 'false' END),'$.stdin_last_write_at',?3) END WHERE id=?1",
        params![execution,fact.open,fact.successful_write_at],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

/// Read recorded values by primary key; absence is unknown, not the start intent.
pub(crate) fn read(
    db: &Connection,
    execution: &str,
) -> rusqlite::Result<(Option<bool>, Option<String>)> {
    db.query_row("SELECT CASE WHEN state IN ('exited','failed','stopped') THEN 0 WHEN json_type(shell_json,'$.stdin_open')='true' THEN 1 WHEN json_type(shell_json,'$.stdin_open')='false' THEN 0 END,CASE WHEN json_type(shell_json,'$.stdin_last_write_at')='text' THEN json_extract(shell_json,'$.stdin_last_write_at') END FROM shell_execution WHERE id=?1",[execution],|r|Ok((r.get(0)?,r.get(1)?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn stdin_facts_missing_is_unknown_and_terminal_is_closed_without_guessing() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE shell_execution(id TEXT PRIMARY KEY,state TEXT,stdin_state TEXT,shell_json TEXT); INSERT INTO shell_execution VALUES('s','running','open','{}');").unwrap();
        assert_eq!(read(&db, "s").unwrap(), (None, None));
        db.execute("UPDATE shell_execution SET state='exited'", [])
            .unwrap();
        assert_eq!(read(&db, "s").unwrap(), (Some(false), None));
    }

    #[test]
    fn stdin_open_fact_does_not_reopen_an_eof_admission_reservation() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE shell_execution(id TEXT PRIMARY KEY,state TEXT,stdin_state TEXT,shell_json TEXT); INSERT INTO shell_execution VALUES('s','running','closed','{}');").unwrap();
        save(
            &db,
            "s",
            &StdinFact {
                open: true,
                successful_write_at: None,
            },
        )
        .unwrap();
        let (reservation,fact):(String,bool)=db.query_row("SELECT stdin_state,json_extract(shell_json,'$.stdin_open') FROM shell_execution WHERE id='s'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(reservation, "closed");
        assert!(
            fact,
            "the owner may still know the pipe is open while EOF is only reserved"
        );
    }

    #[test]
    fn stdin_facts_preserve_success_timestamp_on_eof_failure_and_terminal_races() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE shell_execution(id TEXT PRIMARY KEY,state TEXT,stdin_state TEXT,shell_json TEXT); INSERT INTO shell_execution VALUES('s','running','open','{\"unrelated\":true}');").unwrap();
        let read = || {
            db.query_row(
                "SELECT stdin_state,shell_json FROM shell_execution WHERE id='s'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .unwrap()
        };
        save(
            &db,
            "s",
            &StdinFact {
                open: true,
                successful_write_at: Some("2026-11-09T01:02:03Z".into()),
            },
        )
        .unwrap();
        let (state, json) = read();
        let json: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(state, "open");
        assert_eq!(json["stdin_last_write_at"], "2026-11-09T01:02:03Z");
        assert_eq!(json["unrelated"], true);
        save(
            &db,
            "s",
            &StdinFact {
                open: false,
                successful_write_at: None,
            },
        )
        .unwrap();
        assert_eq!(read().0, "closed");
        assert_eq!(
            serde_json::from_str::<Value>(&read().1).unwrap()["stdin_last_write_at"],
            "2026-11-09T01:02:03Z"
        );
        save(
            &db,
            "s",
            &StdinFact {
                open: true,
                successful_write_at: None,
            },
        )
        .unwrap();
        assert_eq!(
            read().0,
            "closed",
            "confirmed open cannot undo an EOF reservation"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&read().1).unwrap()["stdin_open"],
            false
        );
        db.execute("UPDATE shell_execution SET state='exited' WHERE id='s'", [])
            .unwrap();
        save(
            &db,
            "s",
            &StdinFact {
                open: true,
                successful_write_at: Some("2026-11-09T01:02:04Z".into()),
            },
        )
        .unwrap();
        assert_eq!(
            read().0,
            "closed",
            "late write facts cannot reopen terminal pipes"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&read().1).unwrap()["stdin_last_write_at"],
            "2026-11-09T01:02:04Z",
            "confirmed write remains recorded even if completion committed first"
        );
        save(
            &db,
            "missing",
            &StdinFact {
                open: false,
                successful_write_at: None,
            },
        )
        .unwrap_err();
    }
}
