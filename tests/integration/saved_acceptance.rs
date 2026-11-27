// Read by request ID, independently of the IDs returned in the response.
pub(crate) async fn assert_saved_acceptance(
    store: &shellboy::control::db::Store,
    operation_id: String,
    accepted: &shellboy::protocol::MessageAccepted,
) {
    let node: String = store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT message_node_id FROM receipt WHERE client_operation_id=?1",
                [operation_id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(node, accepted.message_id);
    let saved = store
        .database
        .call({
            let node = node.clone();
            move |db| Ok(shellboy::control::db::message_details(db, &node)?)
        })
        .await
        .unwrap();
    assert_eq!(saved.thread_id, accepted.thread_id);
    assert_eq!(saved.run_id, accepted.run_id);
    assert_eq!(saved.turn_id, accepted.turn_id);
    assert_eq!(saved.assignment, accepted.assignment);
}
