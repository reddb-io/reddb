//! Compatibility re-exports for the historical connector path.
//!
//! RedWire implementation and connection policy live in [`crate::redwire`].
//! This module keeps `reddb_client::connector::redwire::*` imports compiling
//! without carrying a second client or error model.

pub use crate::error::{ClientError as RedWireError, Result};
pub use crate::redwire::{Auth, ConnectOptions, RedWireClient};
