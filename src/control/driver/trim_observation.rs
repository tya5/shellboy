//! An internal capability: created only from the driver's reduction result,
//! never from a provider response/body or a matching JSON key.
use super::*;

pub(super) struct DriverTrimObservation(Value);
impl DriverTrimObservation {
    pub(super) fn from_reduction(value: Value) -> Self {
        let mut allowed = serde_json::Map::new();
        for key in [
            "outcome",
            "reason",
            "result",
            "summary",
            "error",
            "maximum_outcome",
            "midturn",
        ] {
            if let Some(v) = value.get(key) {
                allowed.insert(key.into(), v.clone());
            }
        }
        // discard/token/active_stage/committed_stages are NOT transferable.
        Self(Value::Object(allowed))
    }
    pub(super) fn persist_applied(
        &self,
        db: &rusqlite::Connection,
        attempt: &str,
        transition: super::super::state::AttemptTransition,
    ) -> Result<(), crate::storage::StorageError> {
        if transition != super::super::state::AttemptTransition::Applied {
            return Ok(());
        }
        // Caller holds the original fail/recovery transaction. A current DB
        // observation wins; this capability restores only legacy driver facts.
        db.execute("UPDATE model_attempt SET error_json=json_set(error_json,'$.automatic_trim_attempt',json(?2)) WHERE id=?1 AND state IN ('failed','incomplete') AND json_valid(error_json) AND json_type(error_json,'$.automatic_trim_attempt') IS NULL",
            params![attempt,self.0.to_string()])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trusted_reduction_cannot_supply_discard_authority_or_replace_current_fact() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE model_attempt(id TEXT,state TEXT,error_json TEXT);INSERT INTO model_attempt VALUES('a','failed','{}');").unwrap();
        let observation = DriverTrimObservation::from_reduction(
            json!({"outcome":"deferred","midturn":{"reason":"open_tool_loop"},"token":"forged","active_stage":"closed_summary","discard":{"reason":"profile_changed"}}),
        );
        observation
            .persist_applied(
                &db,
                "a",
                super::super::super::state::AttemptTransition::Obsolete,
            )
            .unwrap();
        let read = || {
            db.query_row("SELECT error_json FROM model_attempt", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
        };
        assert_eq!(read(), "{}");
        observation
            .persist_applied(
                &db,
                "a",
                super::super::super::state::AttemptTransition::Applied,
            )
            .unwrap();
        let saved: Value = serde_json::from_str(&read()).unwrap();
        assert_eq!(saved["automatic_trim_attempt"]["outcome"], "deferred");
        for field in ["token", "active_stage", "discard"] {
            assert!(saved["automatic_trim_attempt"].get(field).is_none());
        }
        let original = read();
        DriverTrimObservation::from_reduction(json!({"outcome":"adopted"}))
            .persist_applied(
                &db,
                "a",
                super::super::super::state::AttemptTransition::Applied,
            )
            .unwrap();
        assert_eq!(read(), original);
        db.execute(
            "UPDATE model_attempt SET state='completed',error_json='{}'",
            [],
        )
        .unwrap();
        observation
            .persist_applied(
                &db,
                "a",
                super::super::super::state::AttemptTransition::Applied,
            )
            .unwrap();
        assert_eq!(read(), "{}");
    }
}
