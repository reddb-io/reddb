//! `BatchProject` — thin wrapper around [`ColumnBatch::project`].
//!
//! Kept as a dedicated operator so the planner can pick projection
//! as a first-class plan node when wiring the batch path later.

use super::super::column_batch::ColumnBatch;

pub fn batch_project(batch: &ColumnBatch, column_indices: &[usize]) -> ColumnBatch {
    batch.project(column_indices)
}

#[cfg(test)]
mod tests {
    use super::super::super::column_batch::{ColumnKind, ColumnVector, Field, Schema, ValueRef};
    use super::*;
    use std::sync::Arc;

    fn wide_batch() -> ColumnBatch {
        let schema = Arc::new(Schema::new(vec![
            Field {
                name: "a".into(),
                kind: ColumnKind::Int64,
                nullable: false,
            },
            Field {
                name: "b".into(),
                kind: ColumnKind::Float64,
                nullable: false,
            },
            Field {
                name: "c".into(),
                kind: ColumnKind::Text,
                nullable: false,
            },
        ]));
        ColumnBatch::new(
            schema,
            vec![
                ColumnVector::Int64 {
                    data: vec![1, 2, 3],
                    validity: None,
                },
                ColumnVector::Float64 {
                    data: vec![1.1, 2.2, 3.3],
                    validity: None,
                },
                ColumnVector::Text {
                    data: vec!["x".into(), "y".into(), "z".into()],
                    validity: None,
                },
            ],
        )
    }

    #[test]
    fn projection_keeps_ordering() {
        let b = wide_batch();
        let p = batch_project(&b, &[2, 0]);
        assert_eq!(p.schema.index_of("c"), Some(0));
        assert_eq!(p.schema.index_of("a"), Some(1));
        assert_eq!(p.value(1, 0), ValueRef::Text("y"));
        assert_eq!(p.value(1, 1), ValueRef::Int64(2));
    }

    #[test]
    fn projection_drops_out_of_range_indices() {
        let b = wide_batch();
        let p = batch_project(&b, &[0, 99, 1]);
        // out-of-range indices are filtered (they produce no column)
        assert_eq!(p.schema.len(), 2);
    }
}
