//! An internal capability: created only from the driver's reduction result,
//! never from a provider response/body or a matching JSON key.
use super::*;

pub(super) struct DriverTrimObservation(Value);
impl DriverTrimObservation {
    pub(super) fn from_reduction(value: Value) -> Self {
        Self(value)
    }

    #[cfg(test)]
    pub(super) fn persist_applied(
        &self,
        db: &rusqlite::Connection,
        attempt: &str,
        transition: super::super::state::AttemptTransition,
    ) -> Result<(), crate::storage::StorageError> {
        if transition != super::super::state::AttemptTransition::Applied {
            return Ok(());
        }
        self.persist_trim(db, attempt)
    }

    pub(super) fn persist_trim(
        &self,
        db: &rusqlite::Connection,
        attempt: &str,
    ) -> Result<(), crate::storage::StorageError> {
        let value = &self.0;
        let metrics = [
            value.get("summary"),
            value.get("result"),
            value.pointer("/error/details"),
        ]
        .into_iter()
        .flatten()
        .find(|v| !v.is_null());
        let mut result = json!({"metrics":metrics});
        for key in ["error", "midturn", "maximum_outcome"] {
            if let Some(fact) = value.get(key) {
                result[key] = fact.clone();
            }
        }
        db.execute("INSERT INTO automatic_compaction(attempt_id,turn_id,attempt_started_at,reason,outcome,result_json,ended_at) SELECT id,turn_id,started_at,?2,?3,?4,ended_at FROM model_attempt WHERE id=?1 AND state IN ('failed','incomplete')",
            params![attempt,value["reason"].as_str(),value["outcome"].as_str(),result.to_string()])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn driver_reduction_admits_once_without_updating_provider_evidence() {
        use super::super::super::state::AttemptTransition;
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE model_attempt(id TEXT,state TEXT,turn_id TEXT,started_at TEXT,ended_at TEXT,error_json TEXT); INSERT INTO model_attempt VALUES('a','failed','u','start','end','{}');
            CREATE TABLE automatic_compaction(attempt_id TEXT PRIMARY KEY,turn_id TEXT,attempt_started_at TEXT,reason TEXT,outcome TEXT,result_json TEXT,ended_at TEXT,token TEXT,failure_json TEXT);").unwrap();
        let observation = DriverTrimObservation::from_reduction(
            json!({"outcome":"deferred","reason":"open_tool_loop","token":"forged","discard":{"reason":"profile_changed"}}),
        );
        let count = || {
            db.query_row("SELECT count(*) FROM automatic_compaction", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        observation
            .persist_applied(&db, "a", AttemptTransition::Obsolete)
            .unwrap();
        assert_eq!(count(), 0);
        observation
            .persist_applied(&db, "a", AttemptTransition::Applied)
            .unwrap();
        assert_eq!(count(), 1);
        let fact: (String, String, bool) = db.query_row("SELECT outcome,reason,token IS NULL AND failure_json IS NULL FROM automatic_compaction WHERE attempt_id='a'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(fact, ("deferred".into(), "open_tool_loop".into(), true));
        assert!(
            DriverTrimObservation::from_reduction(json!({"outcome":"adopted"}))
                .persist_applied(&db, "a", AttemptTransition::Applied)
                .is_err()
        );
        assert_eq!(count(), 1);
        let error: String = db
            .query_row(
                "SELECT error_json FROM model_attempt WHERE id='a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(error, "{}");
    }
}
