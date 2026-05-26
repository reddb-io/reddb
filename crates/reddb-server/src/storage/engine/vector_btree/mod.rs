//! Vector B-tree storage support modules.
//!
//! - `value_codec` (slice A) — self-contained LZ4 codec used by the
//!   B-tree large-value path to decide whether a payload is worth
//!   compressing before considering an overflow spill.
//! - `page_format` (slice C) — on-disk page format v2: adds
//!   `PageType::Overflow`, two flag bits on every leaf cell, and a
//!   format-version bump with v1 backward-read compatibility.
//!
//! Subsequent slices will wire these modules into the engine's
//! overflow chain, page integration, and MVCC glue.

pub mod page_format;
pub mod value_codec;
