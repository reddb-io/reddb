//! Branch Steps
//!
//! Steps that branch traversal flow based on conditions.
//!
//! # Steps
//!
//! - `choose()`: If-then-else branching
//! - `union()`: Execute multiple traversals in parallel
//! - `coalesce()`: First non-empty result wins
//! - `optional()`: Execute traversal or pass through
//! - `repeat()`: Loop execution

use super::{
    BasicTraversal, Step, StepResult, Traversal, Traverser, TraverserRequirement, TraverserValue,
};
use std::any::Any;

/// Trait for branch steps
pub trait BranchStep: Step {
    /// Get branch options
    fn branches(&self) -> Vec<&dyn Traversal>;
}

/// Choose step - conditional branching
#[derive(Debug, Clone)]
pub struct ChooseStep {
    id: String,
    labels: Vec<String>,
    /// Condition traversal (produces value for picking)
    condition: Option<BasicTraversal>,
    /// Options: value -> traversal
    options: Vec<(TraverserValue, BasicTraversal)>,
    /// Default option (none)
    default_option: Option<BasicTraversal>,
}

impl ChooseStep {
    /// Create choose() step
    pub fn new() -> Self {
        Self {
            id: "choose_0".to_string(),
            labels: Vec::new(),
            condition: None,
            options: Vec::new(),
            default_option: None,
        }
    }

    /// Set condition traversal
    pub fn with_condition(mut self, condition: BasicTraversal) -> Self {
        self.condition = Some(condition);
        self
    }

    /// Add option
    pub fn option(mut self, value: TraverserValue, traversal: BasicTraversal) -> Self {
        self.options.push((value, traversal));
        self
    }

    /// Set default option
    pub fn default(mut self, traversal: BasicTraversal) -> Self {
        self.default_option = Some(traversal);
        self
    }
}

impl Default for ChooseStep {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for ChooseStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "ChooseStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        &[]
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        // In real impl, would:
        // 1. Run condition traversal
        // 2. Match result against options
        // 3. Execute matching branch
        // For now, pass through to default or filter
        if self.default_option.is_some() {
            StepResult::emit_one(traverser)
        } else {
            StepResult::Filter
        }
    }

    fn reset(&mut self) {}

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Union step - parallel branch execution
#[derive(Debug, Clone)]
pub struct UnionStep {
    id: String,
    labels: Vec<String>,
    /// Branch traversals
    branches: Vec<BasicTraversal>,
    /// Is this a start step
    is_start: bool,
}

impl UnionStep {
    /// Create union() step
    pub fn new(branches: Vec<BasicTraversal>) -> Self {
        Self {
            id: format!("union_{}", branches.len()),
            labels: Vec::new(),
            branches,
            is_start: false,
        }
    }

    /// Mark as start step
    pub fn as_start(mut self) -> Self {
        self.is_start = true;
        self
    }

    /// Get branches
    pub fn get_branches(&self) -> &[BasicTraversal] {
        &self.branches
    }
}

impl Step for UnionStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "UnionStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        &[]
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        // In real impl, would execute all branches and combine results
        // For now, just duplicate traverser for each branch
        let results: Vec<Traverser> = self.branches.iter().map(|_| traverser.split()).collect();
        StepResult::emit_many(results)
    }

    fn reset(&mut self) {
        for branch in &mut self.branches {
            branch.reset();
        }
    }

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Coalesce step - first non-empty result wins
#[derive(Debug, Clone)]
pub struct CoalesceStep {
    id: String,
    labels: Vec<String>,
    /// Branch traversals (tried in order)
    branches: Vec<BasicTraversal>,
}

impl CoalesceStep {
    /// Create coalesce() step
    pub fn new(branches: Vec<BasicTraversal>) -> Self {
        Self {
            id: format!("coalesce_{}", branches.len()),
            labels: Vec::new(),
            branches,
        }
    }
}

impl Step for CoalesceStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "CoalesceStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        &[]
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        // In real impl, would try each branch until one produces output
        // For now, just pass through if any branches exist
        if !self.branches.is_empty() {
            StepResult::emit_one(traverser)
        } else {
            StepResult::Filter
        }
    }

    fn reset(&mut self) {
        for branch in &mut self.branches {
            branch.reset();
        }
    }

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Optional step - execute traversal or pass through
#[derive(Debug, Clone)]
pub struct OptionalStep {
    id: String,
    labels: Vec<String>,
    /// Child traversal
    traversal: BasicTraversal,
}

impl OptionalStep {
    /// Create optional() step
    pub fn new(traversal: BasicTraversal) -> Self {
        Self {
            id: "optional_0".to_string(),
            labels: Vec::new(),
            traversal,
        }
    }
}

impl Step for OptionalStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "OptionalStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        &[]
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        // In real impl, would try child traversal
        // If it produces output, return that
        // Otherwise, return original traverser
        StepResult::emit_one(traverser)
    }

    fn reset(&mut self) {
        self.traversal.reset();
    }

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Repeat step - loop execution
#[derive(Debug, Clone)]
pub struct RepeatStep {
    id: String,
    labels: Vec<String>,
    /// Loop name (for nested loops)
    loop_name: String,
    /// Repeat traversal
    repeat_traversal: BasicTraversal,
    /// Until predicate traversal
    until_traversal: Option<BasicTraversal>,
    /// Emit predicate traversal
    emit_traversal: Option<BasicTraversal>,
    /// Times limit
    times: Option<u32>,
    /// Until first flag (until before repeat)
    until_first: bool,
    /// Emit first flag (emit before repeat)
    emit_first: bool,
}

