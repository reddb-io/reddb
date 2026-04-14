//! Plan cache key normalisation — Fase 4 P1 building block.
//!
//! Normalises a raw SQL query string into a canonical cache key
//! by replacing literal tokens (integers, floats, strings,
//! booleans, null) with a single `?` placeholder. Two queries
//! that differ only in their literal values collapse to the
//! same key.
//!
//! ## Why here
//!
//! The full parameter-binding story — `Expr::Parameter(n)` in
//! the AST, a bind phase that substitutes concrete values
//! before execution, cache-hit reuse of the parsed expression
//! — requires invasive changes to every path that holds a
//! `QueryExpr`. That's Fase 4 W3+ scope.
//!
//! This module is the smallest immediately-shippable piece:
//! the normalised cache key. Today's `impl_core::execute_query`
//! keys the plan cache by raw SQL text, so `WHERE id = 1` and
//! `WHERE id = 2` produce different entries. Normalising the
//! key first means both queries hit a shared entry.
//!
//! BUT the cached entry still contains the *old* literal
//! values baked into its `QueryExpr`, so cache hits must
//! re-parse the new query and discard the cached plan's
//! AST if the literals matter for execution. The follow-up
//! commit does exactly that — `execute_query` will compare the
//! normalised form on lookup and re-parse when the cached
//! plan's literals don't match the fresh query.
//!
//! Until that follow-up, this module is the fast-path
//! building block: cheap tokenisation + literal stripping,
//! producing a stable `String` the cache can use.
//!
//! ## Algorithm
//!
//! Single-pass tokenizer-lite that walks the query character
//! by character and emits a canonical form:
//!
//! - Integers / floats: emit `?`
//! - Quoted strings (single + double): emit `?`
//! - `TRUE` / `FALSE` / `NULL` keywords (case-insensitive,
//!   word-bounded): emit `?`
//! - Everything else: copy verbatim.
//! - Whitespace runs collapse to a single space so `SELECT  a`
//!   and `SELECT a` produce the same key.
//! - Keywords are uppercased so `select` and `SELECT` match.
//!
//! The output is a best-effort canonical form. It's not a
//! formal parse — we only care about stable equivalence
//! classes, not strict correctness.

/// Normalise a raw SQL query into a cache-friendly canonical
/// form. Stable across whitespace, case, and literal values;
/// identical AST shapes collapse to the same output.
///
/// Worst case O(n) where n = input length, O(1) state. No
/// allocation beyond the output string.
pub fn normalize_cache_key(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut last_was_space = true; // suppress leading space
    while i < bytes.len() {
        let b = bytes[i];

        // Whitespace collapse.
        if b.is_ascii_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            i += 1;
            continue;
        }

        // Single-quoted string: scan to matching quote, emit `?`.
        if b == b'\'' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    // SQL escape: two consecutive quotes is a
                    // literal quote inside the string. Skip both
                    // and continue scanning.
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push('?');
            last_was_space = false;
            continue;
        }

        // Double-quoted string (identifier in SQL-92; still
        // handled as opaque here — quoted identifiers are
        // case-sensitive so we emit them verbatim).
        if b == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            out.push_str(&sql[start..i]);
            last_was_space = false;
            continue;
        }

        // Numeric literal: integer, float, or scientific.
        // Optional leading sign is NOT consumed here because it
        // could be a binary operator; we only canonicalise
        // digit-led runs.
        if b.is_ascii_digit() {
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || bytes[i] == b'.'
                    || bytes[i] == b'e'
                    || bytes[i] == b'E'
                    || bytes[i] == b'+'
                    || bytes[i] == b'-')
            {
                // Only consume + / - when immediately following
                // e / E (scientific notation exponent sign).
                if bytes[i] == b'+' || bytes[i] == b'-' {
                    let prev = if i > 0 { bytes[i - 1] } else { 0 };
                    if prev != b'e' && prev != b'E' {
                        break;
                    }
                }
                i += 1;
            }
            out.push('?');
            last_was_space = false;
            continue;
        }

        // Identifier / keyword run.
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
            {
                i += 1;
            }
            let word = &sql[start..i];
            // Case-insensitive keyword canonicalisation for the
            // three literal keywords TRUE / FALSE / NULL.
            if word.eq_ignore_ascii_case("true")
                || word.eq_ignore_ascii_case("false")
                || word.eq_ignore_ascii_case("null")
            {
                out.push('?');
            } else {
                // Uppercase the word so `select` and `SELECT`
                // collapse. This over-normalises — it also
                // uppercases column names — but plan cache
                // equivalence still holds because the column
                // names are part of the normalised form and
                // retain their identity within the query.
                for c in word.chars() {
                    out.push(c.to_ascii_uppercase());
                }
            }
            last_was_space = false;
            continue;
        }

        // Everything else (punctuation, operators, parens).
        // Emit verbatim.
        out.push(b as char);
        last_was_space = false;
        i += 1;
    }

    // Trim a single trailing space so `SELECT 1 ` and
    // `SELECT 1` collapse.
    if out.ends_with(' ') {
        out.pop();
    }

    out
}

/// Returns true when two raw SQL strings would hit the same
/// plan cache slot. Used by diagnostic tools to verify the
/// normalisation is doing its job.
pub fn same_cache_key(a: &str, b: &str) -> bool {
    normalize_cache_key(a) == normalize_cache_key(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_literals_collapse() {
        assert_eq!(
            normalize_cache_key("SELECT * FROM t WHERE id = 1"),
            normalize_cache_key("SELECT * FROM t WHERE id = 2"),
        );
    }

    #[test]
    fn string_literals_collapse() {
        assert_eq!(
            normalize_cache_key("SELECT * FROM t WHERE name = 'alice'"),
            normalize_cache_key("SELECT * FROM t WHERE name = 'bob'"),
        );
    }

    #[test]
    fn case_insensitive_keywords() {
        assert_eq!(
            normalize_cache_key("select * from t"),
            normalize_cache_key("SELECT * FROM t"),
        );
    }

    #[test]
    fn whitespace_collapses() {
        assert_eq!(
            normalize_cache_key("SELECT   *  FROM  t"),
            normalize_cache_key("SELECT * FROM t"),
        );
    }

    #[test]
    fn different_shape_different_key() {
        assert_ne!(
            normalize_cache_key("SELECT * FROM a WHERE x = 1"),
            normalize_cache_key("SELECT * FROM b WHERE x = 1"),
        );
    }

    #[test]
    fn float_and_scientific_collapse() {
        assert_eq!(
            normalize_cache_key("SELECT 1.5e10"),
            normalize_cache_key("SELECT 3.14"),
        );
    }

    #[test]
    fn null_and_boolean_are_literals() {
        assert_eq!(
            normalize_cache_key("WHERE x IS NULL"),
            normalize_cache_key("WHERE x IS TRUE"),
        );
    }

    #[test]
    fn quoted_identifiers_preserved() {
        // Double-quoted identifiers stay verbatim so
        // "col" and "other" don't collapse.
        assert_ne!(
            normalize_cache_key(r#"SELECT "col" FROM t"#),
            normalize_cache_key(r#"SELECT "other" FROM t"#),
        );
    }
}
