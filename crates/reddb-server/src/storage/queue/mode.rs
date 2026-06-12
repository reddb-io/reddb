// `QueueMode` re-homed to the neutral keystone crate (ADR 0053, RQL Phase 2
// S4b) so the canonical SQL AST resolves it without a `reddb-server` edge. This
// shim keeps `storage::queue::mode::QueueMode` valid for existing call-sites.
pub use reddb_types::queue_mode::QueueMode;
