//! Query Execution Engine
//!
//! Jena-inspired algebraic query execution with pluggable engines.
//!
//! # Architecture
//!
//! ```text
//! QueryExpr (AST) → Op (Algebra) → Plan → Iterator<Binding>
//!
//! ┌─────────────────────────────────────────────────────────────┐
//! │                        QueryEngine                          │
//! ├─────────────────────────────────────────────────────────────┤
//! │  compile(QueryExpr) → Op                                    │
//! │  optimize(Op) → Op                                          │
//! │  execute(Op) → BindingIterator                              │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!          ┌──────────────────┼──────────────────┐
//!          ▼                  ▼                  ▼
//!      OpBGP              OpJoin            OpFilter
//!   (BasicPattern)     (Join two Ops)    (Filter Op)
//! ```
//!
//! # Components
//!
//! - **Op**: Algebraic operators (scan, filter, join, union, etc.)
//! - **Transform**: Visitors that transform Op trees
//! - **Binding**: Variable -> Value mapping
//! - **BindingIterator**: Lazy result stream
//! - **QueryEngine**: Compiles, optimizes, and executes queries

pub mod binding;
pub mod iterator;
pub mod op;
pub mod registry;
pub mod transform;

pub use binding::{Binding, BindingBuilder, Var};
pub use iterator::{
    BindingIterator, QueryIter, QueryIterBase, QueryIterFilter, QueryIterJoin, QueryIterProject,
    QueryIterSlice, QueryIterSort, QueryIterUnion,
};
pub use op::{
    Op, OpBGP, OpDisjunction, OpDistinct, OpExtend, OpFilter, OpGroup, OpJoin, OpLeftJoin, OpMinus,
    OpNull, OpOrder, OpProject, OpReduced, OpSequence, OpSlice, OpTable, OpTriple, OpUnion,
    Pattern, Triple,
};
pub use registry::{QueryEngine, QueryEngineFactory, QueryEngineRegistry};
pub use transform::{OpTransform, OpVisitor, TransformCopy, TransformPushFilter};
