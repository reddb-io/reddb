//! Vector storage stable contract surface — issue #743.
//!
//! This module exists so callers outside the storage engine (Red UI,
//! `red.*` virtual tables, drivers) can ask "what is the shape of this
//! vector collection, and is its artifact ready to search?" without
//! reaching into internal layout (`engine::vector_store`,
//! `engine::turboquant::*`, segment states, mmap'd buffers).
//!
//! Today it ships exactly one submodule:
//!
//! - [`introspection`] — typed metadata + artifact-state registry. The
//!   public Rust surface every operator-facing read path consumes.

pub mod introspection;
