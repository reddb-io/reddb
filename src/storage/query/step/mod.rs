//! Step Architecture
//!
//! TinkerPop-inspired step hierarchy for graph traversal execution.
//!
//! # Architecture
//!
//! ```text
//! Step (trait)
//! ├── SourceStep       - Start of traversal (V(), E())
//! ├── FilterStep       - Predicate filtering (has, where)
//! ├── MapStep          - 1:1 transformation (map, select)
//! ├── FlatMapStep      - 1:N expansion (out, in, both)
//! ├── SideEffectStep   - Side-effect execution (store, aggregate)
//! ├── BranchStep       - Conditional branching (choose, union)
//! └── BarrierStep      - Synchronization (fold, group, dedup)
//! ```
//!
//! # TraverserRequirements
//!
//! Steps declare their requirements, enabling optimization:
//! - `PATH`: Needs path tracking
//! - `BULK`: Supports bulk processing
//! - `LOOP`: Uses loop counters
//! - `LABELS`: Uses step labels

pub mod barrier;
pub mod branch;
pub mod filter;
pub mod flatmap;
pub mod map;
pub mod sideeffect;
pub mod source;
pub mod traverser;

// Re-export common types
pub use barrier::{
    BarrierStep, CollectingBarrierStep, FoldStep, GroupStep, OrderStep, ReducingBarrierStep,
};
pub use branch::{BranchStep, ChooseStep, OptionalStep, RepeatStep, UnionStep};
pub use filter::{DedupStep, FilterStep, HasStep, LimitStep, Predicate, RangeStep, WhereStep};
pub use flatmap::{BothStep, Direction, EdgeStep, FlatMapStep, InStep, OutStep, VertexStep};
pub use map::{IdStep, MapStep, PathStep, ProjectStep, SelectStep, ValueMapStep};
pub use sideeffect::{AggregateStep, PropertyStep, SideEffectStep, StoreStep};
pub use source::{EdgeSourceStep, SourceStep, VertexSourceStep};
pub use traverser::{
    LoopState, Path, Traverser, TraverserGenerator, TraverserRequirement, TraverserValue,
};

use std::any::Any;
use std::fmt::Debug;

/// Core step trait - all traversal steps implement this
pub trait Step: Send + Sync + Debug {
    /// Step identifier (unique in traversal)
    fn id(&self) -> &str;

    /// Human-readable name
    fn name(&self) -> &str;

    /// Labels assigned to this step
    fn labels(&self) -> &[String];

    /// Add a label to this step
    fn add_label(&mut self, label: String);

    /// Requirements this step declares
    fn requirements(&self) -> &[TraverserRequirement];

    /// Process a single traverser (standard algorithm)
    fn process_traverser(&self, traverser: Traverser) -> StepResult;

    /// Reset step state for reuse
    fn reset(&mut self);

    /// Clone as trait object
    fn clone_step(&self) -> Box<dyn Step>;

    /// Downcast to concrete type
    fn as_any(&self) -> &dyn Any;

    /// Downcast to mutable concrete type
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Result of processing a traverser through a step
#[derive(Debug, Clone)]
pub enum StepResult {
    /// Emit these traversers to next step
    Emit(Vec<Traverser>),
    /// Filter out (no traversers emitted)
    Filter,
    /// Hold traversers (barrier steps)
    Hold(Vec<Traverser>),
    /// Error during processing
    Error(String),
}

impl StepResult {
    /// Create emit result with single traverser
    pub fn emit_one(traverser: Traverser) -> Self {
        StepResult::Emit(vec![traverser])
    }

    /// Create emit result with multiple traversers
    pub fn emit_many(traversers: Vec<Traverser>) -> Self {
        if traversers.is_empty() {
            StepResult::Filter
        } else {
            StepResult::Emit(traversers)
        }
    }

    /// Check if result is a filter
    pub fn is_filter(&self) -> bool {
        matches!(self, StepResult::Filter)
    }

    /// Check if result has traversers
    pub fn has_traversers(&self) -> bool {
        match self {
            StepResult::Emit(t) => !t.is_empty(),
            StepResult::Hold(t) => !t.is_empty(),
            _ => false,
        }
    }

    /// Extract traversers if present
    pub fn into_traversers(self) -> Vec<Traverser> {
        match self {
            StepResult::Emit(t) | StepResult::Hold(t) => t,
            _ => Vec::new(),
        }
    }
}

/// Step execution mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Single-machine standard execution
    Standard,
    /// Distributed graph computer execution
    Computer,
}

/// Step position in traversal
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepPosition {
    /// Start step (source)
    Start,
    /// Middle step
    Middle,
    /// End step (terminal)
    End,
}

