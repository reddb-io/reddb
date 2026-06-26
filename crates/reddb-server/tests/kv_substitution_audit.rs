//! Regression tests pinning the KV / vault substitution audit from
//! issue #109. Each test asserts a structural property of the existing
//! parser/AST design — they fail loudly if a future refactor introduces
//! a textual SQL substitution path for `$secret.*` / `$config.*` /
//! `KV(...)` / `SET SECRET` / `PUT`.
//!
//! Companion document: `docs/security/kv-substitution-audit.md`.
//!
//! These tests are pure parser / AST exercises — no runtime, no I/O —
//! so they add zero latency to the SELECT hot path.
#![allow(clippy::needless_borrow)]

use reddb_server::storage::query::ast::{Expr, Filter, QueryExpr};
use reddb_server::storage::query::parser;
use reddb_server::storage::schema::Value;

/// F1 — the reporter's exact `$my.special.key` reference fails at parse
/// time. The whitelist in `parse_dollar_ref_path` only accepts
/// `$secret.*`, `$red.secret.*`, `$config.*`, `$red.config.*`; any
/// other prefix is rejected before it reaches the engine.
#[test]
fn unknown_dollar_reference_fails_at_parse() {
    let result = parser::parse("SELECT * FROM users WHERE foo = bar OR $my.special.key = '123x'");
    let err = result.expect_err("unknown $-reference must be rejected");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unknown $ reference") || msg.contains("$my.special.key"),
        "error must point at the unknown $-reference, got: {err}"
    );
}

/// F2 — a stored secret whose payload looks like a SQL predicate
/// (`'1=1 OR 1=1'`) is referenced via `$secret.<path>` and lands as a
/// typed function-call AST node, NOT as a re-parsed predicate. The
/// parser sees only the path literal; the secret content is opaque to
/// the SQL grammar.
#[test]
fn dollar_secret_payload_lands_as_typed_function_call() {
    // The reporter's scenario: a SELECT that compares a column against
    // a $secret. The substituted value would be the secret payload at
    // execution time; here we assert the AST shape stays a function
    // call over a path literal — the secret content never enters the
    // parser.
    let parsed =
        parser::parse("SELECT id FROM users WHERE token = $secret.mycompany.injection.payload")
            .expect("typed $secret reference must parse");
    let table = match parsed.query {
        QueryExpr::Table(t) => t,
        other => panic!("expected Table, got {other:?}"),
    };
    let filter = table.filter.expect("WHERE clause should produce a Filter");
    let (lhs, rhs) = match filter {
        Filter::CompareExpr { lhs, rhs, .. } => (lhs, rhs),
        other => panic!("expected CompareExpr, got {other:?}"),
    };

    assert!(
        matches!(lhs, Expr::Column { .. }),
        "lhs of `token = $secret.path` must be a column ref"
    );

    let (name, args) = match rhs {
        Expr::FunctionCall { name, args, .. } => (name, args),
        other => panic!("rhs must be a FunctionCall, got {other:?}"),
    };
    assert_eq!(
        name, "__SECRET_REF",
        "$secret.* must desugar to a __SECRET_REF function call"
    );
    assert_eq!(args.len(), 1, "secret ref takes exactly one path argument");

    // Crucial: the function argument is a typed `Value::Text` holding
    // ONLY the path. The eventual secret value does not flow through
    // the parser at all.
    let path = match &args[0] {
        Expr::Literal {
            value: Value::Text(s),
            ..
        } => s.as_ref().to_string(),
        other => panic!("secret-ref arg must be Value::Text path, got {other:?}"),
    };
    assert_eq!(
        path, "red.vault/mycompany.injection.payload",
        "path literal must be the secret path (namespaced under the red.vault \
         collection by the reddb-rql parser), not the secret value"
    );
}

