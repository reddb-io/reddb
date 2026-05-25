//! Scalar TurboQuant vector indexing.
//!
//! MIT notice: this module is a clean-room RedDB implementation of the
//! TurboQuant surface described by PRD #668. The blocked-by-32
//! encoded-codes layout follows the upstream turbovec design (ADR 0024,
//! RyanCodrai/turbovec MIT); no upstream SIMD source is copied.

pub mod assigner;
pub mod codebook;
pub mod codec;
pub mod extent;
pub mod index;
pub mod rotation;
pub mod scoring;
pub mod storage;
