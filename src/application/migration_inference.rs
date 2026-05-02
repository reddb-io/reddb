//! Static analysis for automatic migration dependency inference.
//!
//! Scans a migration body (raw SQL string) for collection references and
//! returns which collection names it touches. The engine uses this to infer
//! dependency edges between migrations that share a collection — without
//! requiring the developer to write explicit DEPENDS ON.
//!
//! Conservative by design: ambiguous references produce no inferred edge
//! rather than a wrong one. The developer can always add an explicit
//! DEPENDS ON to override.

use std::collections::HashSet;

/// Extract collection names referenced by a SQL body.
///
/// Recognises patterns:
/// - `FROM <name>`
/// - `INTO <name>`
/// - `TABLE <name>`
/// - `UPDATE <name>`
/// - `JOIN <name>`
///
/// Returns deduplicated, lowercased names. Names starting with `red_` are
/// excluded (system collections are not user migrations).
pub fn referenced_collections(body: &str) -> HashSet<String> {
    let tokens: Vec<&str> = body.split_ascii_whitespace().collect();
    let mut result = HashSet::new();

    let triggers = ["from", "into", "table", "update", "join", "on"];

    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i].to_ascii_lowercase();
        // Strip trailing punctuation like `;` or `(`
        let tok = tok.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');

        if triggers.contains(&tok) {
            if let Some(next) = tokens.get(i + 1) {
                let name = next
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                    .to_ascii_lowercase();
                // Exclude SQL keywords and system collections
                if !name.is_empty()
                    && !is_sql_keyword(&name)
                    && !name.starts_with("red_")
                    && name.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false)
                {
                    result.insert(name);
                }
            }
        }
        i += 1;
    }

    result
}

/// Infer dependency edges for a new migration given existing migration metadata.
///
/// `new_name`: name of the migration being created
/// `new_body`: SQL body of the new migration
/// `existing`: list of (migration_name, body) for all existing migrations
///
/// Returns a list of `(new_name, depends_on_name)` edges that should be stored
/// as inferred=true in `red_migration_deps`.
///
/// Ambiguity rule: if multiple existing migrations reference the same collection,
/// the inference is skipped for that collection (requires explicit DEPENDS ON).
pub fn infer_dependencies(
    new_name: &str,
    new_body: &str,
    existing: &[(String, String)],
) -> Vec<(String, String)> {
    let new_collections = referenced_collections(new_body);
    if new_collections.is_empty() {
        return Vec::new();
    }

    // Map collection → list of migration names that reference it
    let mut collection_to_migrations: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (name, body) in existing {
        if name == new_name {
            continue;
        }
        for col in referenced_collections(body) {
            collection_to_migrations
                .entry(col)
                .or_default()
                .push(name.clone());
        }
    }

    let mut edges = Vec::new();
    for col in &new_collections {
        if let Some(owners) = collection_to_migrations.get(col) {
            if owners.len() == 1 {
                // Unambiguous: exactly one prior migration touches this collection
                let dep = &owners[0];
                let edge = (new_name.to_string(), dep.clone());
                if !edges.contains(&edge) {
                    edges.push(edge);
                }
            }
            // Ambiguous (multiple owners): skip, require explicit DEPENDS ON
        }
    }

    edges
}

fn is_sql_keyword(name: &str) -> bool {
    matches!(
        name,
        "select" | "insert" | "update" | "delete" | "create" | "drop" | "alter"
            | "table" | "from" | "where" | "set" | "into" | "values" | "join"
            | "inner" | "outer" | "left" | "right" | "on" | "as" | "and"
            | "or" | "not" | "null" | "true" | "false" | "if" | "exists"
            | "column" | "index" | "unique" | "primary" | "key" | "foreign"
            | "references" | "cascade" | "restrict" | "default" | "constraint"
            | "add" | "rename" | "to" | "all" | "distinct" | "order"
            | "by" | "group" | "having" | "limit" | "offset" | "union"
            | "intersect" | "except" | "with" | "returning" | "in" | "like"
            | "between" | "is" | "case" | "when" | "then" | "else" | "end"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_from_clause() {
        let cols = referenced_collections("SELECT * FROM users WHERE id = 1");
        assert!(cols.contains("users"));
    }

    #[test]
    fn extracts_update_target() {
        let cols = referenced_collections("UPDATE users SET email = lower(email)");
        assert!(cols.contains("users"));
    }

    #[test]
    fn extracts_insert_into() {
        let cols = referenced_collections("INSERT INTO profiles (user_id) VALUES (1)");
        assert!(cols.contains("profiles"));
    }

    #[test]
    fn excludes_system_collections() {
        let cols = referenced_collections("SELECT * FROM red_migrations");
        assert!(!cols.contains("red_migrations"));
    }

    #[test]
    fn excludes_sql_keywords() {
        let cols = referenced_collections("CREATE TABLE users (id INT)");
        // "users" should be captured (after TABLE keyword), "create" should not
        assert!(cols.contains("users"));
        assert!(!cols.contains("create"));
    }

    #[test]
    fn infers_unambiguous_dep() {
        let existing = vec![
            ("add_email".to_string(), "ALTER TABLE users ADD COLUMN email TEXT".to_string()),
        ];
        let edges = infer_dependencies(
            "add_email_index",
            "CREATE INDEX idx_email ON users (email)",
            &existing,
        );
        assert!(edges.contains(&("add_email_index".to_string(), "add_email".to_string())));
    }

    #[test]
    fn skips_ambiguous_dep() {
        let existing = vec![
            ("mig_a".to_string(), "ALTER TABLE users ADD COLUMN a INT".to_string()),
            ("mig_b".to_string(), "ALTER TABLE users ADD COLUMN b INT".to_string()),
        ];
        // Two migrations touch "users" → ambiguous → no inferred edge
        let edges = infer_dependencies("mig_c", "UPDATE users SET a = 1", &existing);
        assert!(edges.is_empty());
    }
}
