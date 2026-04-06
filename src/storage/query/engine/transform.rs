//! Op Tree Transformations
//!
//! Visitor and transformer patterns for query algebra trees.
//!
//! # Patterns
//!
//! - **OpVisitor**: Read-only traversal for analysis
//! - **OpTransform**: Transform tree nodes (returns new tree)
//! - **TransformCopy**: Default copy transform (base for custom transforms)
//! - **TransformPushFilter**: Push filters down for early evaluation

use super::binding::Var;
use super::op::*;

/// Visitor trait for walking Op trees (read-only)
pub trait OpVisitor {
    /// Visit BGP
    fn visit_bgp(&mut self, _op: &OpBGP) {}

    /// Visit triple
    fn visit_triple(&mut self, _op: &OpTriple) {}

    /// Visit join
    fn visit_join(&mut self, _op: &OpJoin) {}

    /// Visit left join
    fn visit_left_join(&mut self, _op: &OpLeftJoin) {}

    /// Visit filter
    fn visit_filter(&mut self, _op: &OpFilter) {}

    /// Visit union
    fn visit_union(&mut self, _op: &OpUnion) {}

    /// Visit project
    fn visit_project(&mut self, _op: &OpProject) {}

    /// Visit distinct
    fn visit_distinct(&mut self, _op: &OpDistinct) {}

    /// Visit reduced
    fn visit_reduced(&mut self, _op: &OpReduced) {}

    /// Visit slice
    fn visit_slice(&mut self, _op: &OpSlice) {}

    /// Visit order
    fn visit_order(&mut self, _op: &OpOrder) {}

    /// Visit group
    fn visit_group(&mut self, _op: &OpGroup) {}

    /// Visit extend
    fn visit_extend(&mut self, _op: &OpExtend) {}

    /// Visit minus
    fn visit_minus(&mut self, _op: &OpMinus) {}

    /// Visit right join
    fn visit_right_join(&mut self, _op: &OpRightJoin) {}

    /// Visit cross join
    fn visit_cross_join(&mut self, _op: &OpCrossJoin) {}

    /// Visit intersect
    fn visit_intersect(&mut self, _op: &OpIntersect) {}

    /// Visit table
    fn visit_table(&mut self, _op: &OpTable) {}

    /// Visit sequence
    fn visit_sequence(&mut self, _op: &OpSequence) {}

    /// Visit disjunction
    fn visit_disjunction(&mut self, _op: &OpDisjunction) {}

    /// Visit null
    fn visit_null(&mut self, _op: &OpNull) {}
}

/// Walk an Op tree with a visitor
pub fn walk_op<V: OpVisitor>(visitor: &mut V, op: &Op) {
    match op {
        Op::BGP(o) => {
            visitor.visit_bgp(o);
        }
        Op::Triple(o) => {
            visitor.visit_triple(o);
        }
        Op::Join(o) => {
            walk_op(visitor, &o.left);
            walk_op(visitor, &o.right);
            visitor.visit_join(o);
        }
        Op::LeftJoin(o) => {
            walk_op(visitor, &o.left);
            walk_op(visitor, &o.right);
            visitor.visit_left_join(o);
        }
        Op::Filter(o) => {
            walk_op(visitor, &o.sub_op);
            visitor.visit_filter(o);
        }
        Op::Union(o) => {
            walk_op(visitor, &o.left);
            walk_op(visitor, &o.right);
            visitor.visit_union(o);
        }
        Op::Project(o) => {
            walk_op(visitor, &o.sub_op);
            visitor.visit_project(o);
        }
        Op::Distinct(o) => {
            walk_op(visitor, &o.sub_op);
            visitor.visit_distinct(o);
        }
        Op::Reduced(o) => {
            walk_op(visitor, &o.sub_op);
            visitor.visit_reduced(o);
        }
        Op::Slice(o) => {
            walk_op(visitor, &o.sub_op);
            visitor.visit_slice(o);
        }
        Op::Order(o) => {
            walk_op(visitor, &o.sub_op);
            visitor.visit_order(o);
        }
        Op::Group(o) => {
            walk_op(visitor, &o.sub_op);
            visitor.visit_group(o);
        }
        Op::Extend(o) => {
            walk_op(visitor, &o.sub_op);
            visitor.visit_extend(o);
        }
        Op::Minus(o) => {
            walk_op(visitor, &o.left);
            walk_op(visitor, &o.right);
            visitor.visit_minus(o);
        }
        Op::RightJoin(o) => {
            walk_op(visitor, &o.left);
            walk_op(visitor, &o.right);
            visitor.visit_right_join(o);
        }
        Op::CrossJoin(o) => {
            walk_op(visitor, &o.left);
            walk_op(visitor, &o.right);
            visitor.visit_cross_join(o);
        }
        Op::Intersect(o) => {
            walk_op(visitor, &o.left);
            walk_op(visitor, &o.right);
            visitor.visit_intersect(o);
        }
        Op::Table(o) => {
            visitor.visit_table(o);
        }
        Op::Sequence(o) => {
            for sub in &o.ops {
                walk_op(visitor, sub);
            }
            visitor.visit_sequence(o);
        }
        Op::Disjunction(o) => {
            for sub in &o.ops {
                walk_op(visitor, sub);
            }
            visitor.visit_disjunction(o);
        }
        Op::Null(o) => {
            visitor.visit_null(o);
        }
    }
}

