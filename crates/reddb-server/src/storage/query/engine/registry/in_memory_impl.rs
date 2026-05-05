use super::*;

impl InMemoryEngine {
    /// Create empty engine
    pub fn new() -> Self {
        Self {
            data: Arc::new(HashMap::new()),
        }
    }

    /// Create with data
    pub fn with_data(data: HashMap<String, Vec<Binding>>) -> Self {
        Self {
            data: Arc::new(data),
        }
    }

    /// Execute BGP
    fn execute_bgp(&self, bgp: &OpBGP) -> Box<dyn BindingIterator> {
        // For now, return empty. Real impl would lookup in data store
        let mut bindings = Vec::new();

        // Simple pattern matching simulation
        for triple in &bgp.triples {
            if let Pattern::Uri(pred) = &triple.predicate {
                if let Some(data) = self.data.get(pred) {
                    bindings.extend(data.clone());
                }
            }
        }

        Box::new(QueryIterBase::new(bindings))
    }

    /// Execute triple
    fn execute_triple(&self, triple: &OpTriple) -> Box<dyn BindingIterator> {
        let bgp = OpBGP::from_triples(vec![triple.triple.clone()]);
        self.execute_bgp(&bgp)
    }

    /// Execute join
    fn execute_join(
        &self,
        left: Box<dyn BindingIterator>,
        right_op: Op,
    ) -> Box<dyn BindingIterator> {
        let engine = self.clone();
        let right_vars = right_op.vars();

        Box::new(QueryIterJoin::new(
            left,
            move || engine.execute_op(&right_op),
            right_vars,
        ))
    }

    /// Execute filter
    fn execute_filter(
        &self,
        sub: Box<dyn BindingIterator>,
        filter: &FilterExpr,
    ) -> Box<dyn BindingIterator> {
        let filter = filter.clone();
        Box::new(QueryIterFilter::new(sub, move |b| filter.evaluate(b)))
    }

    /// Execute union
    fn execute_union(
        &self,
        left: Box<dyn BindingIterator>,
        right: Box<dyn BindingIterator>,
    ) -> Box<dyn BindingIterator> {
        Box::new(QueryIterUnion::new(vec![left, right]))
    }

    /// Execute intersect (set intersection)
    fn execute_intersect(
        &self,
        left: Box<dyn BindingIterator>,
        mut right: Box<dyn BindingIterator>,
    ) -> Box<dyn BindingIterator> {
        // Collect right side into a set for O(1) lookups
        let mut right_bindings: Vec<Binding> = Vec::new();
        while let Ok(Some(binding)) = right.next_binding() {
            right_bindings.push(binding);
        }
        let right_set: std::collections::HashSet<_> =
            right_bindings.iter().map(binding_hash).collect();

        // Filter left side to only include bindings that appear in right
        Box::new(QueryIterFilter::new(left, move |b| {
            right_set.contains(&binding_hash(b))
        }))
    }

    /// Execute project
    fn execute_project(
        &self,
        sub: Box<dyn BindingIterator>,
        vars: &[Var],
    ) -> Box<dyn BindingIterator> {
        Box::new(QueryIterProject::new(sub, vars.to_vec()))
    }

    /// Execute distinct
    fn execute_distinct(&self, sub: Box<dyn BindingIterator>) -> Box<dyn BindingIterator> {
        Box::new(QueryIterDistinct::new(sub))
    }

    /// Execute slice
    fn execute_slice(
        &self,
        sub: Box<dyn BindingIterator>,
        offset: u64,
        limit: Option<u64>,
    ) -> Box<dyn BindingIterator> {
        Box::new(QueryIterSlice::new(sub, offset, limit))
    }

    /// Execute order
    fn execute_order(
        &self,
        sub: Box<dyn BindingIterator>,
        keys: &[OrderKey],
    ) -> Box<dyn BindingIterator> {
        let sort_keys: Vec<SortKey> = keys
            .iter()
            .filter_map(|k| {
                if let ExprTerm::Var(v) = &k.expr {
                    Some(SortKey {
                        var: v.clone(),
                        ascending: k.ascending,
                    })
                } else {
                    None
                }
            })
            .collect();

        Box::new(QueryIterSort::new(sub, sort_keys))
    }

