//! 3-way JSON merge used by `VCS.merge`, `cherry_pick`, and `revert`.
//!
//! Given `base` (LCA), `ours` (current HEAD content), and `theirs` (commit
//! being merged in), produces either a cleanly merged value or a list of
//! conflicting paths. Pure data logic, no storage dependencies — unit
//! testable in isolation.
//!
//! Semantics per node:
//! - equal on both sides             → take it
//! - one side equals base            → take the other
//! - both objects                    → recurse per key union
//! - both arrays                     → length-aware element merge
//! - primitives diverge both ways    → conflict
//! - type mismatch across sides      → conflict

use crate::json::{Map, Value};

/// Dot-path into a JSON value, with `[idx]` for array indices.
/// Example: `user.roles[2].name`. Used to report conflict locations.
pub type JsonPath = String;

#[derive(Debug, Clone)]
pub struct MergeConflict {
    pub path: JsonPath,
    pub base: Value,
    pub ours: Value,
    pub theirs: Value,
}

#[derive(Debug, Clone)]
pub struct MergeResult {
    /// Best-effort merged value. Conflicting positions fall back to
    /// `ours` so callers can still persist a coherent document.
    pub merged: Value,
    pub conflicts: Vec<MergeConflict>,
}

impl MergeResult {
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Run a 3-way merge. Returns merged value + conflict list.
pub fn three_way_merge(base: &Value, ours: &Value, theirs: &Value) -> MergeResult {
    let mut conflicts = Vec::new();
    let merged = merge_at(base, ours, theirs, "", &mut conflicts);
    MergeResult { merged, conflicts }
}

fn merge_at(
    base: &Value,
    ours: &Value,
    theirs: &Value,
    path: &str,
    conflicts: &mut Vec<MergeConflict>,
) -> Value {
    if ours == theirs {
        return ours.clone();
    }
    if ours == base {
        return theirs.clone();
    }
    if theirs == base {
        return ours.clone();
    }

    match (ours, theirs) {
        (Value::Object(o), Value::Object(t)) => {
            let empty = Map::new();
            let b = match base {
                Value::Object(m) => m,
                _ => &empty,
            };
            merge_objects(b, o, t, path, conflicts)
        }
        (Value::Array(o), Value::Array(t)) => {
            let empty: Vec<Value> = Vec::new();
            let b: &Vec<Value> = match base {
                Value::Array(a) => a,
                _ => &empty,
            };
            merge_arrays(b, o, t, path, conflicts)
        }
        _ => {
            conflicts.push(MergeConflict {
                path: path.to_string(),
                base: base.clone(),
                ours: ours.clone(),
                theirs: theirs.clone(),
            });
            ours.clone()
        }
    }
}

fn merge_objects(
    base: &Map<String, Value>,
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
    path: &str,
    conflicts: &mut Vec<MergeConflict>,
) -> Value {
    let mut out = Map::new();
    let null = Value::Null;

    let mut keys: Vec<&str> = Vec::new();
    for k in ours.keys() {
        keys.push(k.as_str());
    }
    for k in theirs.keys() {
        if !ours.contains_key(k) {
            keys.push(k.as_str());
        }
    }
    for k in base.keys() {
        if !ours.contains_key(k) && !theirs.contains_key(k) {
            keys.push(k.as_str());
        }
    }

    for k in keys {
        let b = base.get(k).unwrap_or(&null);
        let o = ours.get(k).unwrap_or(&null);
        let t = theirs.get(k).unwrap_or(&null);

        let child_path = if path.is_empty() {
            k.to_string()
        } else {
            format!("{path}.{k}")
        };

        // Both sides deleted (absent + base had it).
        let ours_deleted = !ours.contains_key(k) && base.contains_key(k);
        let theirs_deleted = !theirs.contains_key(k) && base.contains_key(k);

        if ours_deleted && theirs_deleted {
            continue; // both removed — honour the removal
        }
        if ours_deleted && t == b {
            continue; // we removed, they left untouched
        }
        if theirs_deleted && o == b {
            continue; // they removed, we left untouched
        }
        if ours_deleted && t != b {
            // we removed, they modified — conflict
            conflicts.push(MergeConflict {
                path: child_path,
                base: b.clone(),
                ours: Value::Null,
                theirs: t.clone(),
            });
            out.insert(k.to_string(), t.clone());
            continue;
        }
        if theirs_deleted && o != b {
            // they removed, we modified — conflict, keep ours
            conflicts.push(MergeConflict {
                path: child_path,
                base: b.clone(),
                ours: o.clone(),
                theirs: Value::Null,
            });
            out.insert(k.to_string(), o.clone());
            continue;
        }

        let merged = merge_at(b, o, t, &child_path, conflicts);
        out.insert(k.to_string(), merged);
    }

    Value::Object(out)
}

/// Array merge: length-aware. If lengths match, element-wise recursive.
/// If they differ, the side that extended the array from base wins;
/// if both extended differently, whole-array conflict.
fn merge_arrays(
    base: &[Value],
    ours: &[Value],
    theirs: &[Value],
    path: &str,
    conflicts: &mut Vec<MergeConflict>,
) -> Value {
    // Fast path: exactly one side changed length vs base.
    let ours_same_len = ours.len() == base.len();
    let theirs_same_len = theirs.len() == base.len();

    if ours_same_len && theirs_same_len {
        // Both kept the length — element-wise 3-way.
        let mut out = Vec::with_capacity(ours.len());
        for i in 0..ours.len() {
            let child = format!("{path}[{i}]");
            out.push(merge_at(&base[i], &ours[i], &theirs[i], &child, conflicts));
        }
        return Value::Array(out);
    }

    if theirs_same_len && !ours_same_len {
        // Only we changed length — take ours.
        return Value::Array(ours.to_vec());
    }
    if ours_same_len && !theirs_same_len {
        // Only they changed length — take theirs.
        return Value::Array(theirs.to_vec());
    }

    // Both changed length differently — whole-array conflict, keep ours.
    conflicts.push(MergeConflict {
        path: path.to_string(),
        base: Value::Array(base.to_vec()),
        ours: Value::Array(ours.to_vec()),
        theirs: Value::Array(theirs.to_vec()),
    });
    Value::Array(ours.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::json;

    #[test]
    fn identical_trivial() {
        let v = json!({"a": 1});
        let r = three_way_merge(&v, &v, &v);
        assert!(r.is_clean());
        assert_eq!(r.merged, v);
    }

    #[test]
    fn one_sided_change_ours() {
        let base = json!({"a": 1});
        let ours = json!({"a": 2});
        let theirs = json!({"a": 1});
        let r = three_way_merge(&base, &ours, &theirs);
        assert!(r.is_clean());
        assert_eq!(r.merged, ours);
    }

    #[test]
    fn one_sided_change_theirs() {
        let base = json!({"a": 1});
        let ours = json!({"a": 1});
        let theirs = json!({"a": 9});
        let r = three_way_merge(&base, &ours, &theirs);
        assert!(r.is_clean());
        assert_eq!(r.merged, theirs);
    }

    #[test]
    fn disjoint_object_edits_merge_clean() {
        let base = json!({"a": 1, "b": 2});
        let ours = json!({"a": 10, "b": 2});
        let theirs = json!({"a": 1, "b": 20});
        let r = three_way_merge(&base, &ours, &theirs);
        assert!(r.is_clean());
        assert_eq!(r.merged, json!({"a": 10, "b": 20}));
    }

    #[test]
    fn conflicting_object_value() {
        let base = json!({"a": 1});
        let ours = json!({"a": 2});
        let theirs = json!({"a": 3});
        let r = three_way_merge(&base, &ours, &theirs);
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].path, "a");
    }