/// Transformer trait for transforming Op trees
pub trait OpTransform {
    /// Transform BGP
    fn transform_bgp(&mut self, op: OpBGP) -> Op {
        Op::BGP(op)
    }

    /// Transform triple
    fn transform_triple(&mut self, op: OpTriple) -> Op {
        Op::Triple(op)
    }

    /// Transform join
    fn transform_join(&mut self, left: Op, right: Op, _original: &OpJoin) -> Op {
        Op::Join(OpJoin::new(left, right))
    }

    /// Transform left join
    fn transform_left_join(&mut self, left: Op, right: Op, original: &OpLeftJoin) -> Op {
        Op::LeftJoin(OpLeftJoin {
            left: Box::new(left),
            right: Box::new(right),
            filter: original.filter.clone(),
        })
    }

    /// Transform right join
    fn transform_right_join(&mut self, left: Op, right: Op, original: &OpRightJoin) -> Op {
        Op::RightJoin(OpRightJoin {
            left: Box::new(left),
            right: Box::new(right),
            filter: original.filter.clone(),
        })
    }

    /// Transform cross join
    fn transform_cross_join(&mut self, left: Op, right: Op, _original: &OpCrossJoin) -> Op {
        Op::CrossJoin(OpCrossJoin {
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    /// Transform intersect
    fn transform_intersect(&mut self, left: Op, right: Op, _original: &OpIntersect) -> Op {
        Op::Intersect(OpIntersect {
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    /// Transform filter
    fn transform_filter(&mut self, sub_op: Op, original: &OpFilter) -> Op {
        Op::Filter(OpFilter {
            filter: original.filter.clone(),
            sub_op: Box::new(sub_op),
        })
    }

    /// Transform union
    fn transform_union(&mut self, left: Op, right: Op, _original: &OpUnion) -> Op {
        Op::Union(OpUnion::new(left, right))
    }

    /// Transform project
    fn transform_project(&mut self, sub_op: Op, original: &OpProject) -> Op {
        Op::Project(OpProject {
            vars: original.vars.clone(),
            sub_op: Box::new(sub_op),
        })
    }

    /// Transform distinct
    fn transform_distinct(&mut self, sub_op: Op, _original: &OpDistinct) -> Op {
        Op::Distinct(OpDistinct::new(sub_op))
    }

    /// Transform reduced
    fn transform_reduced(&mut self, sub_op: Op, _original: &OpReduced) -> Op {
        Op::Reduced(OpReduced::new(sub_op))
    }

    /// Transform slice
    fn transform_slice(&mut self, sub_op: Op, original: &OpSlice) -> Op {
        Op::Slice(OpSlice::new(sub_op, original.offset, original.limit))
    }

    /// Transform order
    fn transform_order(&mut self, sub_op: Op, original: &OpOrder) -> Op {
        Op::Order(OpOrder {
            sub_op: Box::new(sub_op),
            keys: original.keys.clone(),
        })
    }

    /// Transform group
    fn transform_group(&mut self, sub_op: Op, original: &OpGroup) -> Op {
        Op::Group(OpGroup {
            sub_op: Box::new(sub_op),
            group_vars: original.group_vars.clone(),
            aggregates: original.aggregates.clone(),
        })
    }

    /// Transform extend
    fn transform_extend(&mut self, sub_op: Op, original: &OpExtend) -> Op {
        Op::Extend(OpExtend {
            sub_op: Box::new(sub_op),
            var: original.var.clone(),
            expr: original.expr.clone(),
        })
    }

    /// Transform minus
    fn transform_minus(&mut self, left: Op, right: Op, _original: &OpMinus) -> Op {
        Op::Minus(OpMinus::new(left, right))
    }

    /// Transform table
    fn transform_table(&mut self, op: OpTable) -> Op {
        Op::Table(op)
    }

    /// Transform sequence
    fn transform_sequence(&mut self, ops: Vec<Op>, _original: &OpSequence) -> Op {
        Op::Sequence(OpSequence::new(ops))
    }

    /// Transform disjunction
    fn transform_disjunction(&mut self, ops: Vec<Op>, _original: &OpDisjunction) -> Op {
        Op::Disjunction(OpDisjunction::new(ops))
    }

    /// Transform null
    fn transform_null(&mut self, op: OpNull) -> Op {
        Op::Null(op)
    }
}

/// Apply a transform to an Op tree (bottom-up)
pub fn transform_op<T: OpTransform>(transformer: &mut T, op: Op) -> Op {
    match op {
        Op::BGP(o) => transformer.transform_bgp(o),
        Op::Triple(o) => transformer.transform_triple(o),
        Op::Join(o) => {
            let orig = o.clone();
            let left = transform_op(transformer, *o.left);
            let right = transform_op(transformer, *o.right);
            transformer.transform_join(left, right, &orig)
        }
        Op::LeftJoin(o) => {
            let orig = o.clone();
            let left = transform_op(transformer, *o.left);
            let right = transform_op(transformer, *o.right);
            transformer.transform_left_join(left, right, &orig)
        }
        Op::Filter(o) => {
            let orig = o.clone();
            let sub_op = transform_op(transformer, *o.sub_op);
            transformer.transform_filter(sub_op, &orig)
        }
        Op::Union(o) => {
            let orig = o.clone();
            let left = transform_op(transformer, *o.left);
            let right = transform_op(transformer, *o.right);
            transformer.transform_union(left, right, &orig)
        }
        Op::Project(o) => {
            let orig = o.clone();
            let sub_op = transform_op(transformer, *o.sub_op);
            transformer.transform_project(sub_op, &orig)
        }
        Op::Distinct(o) => {
            let orig = o.clone();
            let sub_op = transform_op(transformer, *o.sub_op);
            transformer.transform_distinct(sub_op, &orig)
        }
        Op::Reduced(o) => {
            let orig = o.clone();
            let sub_op = transform_op(transformer, *o.sub_op);
            transformer.transform_reduced(sub_op, &orig)
        }
        Op::Slice(o) => {
            let orig = o.clone();
            let sub_op = transform_op(transformer, *o.sub_op);
            transformer.transform_slice(sub_op, &orig)
        }
        Op::Order(o) => {
            let orig = o.clone();
            let sub_op = transform_op(transformer, *o.sub_op);
            transformer.transform_order(sub_op, &orig)
        }
        Op::Group(o) => {
            let orig = o.clone();
            let sub_op = transform_op(transformer, *o.sub_op);
            transformer.transform_group(sub_op, &orig)
        }
        Op::Extend(o) => {
            let orig = o.clone();
            let sub_op = transform_op(transformer, *o.sub_op);
            transformer.transform_extend(sub_op, &orig)
        }
        Op::Minus(o) => {
            let orig = o.clone();
            let left = transform_op(transformer, *o.left);
            let right = transform_op(transformer, *o.right);
            transformer.transform_minus(left, right, &orig)
        }
        Op::RightJoin(o) => {
            let orig = o.clone();
            let left = transform_op(transformer, *o.left);
            let right = transform_op(transformer, *o.right);
            transformer.transform_right_join(left, right, &orig)
        }
        Op::CrossJoin(o) => {
            let orig = o.clone();
            let left = transform_op(transformer, *o.left);
            let right = transform_op(transformer, *o.right);
            transformer.transform_cross_join(left, right, &orig)
        }
        Op::Intersect(o) => {
            let orig = o.clone();
            let left = transform_op(transformer, *o.left);
            let right = transform_op(transformer, *o.right);
            transformer.transform_intersect(left, right, &orig)
        }
        Op::Table(o) => transformer.transform_table(o),
        Op::Sequence(o) => {
            let ops: Vec<Op> = o
                .ops
                .into_iter()
                .map(|sub| transform_op(transformer, sub))
                .collect();
            transformer.transform_sequence(ops, &OpSequence::new(Vec::new()))
        }
        Op::Disjunction(o) => {
            let ops: Vec<Op> = o
                .ops
                .into_iter()
                .map(|sub| transform_op(transformer, sub))
                .collect();
            transformer.transform_disjunction(ops, &OpDisjunction::new(Vec::new()))
        }
        Op::Null(o) => transformer.transform_null(o),
    }
}

/// Default copy transform (identity transformation)
#[derive(Debug, Default)]
pub struct TransformCopy;

impl TransformCopy {
    /// Create new copy transform
    pub fn new() -> Self {
        Self
    }
}

impl OpTransform for TransformCopy {}

/// Filter pushdown transform
///
/// Pushes filters down the tree to evaluate them as early as possible.
/// This reduces intermediate result sizes.
#[derive(Debug, Default)]
pub struct TransformPushFilter {
    /// Filters to push down
    pending_filters: Vec<FilterExpr>,
}

impl TransformPushFilter {
    /// Create new pushdown transform
    pub fn new() -> Self {
        Self {
            pending_filters: Vec::new(),
        }
    }

    /// Check if filter uses only variables from the given set
    fn filter_uses_only(&self, filter: &FilterExpr, vars: &[Var]) -> bool {
        let filter_vars = self.extract_filter_vars(filter);
        filter_vars.iter().all(|v| vars.contains(v))
    }

    /// Extract variables used in a filter
    fn extract_filter_vars(&self, filter: &FilterExpr) -> Vec<Var> {
        let mut vars = Vec::new();
        self.collect_filter_vars(filter, &mut vars);
        vars
    }

    fn collect_filter_vars(&self, filter: &FilterExpr, vars: &mut Vec<Var>) {
        match filter {
            FilterExpr::Eq(l, r)
            | FilterExpr::NotEq(l, r)
            | FilterExpr::Lt(l, r)
            | FilterExpr::LtEq(l, r)
            | FilterExpr::Gt(l, r)
            | FilterExpr::GtEq(l, r) => {
                self.collect_term_vars(l, vars);
                self.collect_term_vars(r, vars);
            }
            FilterExpr::And(l, r) | FilterExpr::Or(l, r) => {
                self.collect_filter_vars(l, vars);
                self.collect_filter_vars(r, vars);
            }
            FilterExpr::Not(e) => {
                self.collect_filter_vars(e, vars);
            }
            FilterExpr::Bound(v) => {
                if !vars.contains(v) {
                    vars.push(v.clone());
                }
            }
            FilterExpr::Regex(t, _, _)
            | FilterExpr::StartsWith(t, _)
            | FilterExpr::EndsWith(t, _)
            | FilterExpr::Contains(t, _)
            | FilterExpr::IsUri(t)
            | FilterExpr::IsLiteral(t)
            | FilterExpr::IsBlank(t) => {
                self.collect_term_vars(t, vars);
            }
            FilterExpr::In(t, list) | FilterExpr::NotIn(t, list) => {
                self.collect_term_vars(t, vars);
                for item in list {
                    self.collect_term_vars(item, vars);
                }
            }
            FilterExpr::True | FilterExpr::False => {}
        }
    }

    fn collect_term_vars(&self, term: &ExprTerm, vars: &mut Vec<Var>) {
        match term {
            ExprTerm::Var(v) => {
                if !vars.contains(v) {
                    vars.push(v.clone());
                }
            }
            ExprTerm::Str(inner)
            | ExprTerm::LCase(inner)
            | ExprTerm::UCase(inner)
            | ExprTerm::StrLen(inner) => {
                self.collect_term_vars(inner, vars);
            }
            ExprTerm::Concat(terms) => {
                for t in terms {
                    self.collect_term_vars(t, vars);
                }
            }
            ExprTerm::Const(_) => {}
        }
    }

    /// Wrap op with pending filters that apply to it
    fn apply_pending_filters(&mut self, op: Op) -> Op {
        if self.pending_filters.is_empty() {
            return op;
        }

        let op_vars = op.vars();
        let mut applicable = Vec::new();
        let mut remaining = Vec::new();

        // Collect filters first to avoid borrow conflict
        let filters: Vec<_> = self.pending_filters.drain(..).collect();
        for filter in filters {
            if self.filter_uses_only(&filter, &op_vars) {
                applicable.push(filter);
            } else {
                remaining.push(filter);
            }
        }

        self.pending_filters = remaining;

        let mut result = op;
        for filter in applicable {
            result = Op::Filter(OpFilter::new(filter, result));
        }
        result
    }
}

impl OpTransform for TransformPushFilter {
    fn transform_filter(&mut self, sub_op: Op, original: &OpFilter) -> Op {
        // Collect filter and continue transformation
        self.pending_filters.push(original.filter.clone());
        self.apply_pending_filters(sub_op)
    }

    fn transform_join(&mut self, left: Op, right: Op, _original: &OpJoin) -> Op {
        // Try to push filters to either side
        let left = self.apply_pending_filters(left);
        let right = self.apply_pending_filters(right);

        let result = Op::Join(OpJoin::new(left, right));
        self.apply_pending_filters(result)
    }

    fn transform_bgp(&mut self, op: OpBGP) -> Op {
        self.apply_pending_filters(Op::BGP(op))
    }

    fn transform_triple(&mut self, op: OpTriple) -> Op {
        self.apply_pending_filters(Op::Triple(op))
    }

    fn transform_table(&mut self, op: OpTable) -> Op {
        self.apply_pending_filters(Op::Table(op))
    }
}

/// Variable collector visitor
#[derive(Debug, Default)]
pub struct VarCollector {
    pub vars: Vec<Var>,
}

impl VarCollector {
    /// Create new collector
    pub fn new() -> Self {
        Self { vars: Vec::new() }
    }

    /// Collect vars from op
    pub fn collect(op: &Op) -> Vec<Var> {
        let mut collector = Self::new();
        walk_op(&mut collector, op);
        collector.vars
    }

    fn add_var(&mut self, var: &Var) {
        if !self.vars.contains(var) {
            self.vars.push(var.clone());
        }
    }
}

impl OpVisitor for VarCollector {
    fn visit_bgp(&mut self, op: &OpBGP) {
        for triple in &op.triples {
            for v in triple.vars() {
                self.add_var(&v);
            }
        }
    }

    fn visit_triple(&mut self, op: &OpTriple) {
        for v in op.triple.vars() {
            self.add_var(&v);
        }
    }

    fn visit_project(&mut self, op: &OpProject) {
        for v in &op.vars {
            self.add_var(v);
        }
    }

    fn visit_group(&mut self, op: &OpGroup) {
        for v in &op.group_vars {
            self.add_var(v);
        }
        for (v, _) in &op.aggregates {
            self.add_var(v);
        }
    }

    fn visit_extend(&mut self, op: &OpExtend) {
        self.add_var(&op.var);
    }

    fn visit_table(&mut self, op: &OpTable) {
        for v in &op.vars {
            self.add_var(v);
        }
    }
}

/// Op tree printer visitor
#[derive(Debug)]
pub struct OpPrinter {
    indent: usize,
    output: String,
}

impl OpPrinter {
    /// Create new printer
    pub fn new() -> Self {
        Self {
            indent: 0,
            output: String::new(),
        }
    }

    /// Print op tree to string
    pub fn print(op: &Op) -> String {
        let mut printer = Self::new();
        printer.print_op(op);
        printer.output
    }

    fn print_op(&mut self, op: &Op) {
        let indent_str = "  ".repeat(self.indent);

        match op {
            Op::BGP(o) => {
                self.output.push_str(&format!("{}BGP\n", indent_str));
                self.indent += 1;
                for triple in &o.triples {
                    self.output
                        .push_str(&format!("{}  {}\n", indent_str, triple));
                }
                self.indent -= 1;
            }
            Op::Triple(o) => {
                self.output
                    .push_str(&format!("{}Triple({})\n", indent_str, o.triple));
            }
            Op::Join(o) => {
                self.output.push_str(&format!("{}Join\n", indent_str));
                self.indent += 1;
                self.print_op(&o.left);
                self.print_op(&o.right);
                self.indent -= 1;
            }
            Op::LeftJoin(o) => {
                self.output.push_str(&format!("{}LeftJoin\n", indent_str));
                self.indent += 1;
                self.print_op(&o.left);
                self.print_op(&o.right);
                self.indent -= 1;
            }
            Op::Filter(o) => {
                self.output.push_str(&format!("{}Filter\n", indent_str));
                self.indent += 1;
                self.print_op(&o.sub_op);
                self.indent -= 1;
            }
            Op::Union(o) => {
                self.output.push_str(&format!("{}Union\n", indent_str));
                self.indent += 1;
                self.print_op(&o.left);
                self.print_op(&o.right);
                self.indent -= 1;
            }
            Op::Project(o) => {
                let vars: Vec<String> = o.vars.iter().map(|v| format!("{}", v)).collect();
                self.output
                    .push_str(&format!("{}Project({})\n", indent_str, vars.join(", ")));
                self.indent += 1;
                self.print_op(&o.sub_op);
                self.indent -= 1;
            }
            Op::Distinct(o) => {
                self.output.push_str(&format!("{}Distinct\n", indent_str));
                self.indent += 1;
                self.print_op(&o.sub_op);
                self.indent -= 1;
            }
            Op::Reduced(o) => {
                self.output.push_str(&format!("{}Reduced\n", indent_str));
                self.indent += 1;
                self.print_op(&o.sub_op);
                self.indent -= 1;
            }
            Op::Slice(o) => {
                self.output.push_str(&format!(
                    "{}Slice(offset={}, limit={:?})\n",
                    indent_str, o.offset, o.limit
                ));
                self.indent += 1;
                self.print_op(&o.sub_op);
                self.indent -= 1;
            }
            Op::Order(o) => {
                self.output.push_str(&format!("{}Order\n", indent_str));
                self.indent += 1;
                self.print_op(&o.sub_op);
                self.indent -= 1;
            }
            Op::Group(o) => {
                let vars: Vec<String> = o.group_vars.iter().map(|v| format!("{}", v)).collect();
                self.output
                    .push_str(&format!("{}Group({})\n", indent_str, vars.join(", ")));
                self.indent += 1;
                self.print_op(&o.sub_op);
                self.indent -= 1;
            }
            Op::Extend(o) => {
                self.output
                    .push_str(&format!("{}Extend({})\n", indent_str, o.var));
                self.indent += 1;
                self.print_op(&o.sub_op);
                self.indent -= 1;
            }
            Op::Minus(o) => {
                self.output.push_str(&format!("{}Minus\n", indent_str));
                self.indent += 1;
                self.print_op(&o.left);
                self.print_op(&o.right);
                self.indent -= 1;
            }
            Op::RightJoin(o) => {
                self.output.push_str(&format!("{}RightJoin\n", indent_str));
                self.indent += 1;
                self.print_op(&o.left);
                self.print_op(&o.right);
                self.indent -= 1;
            }
            Op::CrossJoin(o) => {
                self.output.push_str(&format!("{}CrossJoin\n", indent_str));
                self.indent += 1;
                self.print_op(&o.left);
                self.print_op(&o.right);
                self.indent -= 1;
            }
            Op::Intersect(o) => {
                self.output.push_str(&format!("{}Intersect\n", indent_str));
                self.indent += 1;
                self.print_op(&o.left);
                self.print_op(&o.right);
                self.indent -= 1;
            }
            Op::Table(o) => {
                self.output.push_str(&format!(
                    "{}Table({} vars, {} rows)\n",
                    indent_str,
                    o.vars.len(),
                    o.rows.len()
                ));
            }
            Op::Sequence(o) => {
                self.output.push_str(&format!("{}Sequence\n", indent_str));
                self.indent += 1;
                for sub in &o.ops {
                    self.print_op(sub);
                }
                self.indent -= 1;
            }
            Op::Disjunction(o) => {
                self.output
                    .push_str(&format!("{}Disjunction\n", indent_str));
                self.indent += 1;
                for sub in &o.ops {
                    self.print_op(sub);
                }
                self.indent -= 1;
            }
            Op::Null(_) => {
                self.output.push_str(&format!("{}Null\n", indent_str));
            }
        }
    }
}

impl Default for OpPrinter {
    fn default() -> Self {
        Self::new()
    }
}

/// Op statistics visitor
#[derive(Debug, Default)]
pub struct OpStats {
    pub bgp_count: usize,
    pub triple_count: usize,
    pub join_count: usize,
    pub filter_count: usize,
    pub union_count: usize,
    pub total_ops: usize,
}

impl OpStats {
    /// Create new stats collector
    pub fn new() -> Self {
        Self::default()
    }

    /// Collect stats from op
    pub fn collect(op: &Op) -> Self {
        let mut stats = Self::new();
        walk_op(&mut stats, op);
        stats
    }
}

impl OpVisitor for OpStats {
    fn visit_bgp(&mut self, op: &OpBGP) {
        self.bgp_count += 1;
        self.triple_count += op.triples.len();
        self.total_ops += 1;
    }

    fn visit_triple(&mut self, _op: &OpTriple) {
        self.triple_count += 1;
        self.total_ops += 1;
    }

    fn visit_join(&mut self, _op: &OpJoin) {
        self.join_count += 1;
        self.total_ops += 1;
    }

    fn visit_filter(&mut self, _op: &OpFilter) {
        self.filter_count += 1;
        self.total_ops += 1;
    }

    fn visit_union(&mut self, _op: &OpUnion) {
        self.union_count += 1;
        self.total_ops += 1;
    }

    fn visit_left_join(&mut self, _op: &OpLeftJoin) {
        self.total_ops += 1;
    }

    fn visit_project(&mut self, _op: &OpProject) {
        self.total_ops += 1;
    }

    fn visit_distinct(&mut self, _op: &OpDistinct) {
        self.total_ops += 1;
    }

    fn visit_reduced(&mut self, _op: &OpReduced) {
        self.total_ops += 1;
    }

    fn visit_slice(&mut self, _op: &OpSlice) {
        self.total_ops += 1;
    }

    fn visit_order(&mut self, _op: &OpOrder) {
        self.total_ops += 1;
    }

    fn visit_group(&mut self, _op: &OpGroup) {
        self.total_ops += 1;
    }

    fn visit_extend(&mut self, _op: &OpExtend) {
        self.total_ops += 1;
    }

    fn visit_minus(&mut self, _op: &OpMinus) {
        self.total_ops += 1;
    }

    fn visit_table(&mut self, _op: &OpTable) {
        self.total_ops += 1;
    }

    fn visit_sequence(&mut self, _op: &OpSequence) {
        self.total_ops += 1;
    }

    fn visit_disjunction(&mut self, _op: &OpDisjunction) {
        self.total_ops += 1;
    }

    fn visit_null(&mut self, _op: &OpNull) {
        self.total_ops += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::super::binding::Value;
    use super::*;

    fn make_bgp() -> OpBGP {
        let mut bgp = OpBGP::new();
        bgp.add(Triple::new(
            Pattern::Var(Var::new("s")),
            Pattern::Uri("knows".to_string()),
            Pattern::Var(Var::new("o")),
        ));
        bgp
    }

    #[test]
    fn test_walk_op() {
        let bgp = make_bgp();
        let filter = Op::Filter(OpFilter::new(FilterExpr::True, Op::BGP(bgp)));

        let mut stats = OpStats::new();
        walk_op(&mut stats, &filter);

        assert_eq!(stats.bgp_count, 1);
        assert_eq!(stats.filter_count, 1);
        assert_eq!(stats.total_ops, 2);
    }

    #[test]
    fn test_transform_copy() {
        let bgp = make_bgp();
        let op = Op::BGP(bgp);

        let mut copy = TransformCopy::new();
        let result = transform_op(&mut copy, op.clone());

        // Should produce equivalent tree
        assert!(matches!(result, Op::BGP(_)));
    }

    #[test]
    fn test_var_collector() {
        let bgp = make_bgp();
        let op = Op::BGP(bgp);

        let vars = VarCollector::collect(&op);
        assert_eq!(vars.len(), 2);
        assert!(vars.contains(&Var::new("s")));
        assert!(vars.contains(&Var::new("o")));
    }

    #[test]
    fn test_op_printer() {
        let bgp = make_bgp();
        let filter = Op::Filter(OpFilter::new(
            FilterExpr::True,
            Op::Project(OpProject::new(vec![Var::new("s")], Op::BGP(bgp))),
        ));

        let output = OpPrinter::print(&filter);
        assert!(output.contains("Filter"));
        assert!(output.contains("Project"));
        assert!(output.contains("BGP"));
    }

    #[test]
    fn test_op_stats() {
        let bgp1 = make_bgp();
        let bgp2 = make_bgp();

        let join = Op::Join(OpJoin::new(Op::BGP(bgp1), Op::BGP(bgp2)));

        let stats = OpStats::collect(&join);
        assert_eq!(stats.bgp_count, 2);
        assert_eq!(stats.join_count, 1);
        assert_eq!(stats.triple_count, 2);
    }

    #[test]
    fn test_push_filter_transform() {
        // Filter(Join(BGP1, BGP2))
        // Should push applicable filters down
        let mut bgp1 = OpBGP::new();
        bgp1.add(Triple::new(
            Pattern::Var(Var::new("s")),
            Pattern::Uri("name".to_string()),
            Pattern::Var(Var::new("name")),
        ));

        let mut bgp2 = OpBGP::new();
        bgp2.add(Triple::new(
            Pattern::Var(Var::new("s")),
            Pattern::Uri("age".to_string()),
            Pattern::Var(Var::new("age")),
        ));

        // Filter on "age" variable (only in bgp2)
        let filter = FilterExpr::Gt(
            ExprTerm::Var(Var::new("age")),
            ExprTerm::Const(Value::Integer(18)),
        );

        let op = Op::Filter(OpFilter::new(
            filter,
            Op::Join(OpJoin::new(Op::BGP(bgp1), Op::BGP(bgp2))),
        ));

        let mut transform = TransformPushFilter::new();
        let result = transform_op(&mut transform, op);

        // The filter should still be in the tree
        let stats = OpStats::collect(&result);
        assert_eq!(stats.filter_count, 1);
    }
}
