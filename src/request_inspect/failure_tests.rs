use super::*;
use crate::protocol::{ApiError, ErrorCode};

#[test]
fn unrelated_domain_errors_keep_their_code_hint_and_details() {
    let error = ApiError::new(ErrorCode::InvalidArgument, "synthetic argument error")
        .hint("synthetic hint")
        .details(json!({"reason":"synthetic_not_a_history_fault"}));
    let failure = Failure::from(crate::storage::StorageError::from(error));
    assert_eq!(failure.code, "invalid_argument");
    assert_eq!(failure.hint.as_deref(), Some("synthetic hint"));
    assert_eq!(
        failure.details.unwrap()["reason"],
        "synthetic_not_a_history_fault"
    );
}

#[test]
fn sqlite_errors_are_not_relabelled_as_history_faults() {
    let failure = Failure::from(crate::storage::StorageError::Sqlite(
        rusqlite::Error::QueryReturnedNoRows,
    ));
    assert_eq!(failure.code, "storage_failed");
    assert!(failure.details.is_none());
}
