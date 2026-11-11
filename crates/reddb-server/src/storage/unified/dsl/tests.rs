//! DSL tests

use super::helpers::cosine_similarity;
use super::*;

#[test]
fn test_filter_value_from_impls() {
    let s: FilterValue = "hello".into();
    assert!(matches!(s, FilterValue::String(_)));

    let i: FilterValue = 42i32.into();
    assert!(matches!(i, FilterValue::Int(42)));

    let f: FilterValue = 2.5f64.into();
    assert!(matches!(f, FilterValue::Float(_)));
}

#[test]
fn test_cosine_similarity() {
    let a = vec![1.0, 0.0, 0.0];
    let b = vec![1.0, 0.0, 0.0];
    assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);

    let c = vec![0.0, 1.0, 0.0];
    assert!((cosine_similarity(&a, &c)).abs() < 0.001);
}
