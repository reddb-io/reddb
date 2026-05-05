//! Columnar batch representation.
//!
//! A [`ColumnBatch`] is an immutable slice of up to [`BATCH_SIZE`]
//! rows stored as one typed [`ColumnVector`] per column. Compared
//! with the row-at-a-time `Binding` trail the Volcano iterators
//! produce, the columnar layout unlocks tight inner loops the
//! compiler can auto-vectorise and, eventually, explicit SIMD.

use std::sync::Arc;

/// Default vector width. Matches ClickHouse's `max_block_size`
/// lower bound and fits comfortably in L2 cache for f64 columns
/// (2048 × 8 B = 16 KiB per column).
pub const BATCH_SIZE: usize = 2048;

/// Column type markers. Kept deliberately small — the batch layer
/// doesn't need the full `schema::Value` enum to execute arithmetic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnKind {
    Int64,
    Float64,
    Bool,
    Text,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub kind: ColumnKind,
    /// `true` when `ColumnVector` slots of this column may contain
    /// `None`. False lets the operator layer skip null checks.
    pub nullable: bool,
}

#[derive(Debug, Clone)]
pub struct Schema {
    fields: Vec<Field>,
}

impl Schema {
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields }
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
    }

    pub fn field(&self, idx: usize) -> Option<&Field> {
        self.fields.get(idx)
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn with_subset(&self, indices: &[usize]) -> Self {
        Self {
            fields: indices
                .iter()
                .filter_map(|i| self.fields.get(*i).cloned())
                .collect(),
        }
    }
}

/// Typed column storage. Nullable columns carry the validity bits
/// alongside the payload so `Option` isn't materialised row-by-row.
#[derive(Debug, Clone)]
pub enum ColumnVector {
    Int64 {
        data: Vec<i64>,
        validity: Option<Vec<bool>>,
    },
    Float64 {
        data: Vec<f64>,
        validity: Option<Vec<bool>>,
    },
    Bool {
        data: Vec<bool>,
        validity: Option<Vec<bool>>,
    },
    Text {
        data: Vec<String>,
        validity: Option<Vec<bool>>,
    },
}