/// Traversal parent for nested traversals
pub trait TraversalParent: Send + Sync {
    /// Get global child traversals (executed once)
    fn global_children(&self) -> Vec<&dyn Traversal>;

    /// Get local child traversals (per-traverser)
    fn local_children(&self) -> Vec<&dyn Traversal>;

    /// Get all child traversals
    fn children(&self) -> Vec<&dyn Traversal> {
        let mut all = self.global_children();
        all.extend(self.local_children());
        all
    }
}

/// Traversal interface for step pipelines
pub trait Traversal: Send + Sync + Debug {
    /// Get all steps in order
    fn steps(&self) -> &[Box<dyn Step>];

    /// Get mutable steps
    fn steps_mut(&mut self) -> &mut Vec<Box<dyn Step>>;

    /// Add a step to the end
    fn add_step(&mut self, step: Box<dyn Step>);

    /// Insert step at position
    fn insert_step(&mut self, index: usize, step: Box<dyn Step>);

    /// Remove step at position
    fn remove_step(&mut self, index: usize) -> Box<dyn Step>;

    /// Get step by index
    fn get_step(&self, index: usize) -> Option<&dyn Step>;

    /// Find step by id
    fn find_step(&self, id: &str) -> Option<&dyn Step>;

    /// Get aggregated requirements
    fn requirements(&self) -> Vec<TraverserRequirement>;

    /// Reset all steps
    fn reset(&mut self);
}

/// Basic traversal implementation
#[derive(Debug)]
pub struct BasicTraversal {
    steps: Vec<Box<dyn Step>>,
    requirements_cache: Option<Vec<TraverserRequirement>>,
}

impl BasicTraversal {
    /// Create empty traversal
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            requirements_cache: None,
        }
    }

    /// Create traversal with steps
    pub fn with_steps(steps: Vec<Box<dyn Step>>) -> Self {
        Self {
            steps,
            requirements_cache: None,
        }
    }

    /// Invalidate requirements cache
    fn invalidate_cache(&mut self) {
        self.requirements_cache = None;
    }
}

impl Default for BasicTraversal {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for BasicTraversal {
    fn clone(&self) -> Self {
        Self {
            steps: self.steps.iter().map(|s| s.clone_step()).collect(),
            requirements_cache: self.requirements_cache.clone(),
        }
    }
}

impl Traversal for BasicTraversal {
    fn steps(&self) -> &[Box<dyn Step>] {
        &self.steps
    }

    fn steps_mut(&mut self) -> &mut Vec<Box<dyn Step>> {
        self.invalidate_cache();
        &mut self.steps
    }

    fn add_step(&mut self, step: Box<dyn Step>) {
        self.invalidate_cache();
        self.steps.push(step);
    }

    fn insert_step(&mut self, index: usize, step: Box<dyn Step>) {
        self.invalidate_cache();
        self.steps.insert(index, step);
    }

    fn remove_step(&mut self, index: usize) -> Box<dyn Step> {
        self.invalidate_cache();
        self.steps.remove(index)
    }

    fn get_step(&self, index: usize) -> Option<&dyn Step> {
        self.steps.get(index).map(|s| s.as_ref())
    }

    fn find_step(&self, id: &str) -> Option<&dyn Step> {
        self.steps.iter().find(|s| s.id() == id).map(|s| s.as_ref())
    }

    fn requirements(&self) -> Vec<TraverserRequirement> {
        if let Some(ref cached) = self.requirements_cache {
            return cached.clone();
        }

        let mut reqs: Vec<TraverserRequirement> = Vec::new();
        for step in &self.steps {
            for req in step.requirements() {
                if !reqs.contains(req) {
                    reqs.push(req.clone());
                }
            }
        }
        reqs
    }

    fn reset(&mut self) {
        for step in &mut self.steps {
            step.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_result_emit_one() {
        let traverser = Traverser::new("test");
        let result = StepResult::emit_one(traverser);
        assert!(result.has_traversers());
    }

    #[test]
    fn test_step_result_filter() {
        let result = StepResult::Filter;
        assert!(result.is_filter());
        assert!(!result.has_traversers());
    }

    #[test]
    fn test_step_result_into_traversers() {
        let t1 = Traverser::new("a");
        let t2 = Traverser::new("b");
        let result = StepResult::emit_many(vec![t1, t2]);
        let traversers = result.into_traversers();
        assert_eq!(traversers.len(), 2);
    }

    #[test]
    fn test_basic_traversal() {
        let traversal = BasicTraversal::new();
        assert_eq!(traversal.steps().len(), 0);
    }
}