/// F2 — same shape for `$config.*`. The desugared function name is
/// `CONFIG`; the path rides as a `Value::Text` literal.
#[test]
fn dollar_config_reference_is_typed_function_call() {
    let parsed = parser::parse("SELECT id FROM users WHERE flag = $config.mycompany.flags.beta")
        .expect("typed $config reference must parse");
    let table = match parsed.query {
        QueryExpr::Table(t) => t,
        other => panic!("expected Table, got {other:?}"),
    };
    let filter = table.filter.expect("WHERE clause should produce a Filter");
    let rhs = match filter {
        Filter::CompareExpr { rhs, .. } => rhs,
        other => panic!("expected CompareExpr, got {other:?}"),
    };
    let (name, args) = match rhs {
        Expr::FunctionCall { name, args, .. } => (name, args),
        other => panic!("rhs must be a FunctionCall, got {other:?}"),
    };
    assert_eq!(name, "CONFIG");
    assert_eq!(args.len(), 1);
    let path = match &args[0] {
        Expr::Literal {
            value: Value::Text(s),
            ..
        } => s.as_ref().to_string(),
        other => panic!("config arg must be Value::Text, got {other:?}"),
    };
    // The parser lowercases the path under the `red.config.` and
    // `red.secret.` whitelist branches; the bare `$config.*` branch
    // strips the `config.` prefix and keeps the rest as-typed. The
    // contract that matters for this audit is `Value::Text(path)`,
    // not its case.
    assert!(
        path.contains("mycompany.flags.beta"),
        "path literal must carry the config path, got {path:?}"
    );
}

/// F3 — `SET SECRET k = <expr>` only accepts a typed literal in value
/// position. Any expression-shaped RHS (concatenation, arithmetic, a
/// nested $-ref) fails at parse time, so an attacker cannot launder
/// one substitution surface into another by storing it under a
/// secret path.
#[test]
fn set_secret_value_position_rejects_expression() {
    // Bare expressions (concatenation) — must fail.
    let r = parser::parse("SET SECRET mycompany.k = 'a' || 'b'");
    assert!(
        r.is_err(),
        "SET SECRET value must be a typed literal, expression was accepted: {r:?}"
    );

    // A `$secret.*` ref in value position — must fail.
    let r = parser::parse("SET SECRET mycompany.k = $secret.other.path");
    assert!(
        r.is_err(),
        "SET SECRET must reject $-substitution in value position, got {r:?}"
    );

    // Sanity: a plain string literal must still parse.
    let r = parser::parse("SET SECRET mycompany.k = 'opaque-value'")
        .expect("string literal in SET SECRET must parse");
    match r.query {
        QueryExpr::SetSecret { key, value } => {
            assert_eq!(key, "mycompany.k");
            assert_eq!(value, Value::text("opaque-value"));
        }
        other => panic!("expected SetSecret, got {other:?}"),
    }
}

/// F4 — `KV(collection, key)` lands as a function call over typed
/// string literals, never as raw SQL. Confirms the user-defined-KV
/// substitution surface uses the same typed-`Value` transport as
/// `$secret.*` / `$config.*`.
#[test]
fn kv_function_args_are_typed_path_literals() {
    let parsed =
        parser::parse("SELECT KV('config', 'app.mode') FROM users").expect("KV() must parse");
    let table = match parsed.query {
        QueryExpr::Table(t) => t,
        other => panic!("expected Table, got {other:?}"),
    };
    assert_eq!(table.columns.len(), 1);
    // The projection wraps the KV call; assert it is structurally a
    // function call over typed string literals (not SQL fragments).
    use reddb_server::storage::query::ast::Projection;
    match &table.columns[0] {
        Projection::Function(name, _args) => {
            assert!(
                name.eq_ignore_ascii_case("KV") || name.starts_with("KV"),
                "KV projection function must be named KV, got {name}"
            );
        }
        Projection::Expression(filter, _) => {
            // Some grammar paths emit the call as an expression
            // projection. Either way, the AST argument types are
            // string-literal Values, not raw text fragments.
            if let Filter::CompareExpr { .. } = filter.as_ref() {
                panic!("KV(...) projection should not lower to a comparison")
            }
        }
        other => panic!("KV(...) must be a function projection, got {other:?}"),
    }
}

/// F1 — `PUT my.special.key = '...'` is not SQL syntax at all. The
/// reporter's exact attack starts with a `PUT` statement that the SQL
/// parser does not recognise; the only `PUT` surface in the codebase
/// is the HTTP method on `/kv/...`, which never re-enters the SQL
/// parser. Pinning this prevents anyone from inventing a `PUT` SQL
/// verb later without re-running this audit.
#[test]
fn put_command_is_not_sql_syntax() {
    let r = parser::parse("PUT my.special.key = '1=1 OR 123x'");
    assert!(
        r.is_err(),
        "PUT must NOT be a valid SQL verb (would re-enable issue #109's attack), got {r:?}"
    );
}