impl ColumnVector {
    pub fn len(&self) -> usize {
        match self {
            ColumnVector::Int64 { data, .. } => data.len(),
            ColumnVector::Float64 { data, .. } => data.len(),
            ColumnVector::Bool { data, .. } => data.len(),
            ColumnVector::Text { data, .. } => data.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn kind(&self) -> ColumnKind {
        match self {
            ColumnVector::Int64 { .. } => ColumnKind::Int64,
            ColumnVector::Float64 { .. } => ColumnKind::Float64,
            ColumnVector::Bool { .. } => ColumnKind::Bool,
            ColumnVector::Text { .. } => ColumnKind::Text,
        }
    }

    pub fn is_valid(&self, idx: usize) -> bool {
        let validity = match self {
            ColumnVector::Int64 { validity, .. } => validity.as_ref(),
            ColumnVector::Float64 { validity, .. } => validity.as_ref(),
            ColumnVector::Bool { validity, .. } => validity.as_ref(),
            ColumnVector::Text { validity, .. } => validity.as_ref(),
        };
        validity
            .map(|v| v.get(idx).copied().unwrap_or(false))
            .unwrap_or(true)
    }

    pub fn take_indices(&self, indices: &[usize]) -> ColumnVector {
        match self {
            ColumnVector::Int64 { data, validity } => {
                let new_data: Vec<i64> = indices.iter().map(|i| data[*i]).collect();
                let new_validity = validity.as_ref().map(|v| {
                    indices
                        .iter()
                        .map(|i| *v.get(*i).unwrap_or(&true))
                        .collect()
                });
                ColumnVector::Int64 {
                    data: new_data,
                    validity: new_validity,
                }
            }
            ColumnVector::Float64 { data, validity } => {
                let new_data: Vec<f64> = indices.iter().map(|i| data[*i]).collect();
                let new_validity = validity.as_ref().map(|v| {
                    indices
                        .iter()
                        .map(|i| *v.get(*i).unwrap_or(&true))
                        .collect()
                });
                ColumnVector::Float64 {
                    data: new_data,
                    validity: new_validity,
                }
            }
            ColumnVector::Bool { data, validity } => {
                let new_data: Vec<bool> = indices.iter().map(|i| data[*i]).collect();
                let new_validity = validity.as_ref().map(|v| {
                    indices
                        .iter()
                        .map(|i| *v.get(*i).unwrap_or(&true))
                        .collect()
                });
                ColumnVector::Bool {
                    data: new_data,
                    validity: new_validity,
                }
            }
            ColumnVector::Text { data, validity } => {
                let new_data: Vec<String> = indices.iter().map(|i| data[*i].clone()).collect();
                let new_validity = validity.as_ref().map(|v| {
                    indices
                        .iter()
                        .map(|i| *v.get(*i).unwrap_or(&true))
                        .collect()
                });
                ColumnVector::Text {
                    data: new_data,
                    validity: new_validity,
                }
            }
        }
    }
}

/// Value pulled out of a batch at `(column, row)` — kept tiny so
/// operator predicates can consume it without allocating.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueRef<'a> {
    Int64(i64),
    Float64(f64),
    Bool(bool),
    Text(&'a str),
    Null,
}

impl<'a> ValueRef<'a> {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            ValueRef::Int64(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            ValueRef::Float64(v) => Some(*v),
            ValueRef::Int64(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ValueRef::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ValueRef::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, ValueRef::Null)
    }
}

#[derive(Debug, Clone)]
pub struct ColumnBatch {
    pub schema: Arc<Schema>,
    pub columns: Vec<ColumnVector>,
    pub len: usize,
}

impl ColumnBatch {
    pub fn new(schema: Arc<Schema>, columns: Vec<ColumnVector>) -> Self {
        let len = columns.first().map(|c| c.len()).unwrap_or(0);
        debug_assert!(
            columns.iter().all(|c| c.len() == len),
            "column lengths diverge in batch construction"
        );
        debug_assert_eq!(
            schema.len(),
            columns.len(),
            "schema / column count mismatch"
        );
        Self {
            schema,
            columns,
            len,
        }
    }

    pub fn empty(schema: Arc<Schema>) -> Self {
        let columns = schema
            .fields()
            .iter()
            .map(|f| match f.kind {
                ColumnKind::Int64 => ColumnVector::Int64 {
                    data: Vec::new(),
                    validity: None,
                },
                ColumnKind::Float64 => ColumnVector::Float64 {
                    data: Vec::new(),
                    validity: None,
                },
                ColumnKind::Bool => ColumnVector::Bool {
                    data: Vec::new(),
                    validity: None,
                },
                ColumnKind::Text => ColumnVector::Text {
                    data: Vec::new(),
                    validity: None,
                },
            })
            .collect();
        Self {
            schema,
            columns,
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Fetch a single cell. Used by predicates that aren't SIMD-able.
    pub fn value(&self, row: usize, column: usize) -> ValueRef<'_> {
        if row >= self.len || column >= self.columns.len() {
            return ValueRef::Null;
        }
        let col = &self.columns[column];
        if !col.is_valid(row) {
            return ValueRef::Null;
        }
        match col {
            ColumnVector::Int64 { data, .. } => ValueRef::Int64(data[row]),
            ColumnVector::Float64 { data, .. } => ValueRef::Float64(data[row]),
            ColumnVector::Bool { data, .. } => ValueRef::Bool(data[row]),
            ColumnVector::Text { data, .. } => ValueRef::Text(data[row].as_str()),
        }
    }

    /// Build a new batch keeping only the rows at `indices`.
    pub fn take(&self, indices: &[usize]) -> ColumnBatch {
        let columns = self
            .columns
            .iter()
            .map(|c| c.take_indices(indices))
            .collect();
        ColumnBatch {
            schema: Arc::clone(&self.schema),
            columns,
            len: indices.len(),
        }
    }

    /// Project a subset of columns (by index) into a narrower batch.
    pub fn project(&self, indices: &[usize]) -> ColumnBatch {
        let new_schema = Arc::new(self.schema.with_subset(indices));
        let columns = indices
            .iter()
            .filter_map(|i| self.columns.get(*i).cloned())
            .collect();
        ColumnBatch {
            schema: new_schema,
            columns,
            len: self.len,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field {
                name: "id".into(),
                kind: ColumnKind::Int64,
                nullable: false,
            },
            Field {
                name: "value".into(),
                kind: ColumnKind::Float64,
                nullable: false,
            },
            Field {
                name: "name".into(),
                kind: ColumnKind::Text,
                nullable: true,
            },
        ]))
    }

    fn batch_of(n: usize) -> ColumnBatch {
        let schema = simple_schema();
        let ids: Vec<i64> = (0..n as i64).collect();
        let values: Vec<f64> = (0..n).map(|i| i as f64 * 1.5).collect();
        let names: Vec<String> = (0..n).map(|i| format!("row-{i}")).collect();
        ColumnBatch::new(
            schema,
            vec![
                ColumnVector::Int64 {
                    data: ids,
                    validity: None,
                },
                ColumnVector::Float64 {
                    data: values,
                    validity: None,
                },
                ColumnVector::Text {
                    data: names,
                    validity: None,
                },
            ],
        )
    }

    #[test]
    fn schema_lookup_by_name_returns_index() {
        let s = simple_schema();
        assert_eq!(s.index_of("id"), Some(0));
        assert_eq!(s.index_of("value"), Some(1));
        assert_eq!(s.index_of("missing"), None);
    }

    #[test]
    fn value_access_by_row_and_column() {
        let b = batch_of(5);
        assert_eq!(b.value(0, 0), ValueRef::Int64(0));
        assert_eq!(b.value(3, 1), ValueRef::Float64(4.5));
        assert_eq!(b.value(4, 2), ValueRef::Text("row-4"));
    }

    #[test]
    fn value_out_of_range_yields_null() {
        let b = batch_of(3);
        assert!(b.value(99, 0).is_null());
        assert!(b.value(0, 99).is_null());
    }

    #[test]
    fn take_produces_reduced_batch_preserving_schema() {
        let b = batch_of(10);
        let taken = b.take(&[0, 2, 4]);
        assert_eq!(taken.len(), 3);
        assert_eq!(taken.value(1, 0), ValueRef::Int64(2));
        assert_eq!(taken.value(2, 1), ValueRef::Float64(6.0));
    }

    #[test]
    fn project_drops_unwanted_columns() {
        let b = batch_of(4);
        let p = b.project(&[0, 2]);
        assert_eq!(p.schema.len(), 2);
        assert_eq!(p.schema.index_of("value"), None);
        assert_eq!(p.value(2, 0), ValueRef::Int64(2));
    }

    #[test]
    fn validity_bits_mask_nulls() {
        let col = ColumnVector::Int64 {
            data: vec![1, 2, 3],
            validity: Some(vec![true, false, true]),
        };
        assert!(col.is_valid(0));
        assert!(!col.is_valid(1));
        assert!(col.is_valid(2));
    }

    #[test]
    fn batch_size_constant_is_power_of_two() {
        assert_eq!(BATCH_SIZE & (BATCH_SIZE - 1), 0);
        assert!(BATCH_SIZE >= 1024);
    }
}
