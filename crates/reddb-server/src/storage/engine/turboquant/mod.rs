//! Scalar TurboQuant vector indexing.
//!
//! MIT notice: this module is a clean-room RedDB implementation of the
//! TurboQuant surface described by PRD #668. The layout is reserved for
//! future turbovec-compatible fixtures; no upstream turbovec source is copied.

pub mod codebook;
pub mod codec;
pub mod extent;
pub mod index;
pub mod rotation;
pub mod scoring;
