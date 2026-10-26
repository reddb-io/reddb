//! Canonical native slot-indexed aggregate kernels over admitted values.
//! Hosts retain scan/visibility, secret input/output policy, memory admission and spill I/O.
use crate::query::runtime::projection::field_ref_name;
use reddb_rql::ast::Projection;
use reddb_types::{value_to_canonical_key, CanonicalKey, Value};

pub trait AggregateHost {
    fn compare_values(&self, left: &Value, right: &Value) -> Option<std::cmp::Ordering>;
    fn evaluate_static_projection(&self, projection: &Projection) -> Option<Value>;
}

pub fn has_aggregate_projections(projections: &[Projection]) -> bool {
    projections.iter().any(|p| {
        matches!(
            p,
            Projection::Function(name, _)
                if is_aggregate_function(base_function_name(name))
        )
    })
}

pub fn base_function_name(name: &str) -> &str {
    name.split(':').next().unwrap_or(name)
}

pub fn is_aggregate_function(name: &str) -> bool {
    matches!(
        name,
        "COUNT"
            | "AVG"
            | "SUM"
            | "MIN"
            | "MAX"
            | "STDDEV"
            | "VARIANCE"
            | "MEDIAN"
            | "PERCENTILE"
            | "GROUP_CONCAT"
            | "STRING_AGG"
            | "FIRST"
            | "LAST"
            | "ARRAY_AGG"
            | "COUNT_DISTINCT"
    )
}

pub fn aggregate_projection_result_with_host<H: AggregateHost>(
    host: &H,
    projection: &Projection,
    state: &SlottedAggState,
    plan: &CompiledAggPlan,
) -> Option<(String, Value)> {
    let Projection::Function(func, args) = projection else {
        return None;
    };
    let func_name = base_function_name(func);
    if !is_aggregate_function(func_name) {
        return None;
    }

    let col_name = aggregate_argument_key(args)?;
    let result_name = aggregate_output_name(projection, func_name, &col_name);
    let slot_key = aggregate_storage_key(args);

    let result_value = match func_name {
        "COUNT" => {
            if col_name == "*" {
                Value::Integer(state.count as i64)
            } else {
                let idx = plan.slot_for(AggStorageGroup::Count, &slot_key)?;
                Value::Integer(state.count_only[idx] as i64)
            }
        }
        "SUM" => {
            let idx = plan.slot_for(AggStorageGroup::SumCount, &slot_key)?;
            if state.sum_agg_counts[idx] == 0 {
                Value::Null
            } else {
                sum_f64_to_value(state.sums[idx])
            }
        }
        "AVG" => {
            let idx = plan.slot_for(AggStorageGroup::SumCount, &slot_key)?;
            let count = state.sum_agg_counts[idx];
            if count > 0 {
                Value::Float(state.sums[idx] / count as f64)
            } else {
                Value::Null
            }
        }
        "MIN" => {
            let idx = plan.slot_for(AggStorageGroup::Min, &slot_key)?;
            state.mins[idx].clone().unwrap_or(Value::Null)
        }
        "MAX" => {
            let idx = plan.slot_for(AggStorageGroup::Max, &slot_key)?;
            state.maxs[idx].clone().unwrap_or(Value::Null)
        }
        "VARIANCE" => {
            let idx = plan.slot_for(AggStorageGroup::SumCount, &slot_key)?;
            let n = state.sum_agg_counts[idx] as f64;
            if n > 0.0 {
                let sum = state.sums[idx];
                let sum_sq = state.sum_squares[idx];
                Value::Float(sum_sq / n - (sum / n).powi(2))
            } else {
                Value::Null
            }
        }
        "STDDEV" => {
            let idx = plan.slot_for(AggStorageGroup::SumCount, &slot_key)?;
            let n = state.sum_agg_counts[idx] as f64;
            if n > 0.0 {
                let sum = state.sums[idx];
                let sum_sq = state.sum_squares[idx];
                let variance = sum_sq / n - (sum / n).powi(2);
                Value::Float(variance.max(0.0).sqrt())
            } else {
                Value::Null
            }
        }
        "MEDIAN" => {
            let idx = plan.slot_for(AggStorageGroup::AllValues, &slot_key)?;
            let mut values = state.all_values[idx].clone();
            if values.is_empty() {
                Value::Null
            } else {
                values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let mid = values.len() / 2;
                if values.len().is_multiple_of(2) {
                    Value::Float((values[mid - 1] + values[mid]) / 2.0)
                } else {
                    Value::Float(values[mid])
                }
            }
        }
        "PERCENTILE" => {
            let pct = resolve_static_projection_number(host, args.get(1))
                .unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let idx = plan.slot_for(AggStorageGroup::AllValues, &slot_key)?;
            let mut values = state.all_values[idx].clone();
            if values.is_empty() {
                Value::Null
            } else {
                values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let index =
                    ((pct * (values.len() as f64 - 1.0)).round() as usize).min(values.len() - 1);
                Value::Float(values[index])
            }
        }
        "GROUP_CONCAT" | "STRING_AGG" => {
            let idx = plan.slot_for(AggStorageGroup::Concat, &slot_key)?;
            let values = &state.concat_values[idx];
            if values.is_empty() {
                Value::Null
            } else {
                let separator = resolve_static_projection_text(host, args.get(1))
                    .unwrap_or_else(|| ", ".to_string());
                Value::text(values.join(separator.as_str()))
            }
        }
        "FIRST" => {
            let idx = plan.slot_for(AggStorageGroup::First, &slot_key)?;
            state.first_values[idx].clone().unwrap_or(Value::Null)
        }
        "LAST" => {
            let idx = plan.slot_for(AggStorageGroup::Last, &slot_key)?;
            state.last_values[idx].clone().unwrap_or(Value::Null)
        }
        "ARRAY_AGG" => {
            let idx = plan.slot_for(AggStorageGroup::Array, &slot_key)?;
            let values = state.array_values[idx].clone();
            if values.is_empty() {
                Value::Null
            } else {
                Value::Array(values)
            }
        }
        "COUNT_DISTINCT" => {
            let idx = plan.slot_for(AggStorageGroup::Distinct, &slot_key)?;
            Value::Integer(
                state.distinct_sets[idx]
                    .as_ref()
                    .map(|s| s.len())
                    .unwrap_or(0) as i64,
            )
        }
        _ => Value::Null,
    };

    Some((result_name, result_value))
}

