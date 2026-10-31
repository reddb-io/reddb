use reddb_file::native_value::{read_native_value_with_budget, write_native_value};
use reddb_types::Value;

#[test]
fn compressed_native_text_cannot_expand_past_decode_budget() {
    let mut bytes = Vec::new();
    write_native_value(&mut bytes, &Value::text("x".repeat(32 * 1024)));
    assert!(
        bytes.len() < 4096,
        "exercise native value compression rather than record overflow"
    );
    assert!(
        read_native_value_with_budget(&bytes, &mut 0, 4096).is_err(),
        "reject the expanded text before allocation"
    );
}

#[test]
fn native_array_capacity_is_admitted_before_allocation() {
    let mut bytes = Vec::new();
    write_native_value(&mut bytes, &Value::Array(vec![Value::Null; 1024]));
    assert!(bytes.len() < 4096);
    assert!(
        read_native_value_with_budget(&bytes, &mut 0, 4096).is_err(),
        "encoded null tags must not hide the size of the Value vector"
    );
}

#[test]
fn nested_native_values_roundtrip_with_sufficient_allowance() {
    let value = Value::Array(vec![
        Value::Array(vec![Value::Integer(7), Value::text("abc")]),
        Value::Blob(vec![1, 2, 3]),
    ]);
    let mut bytes = Vec::new();
    write_native_value(&mut bytes, &value);
    assert_eq!(
        read_native_value_with_budget(&bytes, &mut 0, 128 * 1024).expect("bounded roundtrip"),
        value
    );
}

#[test]
fn budget_denial_preserves_native_value_position_for_retry() {
    let mut bytes = Vec::new();
    let original = Value::text("x".repeat(32 * 1024));
    write_native_value(&mut bytes, &original);
    let mut position = 0;
    assert!(read_native_value_with_budget(&bytes, &mut position, 4096).is_err());
    assert_eq!(position, 0);
    assert_eq!(
        read_native_value_with_budget(&bytes, &mut position, 128 * 1024).expect("retry"),
        original
    );
    assert_eq!(position, bytes.len());
}