impl RepeatStep {
    /// Create repeat() step
    pub fn new(repeat_traversal: BasicTraversal) -> Self {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Self {
            id: format!("repeat_{}", id),
            labels: Vec::new(),
            loop_name: format!("repeat_{}", id),
            repeat_traversal,
            until_traversal: None,
            emit_traversal: None,
            times: None,
            until_first: false,
            emit_first: false,
        }
    }

    /// Set loop name
    pub fn with_name(mut self, name: String) -> Self {
        self.loop_name = name;
        self
    }

    /// Set until condition
    pub fn until(mut self, traversal: BasicTraversal) -> Self {
        self.until_traversal = Some(traversal);
        self
    }

    /// Set emit condition
    pub fn emit(mut self, traversal: BasicTraversal) -> Self {
        self.emit_traversal = Some(traversal);
        self
    }

    /// Set times limit
    pub fn times(mut self, times: u32) -> Self {
        self.times = Some(times);
        self
    }

    /// Set until-first (check until before repeat)
    pub fn until_first(mut self) -> Self {
        self.until_first = true;
        self
    }

    /// Set emit-first (emit before repeat)
    pub fn emit_first(mut self) -> Self {
        self.emit_first = true;
        self
    }

    /// Get loop name
    pub fn loop_name(&self) -> &str {
        &self.loop_name
    }
}

impl Step for RepeatStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "RepeatStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        static REQS: &[TraverserRequirement] =
            &[TraverserRequirement::SingleLoop, TraverserRequirement::Path];
        REQS
    }

    fn process_traverser(&self, mut traverser: Traverser) -> StepResult {
        // Initialize loop if needed
        traverser.init_loop(&self.loop_name);

        // Check times limit
        if let Some(times) = self.times {
            if traverser.loop_count(&self.loop_name) >= times {
                return StepResult::emit_one(traverser);
            }
        }

        // In real impl:
        // 1. Check until condition (if until_first)
        // 2. Emit if emit condition passes (if emit_first)
        // 3. Execute repeat traversal
        // 4. Increment loop counter
        // 5. Check until condition (if not until_first)
        // 6. Emit if emit condition passes (if not emit_first)
        // 7. Loop back to step 1

        traverser.incr_loop(&self.loop_name);
        StepResult::emit_one(traverser)
    }

    fn reset(&mut self) {
        self.repeat_traversal.reset();
        if let Some(ref mut t) = self.until_traversal {
            t.reset();
        }
        if let Some(ref mut t) = self.emit_traversal {
            t.reset();
        }
    }

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Local step - execute child traversal for each traverser
#[derive(Debug, Clone)]
pub struct LocalStep {
    id: String,
    labels: Vec<String>,
    /// Child traversal
    traversal: BasicTraversal,
}

impl LocalStep {
    /// Create local() step
    pub fn new(traversal: BasicTraversal) -> Self {
        Self {
            id: "local_0".to_string(),
            labels: Vec::new(),
            traversal,
        }
    }
}

impl Step for LocalStep {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "LocalStep"
    }

    fn labels(&self) -> &[String] {
        &self.labels
    }

    fn add_label(&mut self, label: String) {
        if !self.labels.contains(&label) {
            self.labels.push(label);
        }
    }

    fn requirements(&self) -> &[TraverserRequirement] {
        &[]
    }

    fn process_traverser(&self, traverser: Traverser) -> StepResult {
        // In real impl, would execute child traversal with this traverser
        StepResult::emit_one(traverser)
    }

    fn reset(&mut self) {
        self.traversal.reset();
    }

    fn clone_step(&self) -> Box<dyn Step> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_choose_step() {
        let step = ChooseStep::new();
        assert_eq!(step.name(), "ChooseStep");
    }

    #[test]
    fn test_union_step() {
        let branch1 = BasicTraversal::new();
        let branch2 = BasicTraversal::new();
        let step = UnionStep::new(vec![branch1, branch2]);

        assert_eq!(step.get_branches().len(), 2);

        let traverser = Traverser::new("v1");
        let result = step.process_traverser(traverser);
        if let StepResult::Emit(traversers) = result {
            assert_eq!(traversers.len(), 2);
        }
    }

    #[test]
    fn test_coalesce_step() {
        let step = CoalesceStep::new(vec![BasicTraversal::new()]);
        assert_eq!(step.name(), "CoalesceStep");

        let traverser = Traverser::new("v1");
        let result = step.process_traverser(traverser);
        assert!(matches!(result, StepResult::Emit(_)));
    }

    #[test]
    fn test_optional_step() {
        let step = OptionalStep::new(BasicTraversal::new());
        assert_eq!(step.name(), "OptionalStep");

        let traverser = Traverser::new("v1");
        let result = step.process_traverser(traverser);
        assert!(matches!(result, StepResult::Emit(_)));
    }

    #[test]
    fn test_repeat_step() {
        let repeat_t = BasicTraversal::new();
        let step = RepeatStep::new(repeat_t).times(3);

        assert_eq!(step.name(), "RepeatStep");

        let traverser = Traverser::new("v1");
        let result = step.process_traverser(traverser);
        if let StepResult::Emit(t) = result {
            assert_eq!(t[0].loop_count(step.loop_name()), 1);
        }
    }

    #[test]
    fn test_repeat_with_until_first() {
        let step = RepeatStep::new(BasicTraversal::new())
            .until_first()
            .times(5);

        assert!(step.until_first);
    }

    #[test]
    fn test_local_step() {
        let step = LocalStep::new(BasicTraversal::new());
        assert_eq!(step.name(), "LocalStep");
    }
}