pub fn aggregate_argument_key(args: &[Projection]) -> Option<String> {
    if !projection_depth_allowed(args) {
        return None;
    }
    args.first().map(render_aggregate_argument_key)
}

pub fn aggregate_output_name(
    projection: &Projection,
    func_name: &str,
    column_name: &str,
) -> String {
    if let Projection::Function(name, _) = projection {
        if let Some((_, alias)) = name.split_once(':') {
            return alias.to_string();
        }
    }

    if column_name == "*" {
        format!("{}(*)", func_name.to_lowercase())
    } else {
        format!("{}({})", func_name.to_lowercase(), column_name)
    }
}

// Compiler metadata only: retain the complete typed argument (including spans)
// instead of its display label. Derived Debug distinguishes literals/fields and
// remains reflexive for NaN; it is never persisted or exposed as an output name.
fn aggregate_storage_key(arguments: &[Projection]) -> String {
    format!("{:?}", arguments.first())
}

fn render_aggregate_argument_key(arg: &Projection) -> String {
    match arg {
        Projection::Column(column) => column
            .strip_prefix("LIT:")
            .map(str::to_string)
            .unwrap_or_else(|| column.clone()),
        Projection::All => "*".to_string(),
        Projection::Alias(_, alias) => alias.clone(),
        Projection::Field(field, alias) => alias.clone().unwrap_or_else(|| field_ref_name(field)),
        Projection::Function(name, args) => {
            let rendered = args
                .iter()
                .map(render_aggregate_argument_key)
                .collect::<Vec<_>>()
                .join(",");
            format!("{}({rendered})", base_function_name(name))
        }
        Projection::Expression(_, alias) => alias.clone().unwrap_or_else(|| "expr".to_string()),
        Projection::Window { name, alias, .. } => alias.clone().unwrap_or_else(|| name.clone()),
    }
}

fn resolve_static_projection_number<H: AggregateHost>(
    host: &H,
    arg: Option<&Projection>,
) -> Option<f64> {
    let value = host.evaluate_static_projection(arg?)?;
    aggregate_value_to_f64(&value)
}

fn resolve_static_projection_text<H: AggregateHost>(
    host: &H,
    arg: Option<&Projection>,
) -> Option<String> {
    let value = host.evaluate_static_projection(arg?)?;
    Some(match value {
        Value::Null => String::new(),
        Value::Text(text) => text.to_string(),
        other => other.display_string(),
    })
}

