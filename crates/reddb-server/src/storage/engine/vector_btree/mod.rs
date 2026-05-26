//! Vector B-tree storage support modules.
//!
//! Slice A only ships `value_codec` — a self-contained LZ4 codec used
//! later by the B-tree large-value path to decide whether a payload is
//! worth compressing before considering an overflow spill. Subsequent
//! slices will add the overflow chain, page integration, and MVCC glue.

pub mod value_codec;