    /// Execute table
    fn execute_table(&self, table: &OpTable) -> Box<dyn BindingIterator> {
        let mut bindings = Vec::new();

        for row in &table.rows {
            let mut builder = crate::storage::query::engine::binding::BindingBuilder::new();
            for (i, var) in table.vars.iter().enumerate() {
                if let Some(Some(value)) = row.get(i) {
                    builder = builder.add(var.clone(), value.clone());
                }
            }
            bindings.push(builder.build());
        }

        Box::new(QueryIterBase::new(bindings))
    }

    fn collect_bindings(iter: Box<dyn BindingIterator>) -> Vec<Binding> {
        let query_iter = QueryIter::new(iter);
        query_iter
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_default()
    }

    fn execute_group_op(&self, group: &OpGroup) -> Box<dyn BindingIterator> {
        let sub = self.execute_op(&group.sub_op);
        let bindings = Self::collect_bindings(sub);
        let results = Self::group_bindings(bindings, &group.group_vars, &group.aggregates);
        Box::new(QueryIterBase::new(results))
    }

    fn execute_extend_op(&self, extend: &OpExtend) -> Box<dyn BindingIterator> {
        let sub = self.execute_op(&extend.sub_op);
        let bindings = Self::collect_bindings(sub);

        let result: Vec<Binding> = bindings
            .into_iter()
            .filter_map(|binding| {
                let existing = binding.get(&extend.var).cloned();
                let evaluated = extend.expr.evaluate(&binding);

                match (existing, evaluated) {
                    (Some(current), Some(value)) => {
                        if current == value {
                            Some(binding)
                        } else {
                            None
                        }
                    }
                    (Some(_), None) => Some(binding),
                    (None, Some(value)) => Some(binding.extend(extend.var.clone(), value)),
                    (None, None) => Some(binding),
                }
            })
            .collect();

        Box::new(QueryIterBase::new(result))
    }

    fn execute_minus_op(&self, minus: &OpMinus) -> Box<dyn BindingIterator> {
        let left = Self::collect_bindings(self.execute_op(&minus.left));
        let right = Self::collect_bindings(self.execute_op(&minus.right));

        let result: Vec<Binding> = left
            .into_iter()
            .filter(|binding| {
                !right.iter().any(|candidate| {
                    bindings_share_vars(binding, candidate)
                        && bindings_compatible(binding, candidate)
                })
            })
            .collect();

        Box::new(QueryIterBase::new(result))
    }

    fn group_bindings(
        bindings: Vec<Binding>,
        group_vars: &[Var],
        aggregates: &[(Var, Aggregate)],
    ) -> Vec<Binding> {
        let mut groups: HashMap<Vec<Option<Value>>, Vec<Binding>> = HashMap::new();
        let mut group_order: Vec<Vec<Option<Value>>> = Vec::new();

        for binding in bindings {
            let key_values: Vec<Option<Value>> =
                group_vars.iter().map(|v| binding.get(v).cloned()).collect();

            if !groups.contains_key(&key_values) {
                group_order.push(key_values.clone());
            }
            groups.entry(key_values).or_default().push(binding);
        }

        let mut results = Vec::new();

        for key_values in group_order {
            let Some(group_bindings) = groups.get(&key_values) else {
                continue;
            };
            if group_bindings.is_empty() {
                continue;
            }

            let mut result = Binding::empty();

            for (idx, var) in group_vars.iter().enumerate() {
                if let Some(Some(value)) = key_values.get(idx) {
                    result = result.extend(var.clone(), value.clone());
                }
            }

            for (result_var, agg) in aggregates {
                if let Some(mut aggregator) = Self::build_aggregator(agg) {
                    for binding in group_bindings {
                        let value = Self::aggregate_value(agg, binding);
                        aggregator.accumulate(value.as_ref());
                    }
                    let agg_value = aggregator.finalize();
                    result = result.extend(result_var.clone(), agg_value);
                }
            }

            results.push(result);
        }

        results
    }

    fn build_aggregator(agg: &Aggregate) -> Option<Box<dyn Aggregator>> {
        match agg {
            Aggregate::Count(None) => Some(Box::new(CountAggregator::count_all())),
            Aggregate::Count(Some(_)) => Some(Box::new(CountAggregator::count_column())),
            Aggregate::CountDistinct(_) => Some(Box::new(CountDistinctAggregator::new())),
            Aggregate::Sum(_) => Some(Box::new(SumAggregator::new())),
            Aggregate::Avg(_) => Some(Box::new(AvgAggregator::new())),
            Aggregate::Min(_) => Some(Box::new(MinAggregator::new())),
            Aggregate::Max(_) => Some(Box::new(MaxAggregator::new())),
            Aggregate::Sample(_) => Some(Box::new(SampleAggregator::new())),
            Aggregate::GroupConcat(_, sep) => {
                Some(Box::new(GroupConcatAggregator::new(sep.clone())))
            }
        }
    }