pub fn build_aggregate_group_key(values: &[Value]) -> AggregateGroupKey {
    values
        .iter()
        .map(|value| {
            value_to_canonical_key(value)
                .map(GroupKeyPart::Canonical)
                .unwrap_or_else(|| GroupKeyPart::Rendered(group_value_key(value)))
        })
        .collect()
}

fn group_value_key(value: &Value) -> String {
    use std::fmt::Write;
    let mut buf = String::with_capacity(32);
    match value {
        Value::Null => buf.push_str("null"),
        Value::Boolean(v) => {
            buf.push_str("b:");
            let _ = write!(buf, "{v}");
        }
        Value::Integer(v) => {
            buf.push_str("i:");
            let _ = write!(buf, "{v}");
        }
        Value::UnsignedInteger(v) => {
            buf.push_str("u:");
            let _ = write!(buf, "{v}");
        }
        Value::Float(v) => {
            buf.push_str("f:");
            let _ = write!(buf, "{:016x}", v.to_bits());
        }
        Value::Text(v) => {
            buf.push_str("t:");
            buf.push_str(v);
        }
        other => {
            buf.push_str("o:");
            let _ = write!(buf, "{other:?}");
        }
    }
    buf
}

pub type AggregateGroupKey = Vec<GroupKeyPart>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GroupKeyPart {
    Canonical(CanonicalKey),
    Rendered(String),
}

// ── Slot-indexed aggregate state ─────────────────────────────────────────────
//
// Replaces the HashMap<String, T> fields in the old AggState with Vec<T>
// indexed by pre-assigned compile-time slot indices. The "plan" is compiled
// once from `all_aggregate_projections` before the hot loop; thereafter every
// accumulation step is a single array write — zero String allocation,
// zero hash lookup.
//
// Slot assignment is deduplicated by storage group and admitted argument: SUM(age) and
// AVG(age) share the same SumCount slot; MIN(price) and MAX(price) get
// separate Min and Max slots for the same column.

/// Which backing Vec within `SlottedAggState` stores a given function's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
enum AggStorageGroup {
    SumCount = 0, // sums + sum_agg_counts (+sum_squares for STDDEV/VARIANCE)
    Count = 1,    // count_only (COUNT(col))
    Min = 2,
    Max = 3,
    AllValues = 4, // MEDIAN, PERCENTILE
    Concat = 5,    // GROUP_CONCAT, STRING_AGG
    First = 6,
    Last = 7,
    Array = 8,    // ARRAY_AGG
    Distinct = 9, // COUNT_DISTINCT
}

fn func_storage_group(func_name: &str) -> Option<AggStorageGroup> {
    match func_name {
        "SUM" | "AVG" | "STDDEV" | "VARIANCE" => Some(AggStorageGroup::SumCount),
        "COUNT" => Some(AggStorageGroup::Count),
        "MIN" => Some(AggStorageGroup::Min),
        "MAX" => Some(AggStorageGroup::Max),
        "MEDIAN" | "PERCENTILE" => Some(AggStorageGroup::AllValues),
        "GROUP_CONCAT" | "STRING_AGG" => Some(AggStorageGroup::Concat),
        "FIRST" => Some(AggStorageGroup::First),
        "LAST" => Some(AggStorageGroup::Last),
        "ARRAY_AGG" => Some(AggStorageGroup::Array),
        "COUNT_DISTINCT" => Some(AggStorageGroup::Distinct),
        _ => None,
    }
}

/// Per-projection slot reference: tells the hot loop exactly which Vec index
/// to write for each aggregate projection.
#[derive(Debug, Clone, Copy)]
pub enum ProjSlot {
    /// COUNT(*) — just increment the global `state.count`.
    CountStar,
    /// Another projection owns this slot's single per-row update.
    Shared,
    /// sums[idx] + sum_agg_counts[idx] — SUM, AVG.
    SumCount(usize),
    /// SumCount + sum_squares[idx] — STDDEV, VARIANCE.
    SumCountSq(usize),
    /// count_only[idx] — COUNT(col).
    CountOnly(usize),
    Min(usize),
    Max(usize),
    AllValues(usize),
    Concat(usize),
    First(usize),
    Last(usize),
    Array(usize),
    Distinct(usize),
}

