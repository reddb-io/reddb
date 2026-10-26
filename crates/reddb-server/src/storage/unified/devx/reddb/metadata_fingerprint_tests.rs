use super::*;

fn assert_rendered_fingerprint<T: Debug>(value: &T) {
    let mut expected = fnv1a_seed();
    fnv1a_hash_bytes(&mut expected, b"existing-prefix");
    let mut actual = expected;
    fnv1a_hash_bytes(&mut expected, format!("{value:?}").as_bytes());
    fnv1a_hash_value(&mut actual, value);
    assert_eq!(actual, expected, "persisted fingerprint bytes changed");
}

#[test]
fn metadata_fingerprints_preserve_rendered_native_rows() {
    for text in [
        String::new(),
        "plain ASCII".to_owned(),
        "áβ😃\n\t\"quotes\"\\\0".to_owned(),
        "large field with spaces ".repeat(16384),
    ] {
        let mut entity = UnifiedEntity::table_row(
            EntityId::new(7),
            "fingerprint_rows",
            7,
            vec![Value::Integer(-7), Value::text(text.clone())],
        );
        assert_rendered_fingerprint(&entity.kind);
        assert_rendered_fingerprint(&entity.data);
        let EntityData::Row(row) = &mut entity.data else {
            panic!("native table row");
        };
        row.named = Some(HashMap::from([
            ("id".to_owned(), Value::Integer(-7)),
            ("payload".to_owned(), Value::text(text)),
        ]));
        assert_rendered_fingerprint(&entity.data);
    }
    assert_rendered_fingerprint(&u64::MAX);
    assert_rendered_fingerprint(&i64::MIN);
    assert_rendered_fingerprint(&[f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -0.0]);
}

#[test]
fn fingerprint_formatter_preserves_chunk_boundaries_and_unicode() {
    struct Chunks;
    impl Debug for Chunks {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("prefix")?;
            formatter.write_str("")?;
            formatter.write_str("áβ😃")?;
            formatter.write_str("\n\t\"\\")?;
            formatter.write_str("suffix")
        }
    }
    assert_rendered_fingerprint(&Chunks);
    let mut expected = fnv1a_seed();
    let mut actual = expected;
    for value in ["first", "áβ😃", "", "last"] {
        fnv1a_hash_bytes(&mut expected, format!("{value:?}").as_bytes());
        fnv1a_hash_value(&mut actual, &value);
    }
    assert_eq!(
        actual, expected,
        "sequential fields must preserve the running state"
    );
}
