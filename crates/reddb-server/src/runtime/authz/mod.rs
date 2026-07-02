//! Authorization surface extracted from `impl_core` (issues #1622/#1623,
//! PRD #1619). Tightly-coupled families live here:
//!
//! - [`privilege`] — the query privilege gate
//!   ([`RedDBRuntime::check_query_privilege`]) and its per-domain gates.
//! - [`projection`] — relational SELECT/JOIN column authorization.
//! - [`policy_columns`] — the free IAM/policy-column helpers the gates
//!   and projection authorization consume.
//! - [`statements`] — the IAM/GRANT/user/policy statement-execution
//!   family (issue #1623) that consumes those same policy-column helpers.
//!
//! Behaviour-preserving move following the `runtime/ai/` directory
//! precedent: this `mod.rs` only lists submodules; the central dispatch
//! in [`super::impl_core`] calls the (unchanged) `pub(crate)` items.

pub(crate) mod policy_columns;
pub(crate) mod privilege;
pub(crate) mod projection;
pub(crate) mod statements;