/// Compiled per-query aggregation plan: slot assignments for all projections.
pub struct CompiledAggPlan {
    /// One slot per entry in `all_aggregate_projections`.
    pub proj_slots: Vec<ProjSlot>,
    /// Vec sizes for `SlottedAggState` allocation.
    n_sum_count: usize,
    n_count: usize,
    n_min: usize,
    n_max: usize,
    n_all_values: usize,
    n_concat: usize,
    n_first: usize,
    n_last: usize,
    n_array: usize,
    n_distinct: usize,
    /// Reverse lookup for result building: (group, typed argument key) → slot_idx.
    result_slot_map: std::collections::HashMap<(AggStorageGroup, String), usize>,
}

impl CompiledAggPlan {
    pub fn compile(projections: &[Projection]) -> Option<Self> {
        if !projection_depth_allowed(projections) {
            return None;
        }
        use std::collections::HashMap;
        let mut slot_key_to_idx: HashMap<(AggStorageGroup, String), usize> = HashMap::new();
        let mut counters = [0usize; 10];
        let mut proj_slots = Vec::with_capacity(projections.len());
        // Tracks whether each SumCount slot needs sum_squares.
        let mut sum_count_needs_sq: Vec<bool> = Vec::new();

        for proj in projections {
            let Projection::Function(func, args) = proj else {
                proj_slots.push(ProjSlot::CountStar);
                continue;
            };
            let func_name = base_function_name(func);
            let col_name = aggregate_argument_key(args).unwrap_or_default();

            if func_name == "COUNT" && col_name == "*" {
                proj_slots.push(ProjSlot::CountStar);
                continue;
            }

            let Some(group) = func_storage_group(func_name) else {
                proj_slots.push(ProjSlot::CountStar);
                continue;
            };

            let key = (group, aggregate_storage_key(args));
            let mut fresh = false;
            let idx = *slot_key_to_idx.entry(key).or_insert_with(|| {
                fresh = true;
                let index = counters[group as usize];
                counters[group as usize] += 1;
                if group == AggStorageGroup::SumCount {
                    sum_count_needs_sq.push(false);
                }
                index
            });

            // STDDEV/VARIANCE need sum_squares for this slot.
            if group == AggStorageGroup::SumCount
                && (func_name == "STDDEV" || func_name == "VARIANCE")
                && idx < sum_count_needs_sq.len()
            {
                sum_count_needs_sq[idx] = true;
            }

            if !fresh {
                proj_slots.push(ProjSlot::Shared);
                continue;
            }

            let ps = match group {
                AggStorageGroup::SumCount => {
                    if func_name == "STDDEV" || func_name == "VARIANCE" {
                        ProjSlot::SumCountSq(idx)
                    } else {
                        ProjSlot::SumCount(idx)
                    }
                }
                AggStorageGroup::Count => ProjSlot::CountOnly(idx),
                AggStorageGroup::Min => ProjSlot::Min(idx),
                AggStorageGroup::Max => ProjSlot::Max(idx),
                AggStorageGroup::AllValues => ProjSlot::AllValues(idx),
                AggStorageGroup::Concat => ProjSlot::Concat(idx),
                AggStorageGroup::First => ProjSlot::First(idx),
                AggStorageGroup::Last => ProjSlot::Last(idx),
                AggStorageGroup::Array => ProjSlot::Array(idx),
                AggStorageGroup::Distinct => ProjSlot::Distinct(idx),
            };
            proj_slots.push(ps);
        }

        // The first projection owns the shared update, even when a later
        // VARIANCE/STDDEV projection introduces the need for squared values.
        for slot in &mut proj_slots {
            if let ProjSlot::SumCount(index) = slot {
                if sum_count_needs_sq[*index] {
                    *slot = ProjSlot::SumCountSq(*index);
                }
            }
        }

        Some(CompiledAggPlan {
            proj_slots,
            n_sum_count: counters[0],
            n_count: counters[1],
            n_min: counters[2],
            n_max: counters[3],
            n_all_values: counters[4],
            n_concat: counters[5],
            n_first: counters[6],
            n_last: counters[7],
            n_array: counters[8],
            n_distinct: counters[9],
            result_slot_map: slot_key_to_idx,
        })
    }