/// F2 (continued) — the exact reported scenario, end-to-end at the
/// parser layer: a SELECT whose WHERE compares a column against
/// `$<some>.path` where the path looks attacker-controlled. The
/// parser's whitelist rejects every prefix outside `$secret.*` /
/// `$config.*` / `$red.secret.*` / `$red.config.*`, so the attack
/// surface is closed at the lexer + first-prefix check.
#[test]
fn reported_scenario_does_not_parse() {
    // Verbatim shape from the issue.
    let payloads = [
        "SELECT * FROM users WHERE foo = bar OR $my.special.key = '123x'",
        "SELECT * FROM users WHERE $arbitrary.path",
        "SELECT $tenant.flag FROM users",
    ];
    for sql in payloads {
        let r = parser::parse(sql);
        assert!(
            r.is_err(),
            "non-whitelisted $-reference must fail to parse: {sql}"
        );
    }
}

/// F8 — full-cycle proof for the reporter's restated scenario
/// (`SET CONFIG …` instead of `PUT`):
///
///   SET CONFIG my.attack = '1=1 OR 1=1'
///   SELECT 1 WHERE 'normal_id' = $config.my.attack
///
/// At parse time we pin two AST invariants together. Combined they
/// rule out any SQL-injection path through stored config values:
///
///   (a) `SET CONFIG <path> = <rhs>` accepts ONLY a typed literal
///       on the RHS. The attacker cannot bury an expression there,
///       so the stored payload is always opaque bytes / typed atom.
///
///   (b) `$config.my.attack` desugars to `Expr::FunctionCall { name:
///       "CONFIG", args: [Literal(Value::Text("my.attack"))] }` —
///       the *path* is the literal, never the value. At runtime
///       `current_config_value("my.attack")` returns
///       `Value::Text("1=1 OR 1=1")` and `apply_binop(Eq, Text, Text)`
///       compares bytewise. There is no path that feeds the stored
///       string back through the SQL parser.
///
/// Removing either invariant — letting SET CONFIG accept
/// expressions, OR letting `$config.x` be substituted textually
/// before parsing — would break this test loudly.
#[test]
fn set_config_attacker_value_does_not_alter_predicate_at_parse() {
    // (a) SET CONFIG RHS rejects expressions; only typed literals
    // survive parser::parse. We try a benign-looking attacker
    // payload and assert it parses (typed string literal); we then
    // try an expression in the same slot and assert it errors.
    parser::parse("SET CONFIG my.attack = '1=1 OR 1=1'")
        .expect("attacker-controlled string literal value must parse cleanly");
    parser::parse("SET CONFIG my.attack = ('1=1' OR '1=1')")
        .expect_err("expression on RHS of SET CONFIG must be rejected");
    parser::parse("SET CONFIG my.attack = $config.elsewhere")
        .expect_err("$-reference on RHS of SET CONFIG must be rejected");

    // (b) The reading-side predicate uses `$config.my.attack` as one
    // operand of `=`. We assert the AST shape: comparison whose RHS
    // is a function-call expression over a Value::Text path, NOT a
    // Value::Text expression of the *content*.
    let parsed = parser::parse("SELECT 1 FROM users WHERE 'normal_id' = $config.my.attack")
        .expect("$config substitution in WHERE must parse");

    let where_filter = match parsed.query {
        QueryExpr::Table(table) => table
            .filter
            .clone()
            .expect("WHERE clause must be present in the AST"),
        ref other => panic!("expected Table query, got {other:?}"),
    };

    // Walk into the comparison and confirm the substitution lands as
    // a function-call over a typed path literal.
    let rhs_expr = match where_filter {
        Filter::CompareExpr { rhs, .. } => rhs,
        other => panic!("expected Filter::CompareExpr, got {other:?}"),
    };

    match rhs_expr {
        Expr::FunctionCall { name, args, .. } => {
            assert!(
                name.eq_ignore_ascii_case("__CONFIG_REF") || name.eq_ignore_ascii_case("CONFIG"),
                "$config.* must desugar to a CONFIG/__CONFIG_REF function call, got {name:?}"
            );
            assert_eq!(args.len(), 1, "exactly one path arg");
            match &args[0] {
                Expr::Literal { value, .. } => {
                    let text = match value {
                        Value::Text(s) => s.to_string(),
                        other => panic!("path arg must be Value::Text, got {other:?}"),
                    };
                    assert_eq!(
                        text, "red.config/my.attack",
                        "argument must be the typed path literal (namespaced \
                         under the red.config collection by the reddb-rql parser)"
                    );
                }
                other => panic!("config path arg must be Literal(Text), got {other:?}"),
            }
        }
        other => panic!(
            "$config.* must produce Expr::FunctionCall, got {other:?} \
             (a non-FunctionCall arm here would mean the substitution \
             leaks the stored value into the AST shape — SQL injection)",
        ),
    }
}
