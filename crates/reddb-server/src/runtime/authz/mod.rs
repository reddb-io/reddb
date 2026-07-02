//! Authorization surface extracted from `impl_core` (issue #1622, PRD
//! #1619). Three tightly-coupled families live here:
//!
//! - [`privilege`] — the query privilege gate
//!   ([`RedDBRuntime::check_query_privilege`]) and its per-domain gates.
//! - [`projection`] — relational SELECT/JOIN column authorization.
//! - [`policy_columns`] — the free IAM/policy-column helpers the gates
//!   and projection authorization consume.
//!
//! Behaviour-preserving move following the `runtime/ai/` directory
//! precedent: this `mod.rs` only lists submodules; the central dispatch
//! in [`super::impl_core`] calls the (unchanged) `pub(crate)` items.

pub(crate) mod policy_columns;
pub(crate) mod privilege;
pub(crate) mod projection;
