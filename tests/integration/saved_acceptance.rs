use serde_json::Value;

// Read by request ID, independently of the IDs returned in the response.
pub(crate) async fn assert_saved_acceptance(
    store: &shellboy::control::db::Store,
    operation_id: String,
    accepted: &shellboy::protocol::MessageAccepted,
) {
    let (node, thread, run, turn, saved) = store.database.call(move |db| {
        Ok(db.query_row(
            "SELECT message_node_id,target_thread_id,run_id,turn_id,result_json FROM receipt WHERE client_operation_id=?1",
            [operation_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, String>(4)?)),
        )?)
    }).await.unwrap();
    assert_eq!(node, accepted.message_id);
    assert_eq!(thread, accepted.thread_id);
    assert_eq!(run, accepted.run_id);
    assert_eq!(turn, accepted.turn_id);
    let saved: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(
        saved["assignment"],
        serde_json::to_value(accepted.assignment).unwrap()
    );
}