    /// Look up the slot index for a result-building call.
    fn slot_for(&self, group: AggStorageGroup, argument_key: &str) -> Option<usize> {
        self.result_slot_map
            .get(&(group, argument_key.to_string()))
            .copied()
    }
}

/// Vec-indexed replacement for the old HashMap-based `AggState`.
/// Allocated once per group; hot-path writes are direct array assignments.
#[derive(Clone)]
// Public fields are retained for the native spill codec's unchanged wire layout.
// Hosts must reconstruct this state from the same compiled plan before use.
#[doc(hidden)]
pub struct SlottedAggState {
    pub count: u64,
    pub sums: Vec<f64>,
    pub sum_agg_counts: Vec<u64>,
    pub sum_squares: Vec<f64>,
    pub count_only: Vec<u64>,
    pub mins: Vec<Option<Value>>,
    pub maxs: Vec<Option<Value>>,
    pub all_values: Vec<Vec<f64>>,
    pub concat_values: Vec<Vec<String>>,
    pub first_values: Vec<Option<Value>>,
    pub last_values: Vec<Option<Value>>,
    pub array_values: Vec<Vec<Value>>,
    pub distinct_sets: Vec<Option<std::collections::HashSet<String>>>,
}

impl SlottedAggState {
    pub fn new(plan: &CompiledAggPlan) -> Self {
        Self {
            count: 0,
            sums: vec![0.0; plan.n_sum_count],
            sum_agg_counts: vec![0; plan.n_sum_count],
            sum_squares: vec![0.0; plan.n_sum_count],
            count_only: vec![0; plan.n_count],
            mins: vec![None; plan.n_min],
            maxs: vec![None; plan.n_max],
            all_values: vec![Vec::new(); plan.n_all_values],
            concat_values: vec![Vec::new(); plan.n_concat],
            first_values: vec![None; plan.n_first],
            last_values: vec![None; plan.n_last],
            array_values: vec![Vec::new(); plan.n_array],
            distinct_sets: vec![None; plan.n_distinct],
        }
    }
}

fn update_extreme_value_slot<H: AggregateHost>(
    host: &H,
    slot: &mut Option<Value>,
    candidate: &Value,
    ordering: std::cmp::Ordering,
) {
    if matches!(candidate, Value::Null) {
        return;
    }
    match slot {
        Some(current) => {
            if host
                .compare_values(candidate, current)
                .is_some_and(|ord| ord == ordering)
            {
                *current = candidate.clone();
            }
        }
        None => {
            *slot = Some(candidate.clone());
        }
    }
}

pub fn sum_f64_to_value(f: f64) -> Value {
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
        Value::Integer(f as i64)
    } else {
        Value::Float(f)
    }
}

/// Convert values accepted by `SUM` and `AVG` into their accumulator form.
///
/// Both aggregate execution routes use this function so their accepted types
/// and IEEE 754 behavior cannot drift apart.
pub fn aggregate_value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        Value::BigInt(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        Value::Decimal(value) => Some(reddb_types::types::decimal_to_f64(*value)),
        Value::Boolean(value) => Some(if *value { 1.0 } else { 0.0 }),
        _ => None,
    }
}

// Expr/Filter trees remain subject to the host's existing parser/AST admission.
fn projection_depth_allowed(projections: &[Projection]) -> bool {
    let mut pending: Vec<_> = projections
        .iter()
        .map(|projection| (projection, 0))
        .collect();
    while let Some((projection, depth)) = pending.pop() {
        if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
            return false;
        }
        if let Projection::Function(_, arguments)
        | Projection::Window {
            args: arguments, ..
        } = projection
        {
            pending.extend(arguments.iter().map(|argument| (argument, depth + 1)));
        }
    }
    true
}

