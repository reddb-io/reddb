//! DevX Error Types
//!
//! Error handling for DevX operations.

use std::fmt;

/// Error type for DevX operations
#[derive(Debug)]
pub enum DevXError {
    Validation(String),
    Storage(String),
    NotFound(String),
}

impl fmt::Display for DevXError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(msg) => write!(f, "Validation error: {}", msg),
            Self::Storage(msg) => write!(f, "Storage error: {}", msg),
            Self::NotFound(msg) => write!(f, "Not found: {}", msg),
        }
    }
}

impl std::error::Error for DevXError {}
