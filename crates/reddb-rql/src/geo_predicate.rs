//! Shape recognition for the composable `GEO_WITHIN` geo predicate.
//!
//! `GEO_WITHIN(<geo-column>, POLYGON((lat lon), …))` parses into an
//! ordinary `Expr::FunctionCall`, which the filter layer wraps as
//! `<call> = TRUE`. Both the table executor (which turns a constant
//! polygon over an H3-indexed column into a covering-cell candidate
//! set) and `EXPLAIN` (which reports whether that route was taken)
//! must agree, byte for byte, on which calls qualify. They agree by
//! calling the recognizers here rather than re-deriving the shape.
//!
//! Nothing in this module decides *membership* — the inside-test lives
//! with the `SEARCH SPATIAL WITHIN POLYGON` verb's exact even-odd
//! point-in-polygon authority. These functions only answer "is this
//! call a constant-polygon GEO_WITHIN over column X, and if so, what
//! are the vertices?".

use crate::ast::{CompareOp, Expr, FieldRef};
use reddb_types::types::Value;

/// A recognised `GEO_WITHIN(<column>, POLYGON(<constant vertices>))`.
pub struct GeoWithinPredicate<'a> {
    /// The geo column named by the call's first argument.
    pub column: &'a str,
    /// `(lat, lon)` vertices in the order the user wrote them.
    pub vertices: Vec<(f64, f64)>,
}

/// The `(lat, lon)` pairs of a `POLYGON(...)` argument list, when every
/// coordinate is a numeric literal. A polygon assembled from columns,
/// parameters, or arithmetic is *not* constant: it can differ per row,
/// so no single covering-cell set describes it and callers fall back to
/// evaluating the predicate over a full scan.
pub fn literal_polygon_vertices(args: &[Expr]) -> Option<Vec<(f64, f64)>> {
    if args.len() < 6 || !args.len().is_multiple_of(2) {
        return None;
    }
    let mut vertices = Vec::with_capacity(args.len() / 2);
    for pair in args.chunks_exact(2) {
        vertices.push((literal_f64(&pair[0])?, literal_f64(&pair[1])?));
    }
    Some(vertices)
}

/// Recognise a bare `GEO_WITHIN(column, POLYGON(constant…))` call.
///
/// `None` for a non-constant polygon, a first argument that is not a
/// plain column reference, or any other function.
pub fn geo_within_call(expr: &Expr) -> Option<GeoWithinPredicate<'_>> {
    let Expr::FunctionCall { name, args, .. } = expr else {
        return None;
    };
    if !name.eq_ignore_ascii_case("GEO_WITHIN") {
        return None;
    }
    let [Expr::Column { field, .. }, polygon] = args.as_slice() else {
        return None;
    };
    let FieldRef::TableColumn { column, .. } = field else {
        return None;
    };
    let Expr::FunctionCall {
        name: polygon_name,
        args: polygon_args,
        ..
    } = polygon
    else {
        return None;
    };
    if !polygon_name.eq_ignore_ascii_case("POLYGON") {
        return None;
    }
    Some(GeoWithinPredicate {
        column: column.as_str(),
        vertices: literal_polygon_vertices(polygon_args)?,
    })
}

/// Recognise the truth test the filter layer builds for a bare boolean
/// predicate: `GEO_WITHIN(...) = TRUE`.
///
/// Callers pass the comparison's two sides in both orders (flipping the
/// operator) so `TRUE = GEO_WITHIN(...)` is recognised too. A negated
/// test is deliberately not recognised: the covering cells bound where
/// matches *can* be, which says nothing about where non-matches are.
pub fn geo_within_truth_test<'a>(
    lhs: &'a Expr,
    op: CompareOp,
    rhs: &Expr,
) -> Option<GeoWithinPredicate<'a>> {
    if !matches!(op, CompareOp::Eq) {
        return None;
    }
    if !matches!(
        rhs,
        Expr::Literal {
            value: Value::Boolean(true),
            ..
        }
    ) {
        return None;
    }
    geo_within_call(lhs)
}

fn literal_f64(expr: &Expr) -> Option<f64> {
    match expr {
        Expr::Literal {
            value: Value::Float(value),
            ..
        } => Some(*value),
        Expr::Literal {
            value: Value::Integer(value),
            ..
        } => Some(*value as f64),
        Expr::Literal {
            value: Value::UnsignedInteger(value),
            ..
        } => Some(*value as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Span;

    fn lit(value: f64) -> Expr {
        Expr::Literal {
            value: Value::Float(value),
            span: Span::synthetic(),
        }
    }

    fn polygon(coords: &[f64]) -> Expr {
        Expr::FunctionCall {
            name: "POLYGON".to_string(),
            args: coords.iter().copied().map(lit).collect(),
            span: Span::synthetic(),
        }
    }

    fn geo_within(column: &str, polygon: Expr) -> Expr {
        Expr::FunctionCall {
            name: "GEO_WITHIN".to_string(),
            args: vec![
                Expr::Column {
                    field: FieldRef::TableColumn {
                        table: String::new(),
                        column: column.to_string(),
                    },
                    span: Span::synthetic(),
                },
                polygon,
            ],
            span: Span::synthetic(),
        }
    }

    #[test]
    fn recognises_constant_polygon_call() {
        let call = geo_within("loc", polygon(&[0.0, 0.0, 1.0, 0.0, 1.0, 1.0]));
        let predicate = geo_within_call(&call).expect("constant polygon is recognised");
        assert_eq!(predicate.column, "loc");
        assert_eq!(predicate.vertices, [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]);
    }

    #[test]
    fn rejects_non_constant_polygon() {
        let mut args: Vec<Expr> = [0.0, 0.0, 1.0, 0.0, 1.0].iter().copied().map(lit).collect();
        args.push(Expr::Column {
            field: FieldRef::TableColumn {
                table: String::new(),
                column: "lon".to_string(),
            },
            span: Span::synthetic(),
        });
        let call = geo_within(
            "loc",
            Expr::FunctionCall {
                name: "POLYGON".to_string(),
                args,
                span: Span::synthetic(),
            },
        );
        assert!(geo_within_call(&call).is_none());
    }

    #[test]
    fn rejects_degenerate_vertex_count() {
        assert!(literal_polygon_vertices(&[lit(0.0), lit(0.0), lit(1.0), lit(1.0)]).is_none());
        assert!(literal_polygon_vertices(&[lit(0.0), lit(0.0), lit(1.0)]).is_none());
    }

    #[test]
    fn truth_test_only_matches_equals_true() {
        let call = geo_within("loc", polygon(&[0.0, 0.0, 1.0, 0.0, 1.0, 1.0]));
        let yes = Expr::Literal {
            value: Value::Boolean(true),
            span: Span::synthetic(),
        };
        let no = Expr::Literal {
            value: Value::Boolean(false),
            span: Span::synthetic(),
        };
        assert!(geo_within_truth_test(&call, CompareOp::Eq, &yes).is_some());
        assert!(geo_within_truth_test(&call, CompareOp::Eq, &no).is_none());
        assert!(geo_within_truth_test(&call, CompareOp::Ne, &yes).is_none());
    }
}