impl SlottedAggState {
    /// The slot must come from the plan that allocated this state; values must
    /// already have passed the host's secret input policy.
    pub fn accumulate<H: AggregateHost>(&mut self, host: &H, slot: &ProjSlot, val: Value) {
        let state = self;
        let num = aggregate_value_to_f64(&val);

        match slot {
            ProjSlot::CountStar | ProjSlot::Shared => {}
            ProjSlot::CountOnly(idx) => {
                if !matches!(val, Value::Null) {
                    state.count_only[*idx] += 1;
                }
            }
            ProjSlot::SumCount(idx) => {
                if let Some(n) = num {
                    state.sums[*idx] += n;
                    state.sum_agg_counts[*idx] += 1;
                }
            }
            ProjSlot::SumCountSq(idx) => {
                if let Some(n) = num {
                    state.sums[*idx] += n;
                    state.sum_agg_counts[*idx] += 1;
                    state.sum_squares[*idx] += n * n;
                }
            }
            ProjSlot::Min(idx) => {
                update_extreme_value_slot(
                    host,
                    &mut state.mins[*idx],
                    &val,
                    std::cmp::Ordering::Less,
                );
            }
            ProjSlot::Max(idx) => {
                update_extreme_value_slot(
                    host,
                    &mut state.maxs[*idx],
                    &val,
                    std::cmp::Ordering::Greater,
                );
            }
            ProjSlot::AllValues(idx) => {
                if let Some(n) = num {
                    state.all_values[*idx].push(n);
                }
            }
            ProjSlot::Concat(idx) => {
                if !matches!(val, Value::Null) {
                    let text: String = match &val {
                        Value::Text(s) => s.to_string(),
                        other => other.display_string(),
                    };
                    state.concat_values[*idx].push(text);
                }
            }
            ProjSlot::First(idx) => {
                if state.first_values[*idx].is_none() {
                    state.first_values[*idx] = Some(val);
                }
            }
            ProjSlot::Last(idx) => {
                state.last_values[*idx] = Some(val);
            }
            ProjSlot::Array(idx) => {
                state.array_values[*idx].push(val);
            }
            ProjSlot::Distinct(idx) => {
                if !matches!(val, Value::Null) {
                    state.distinct_sets[*idx]
                        .get_or_insert_with(std::collections::HashSet::new)
                        .insert(group_value_key(&val));
                }
            }
        }
    }
    /// Merge states from the same plan in batch order, retaining native FIRST/LAST rules.
    pub fn merge_with_host<H: AggregateHost>(&mut self, host: &H, other: Self) {
        let s = self;
        let o = other;
        s.count += o.count;
        for (i, v) in o.sums.into_iter().enumerate() {
            if i < s.sums.len() {
                s.sums[i] += v;
            }
        }
        for (i, v) in o.sum_agg_counts.into_iter().enumerate() {
            if i < s.sum_agg_counts.len() {
                s.sum_agg_counts[i] += v;
            }
        }
        for (i, v) in o.sum_squares.into_iter().enumerate() {
            if i < s.sum_squares.len() {
                s.sum_squares[i] += v;
            }
        }
        for (i, v) in o.count_only.into_iter().enumerate() {
            if i < s.count_only.len() {
                s.count_only[i] += v;
            }
        }
        for (i, candidate) in o.mins.into_iter().enumerate() {
            if i < s.mins.len() {
                if let Some(c) = candidate {
                    update_extreme_value_slot(host, &mut s.mins[i], &c, std::cmp::Ordering::Less);
                }
            }
        }
        for (i, candidate) in o.maxs.into_iter().enumerate() {
            if i < s.maxs.len() {
                if let Some(c) = candidate {
                    update_extreme_value_slot(
                        host,
                        &mut s.maxs[i],
                        &c,
                        std::cmp::Ordering::Greater,
                    );
                }
            }
        }
        for (i, v) in o.all_values.into_iter().enumerate() {
            if i < s.all_values.len() {
                s.all_values[i].extend(v);
            }
        }
        for (i, v) in o.concat_values.into_iter().enumerate() {
            if i < s.concat_values.len() {
                s.concat_values[i].extend(v);
            }
        }
        // FIRST: keep self (first batch wins)
        for (i, v) in o.first_values.into_iter().enumerate() {
            if i < s.first_values.len() && s.first_values[i].is_none() {
                s.first_values[i] = v;
            }
        }
        // LAST: overwrite with other (later batch)
        for (i, v) in o.last_values.into_iter().enumerate() {
            if i < s.last_values.len() && v.is_some() {
                s.last_values[i] = v;
            }
        }
        for (i, v) in o.array_values.into_iter().enumerate() {
            if i < s.array_values.len() {
                s.array_values[i].extend(v);
            }
        }
        for (i, set_opt) in o.distinct_sets.into_iter().enumerate() {
            if i < s.distinct_sets.len() {
                if let Some(set) = set_opt {
                    s.distinct_sets[i]
                        .get_or_insert_with(std::collections::HashSet::new)
                        .extend(set);
                }
            }
        }
    }
}