    #[test]
    fn nested_disjoint_edits() {
        let base = json!({"user": json!({"name": "a", "age": 30})});
        let ours = json!({"user": json!({"name": "b", "age": 30})});
        let theirs = json!({"user": json!({"name": "a", "age": 31})});
        let r = three_way_merge(&base, &ours, &theirs);
        assert!(r.is_clean());
        assert_eq!(r.merged, json!({"user": json!({"name": "b", "age": 31})}));
    }

    #[test]
    fn both_added_same_key_conflict() {
        let base = json!({});
        let ours = json!({"x": 1});
        let theirs = json!({"x": 2});
        let r = three_way_merge(&base, &ours, &theirs);
        assert_eq!(r.conflicts.len(), 1);
    }

    #[test]
    fn both_added_same_key_equal() {
        let base = json!({});
        let ours = json!({"x": 1});
        let theirs = json!({"x": 1});
        let r = three_way_merge(&base, &ours, &theirs);
        assert!(r.is_clean());
    }

    #[test]
    fn array_elementwise_disjoint() {
        let base = json!([1, 2, 3]);
        let ours = json!([10, 2, 3]);
        let theirs = json!([1, 2, 30]);
        let r = three_way_merge(&base, &ours, &theirs);
        assert!(r.is_clean());
        assert_eq!(r.merged, json!([10, 2, 30]));
    }

    #[test]
    fn array_elementwise_conflict() {
        let base = json!([1]);
        let ours = json!([2]);
        let theirs = json!([3]);
        let r = three_way_merge(&base, &ours, &theirs);
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].path, "[0]");
    }

    #[test]
    fn array_one_sided_length_change() {
        let base = json!([1, 2]);
        let ours = json!([1, 2, 3]);
        let theirs = json!([1, 2]);
        let r = three_way_merge(&base, &ours, &theirs);
        assert!(r.is_clean());
        assert_eq!(r.merged, ours);
    }

    #[test]
    fn array_both_extended_conflict() {
        let base = json!([1]);
        let ours = json!([1, 2]);
        let theirs = json!([1, 3]);
        let r = three_way_merge(&base, &ours, &theirs);
        assert_eq!(r.conflicts.len(), 1);
    }

    #[test]
    fn deletion_both_sides_clean() {
        let base = json!({"a": 1, "b": 2});
        let ours = json!({"a": 1});
        let theirs = json!({"a": 1});
        let r = three_way_merge(&base, &ours, &theirs);
        assert!(r.is_clean());
        assert_eq!(r.merged, json!({"a": 1}));
    }

    #[test]
    fn deletion_vs_modification_conflict() {
        let base = json!({"a": 1, "b": 2});
        let ours = json!({"a": 1});
        let theirs = json!({"a": 1, "b": 99});
        let r = three_way_merge(&base, &ours, &theirs);
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].path, "b");
    }

    #[test]
    fn type_mismatch_conflict() {
        let base = json!(null);
        let ours = json!({"a": 1});
        let theirs = json!([1, 2]);
        let r = three_way_merge(&base, &ours, &theirs);
        assert_eq!(r.conflicts.len(), 1);
    }
}