    fn aggregate_value(agg: &Aggregate, binding: &Binding) -> Option<Value> {
        match agg {
            Aggregate::Count(None) => None,
            Aggregate::Count(Some(expr))
            | Aggregate::CountDistinct(expr)
            | Aggregate::Sum(expr)
            | Aggregate::Avg(expr)
            | Aggregate::Min(expr)
            | Aggregate::Max(expr)
            | Aggregate::Sample(expr)
            | Aggregate::GroupConcat(expr, _) => expr.evaluate(binding),
        }
    }

    /// Execute an Op recursively
    pub(crate) fn execute_op(&self, op: &Op) -> Box<dyn BindingIterator> {
        match op {
            Op::BGP(bgp) => self.execute_bgp(bgp),
            Op::Triple(triple) => self.execute_triple(triple),
            Op::Join(join) => {
                let left = self.execute_op(&join.left);
                self.execute_join(left, (*join.right).clone())
            }
            Op::LeftJoin(lj) => {
                // Simplified: execute as regular join with null extension
                let left = self.execute_op(&lj.left);
                self.execute_join(left, (*lj.right).clone())
            }
            Op::Filter(filter) => {
                let sub = self.execute_op(&filter.sub_op);
                self.execute_filter(sub, &filter.filter)
            }
            Op::Union(union) => {
                let left = self.execute_op(&union.left);
                let right = self.execute_op(&union.right);
                self.execute_union(left, right)
            }
            Op::Project(project) => {
                let sub = self.execute_op(&project.sub_op);
                self.execute_project(sub, &project.vars)
            }
            Op::Distinct(distinct) => {
                let sub = self.execute_op(&distinct.sub_op);
                self.execute_distinct(sub)
            }
            Op::Reduced(reduced) => {
                // Reduced is like distinct but weaker - for now, same impl
                let sub = self.execute_op(&reduced.sub_op);
                self.execute_distinct(sub)
            }
            Op::Slice(slice) => {
                let sub = self.execute_op(&slice.sub_op);
                self.execute_slice(sub, slice.offset, slice.limit)
            }
            Op::Order(order) => {
                let sub = self.execute_op(&order.sub_op);
                self.execute_order(sub, &order.keys)
            }
            Op::Group(group) => self.execute_group_op(group),
            Op::Extend(extend) => self.execute_extend_op(extend),
            Op::Minus(minus) => self.execute_minus_op(minus),
            Op::RightJoin(rj) => {
                // Right join: swap left/right, execute as left join, swap back
                let left = self.execute_op(&rj.left);
                let right = self.execute_op(&rj.right);
                // Simplified: execute right side, join with left
                self.execute_join(right, (*rj.left).clone())
            }
            Op::CrossJoin(cj) => {
                // Cross join: Cartesian product
                let left = self.execute_op(&cj.left);
                self.execute_join(left, (*cj.right).clone())
            }
            Op::Intersect(inter) => {
                // Set intersection: only bindings that appear in both sides
                let left = self.execute_op(&inter.left);
                let right = self.execute_op(&inter.right);
                self.execute_intersect(left, right)
            }
            Op::Table(table) => self.execute_table(table),
            Op::Sequence(seq) => {
                // Execute in sequence, join results
                if seq.ops.is_empty() {
                    return Box::new(QueryIterBase::single(Binding::empty()));
                }

                let mut result = self.execute_op(&seq.ops[0]);
                for op in seq.ops.iter().skip(1) {
                    let right = op.clone();
                    result = self.execute_join(result, right);
                }
                result
            }
            Op::Disjunction(disj) => {
                // Union all branches
                if disj.ops.is_empty() {
                    return Box::new(QueryIterBase::empty());
                }

                let iters: Vec<Box<dyn BindingIterator>> =
                    disj.ops.iter().map(|op| self.execute_op(op)).collect();

                Box::new(QueryIterUnion::new(iters))
            }
            Op::Null(_) => Box::new(QueryIterBase::empty()),
        }
    }
}
