//! Compatibility exports for RedDB file layout contracts.
//!
//! The layout definitions live in `reddb-file`; `reddb-server` keeps this
//! module only so existing `crate::storage::layout::*` imports continue to
//! resolve while runtime code is migrated.

pub use reddb_file::{
    LayoutOverrides, LayoutToggles, LogDestination, LogRoutingOverrides, StorageLayout,
    TieredLayoutPaths,
};
