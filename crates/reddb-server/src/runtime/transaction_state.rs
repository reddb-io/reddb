//! Native compatibility path and error classification for shared transaction state.
use reddb_engine::transaction::state::SavepointError;
pub(crate) use reddb_engine::transaction::state::TransactionState;

impl From<SavepointError> for crate::api::RedDBError {
    fn from(error: SavepointError) -> Self {
        Self::Internal(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RedDBError;
    use crate::storage::transaction::IsolationLevel;

    #[test]
    fn missing_savepoints_preserve_native_error_classification() {
        let state = TransactionState::new();
        state.begin(1, IsolationLevel::SnapshotIsolation);
        for error in [
            state.release_savepoint(1, "missing").err(),
            state.rollback_to_savepoint(1, "missing").err(),
        ] {
            let native: RedDBError = error.expect("missing savepoint fails").into();
            assert!(
                matches!(native, RedDBError::Internal(message) if message == "savepoint missing does not exist")
            );
        }
    }
}
