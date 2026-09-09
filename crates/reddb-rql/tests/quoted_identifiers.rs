use reddb_rql::ast::{Expr, FieldRef, QueryExpr, SelectItem};
use reddb_rql::lexer::{Lexer, Token};
use reddb_rql::parser::Parser;
use reddb_types::Value;

fn literal(input: &str) -> Value {
    let sql = format!("SELECT {input}");
    let QueryExpr::Table(table) = Parser::new(&sql).expect("lexer").parse().expect("select") else {
        panic!("table")
    };
    let SelectItem::Expr {
        expr: Expr::Literal { value, .. },
        ..
    } = table.select_items[0].clone()
    else {
        panic!("literal")
    };
    value
}

#[test]
fn quoted_identifiers_are_columns_not_literals_or_contextual_keywords() {
    let mut parser =
        Parser::new(r#"SELECT "CASE", "current_user", "a""b" AS "output name" FROM "select""#)
            .expect("lex");
    let QueryExpr::Table(table) = parser.parse().expect("quoted identifiers") else {
        panic!("table query")
    };
    assert_eq!(table.table, "select");
    for (item, expected) in table
        .select_items
        .iter()
        .zip(["CASE", "current_user", "a\"b"])
    {
        let SelectItem::Expr {
            expr:
                Expr::Column {
                    field: FieldRef::TableColumn { table, column },
                    ..
                },
            ..
        } = item
        else {
            panic!("column: {item:?}")
        };
        assert!(table.is_empty());
        assert_eq!(column, expected);
    }
    let SelectItem::Expr { alias, .. } = &table.select_items[2] else {
        panic!("alias")
    };
    assert_eq!(alias.as_deref(), Some("output name"));
}

#[test]
fn json_quotes_remain_values_and_sql_single_quotes_remain_strings() {
    for (sql, expected) in [
        (r#"'text'"#, Value::text("text")),
        (
            r#"["a", "b\"c", 9007199254740993]"#,
            Value::Array(vec![
                Value::text("a"),
                Value::text("b\"c"),
                Value::Integer(9007199254740993),
            ]),
        ),
    ] {
        let value = literal(sql);
        assert_eq!(value, expected);
    }
    for sql in [
        r#"{"key":"text", "nested":["value"]}"#,
        r#"{key:"text", nested:["value"]}"#,
    ] {
        let Value::Json(value) = literal(sql) else {
            panic!("JSON value")
        };
        assert_eq!(
            std::str::from_utf8(&value).expect("UTF-8"),
            r#"{"key":"text","nested":["value"]}"#
        );
    }
}

#[test]
fn quoted_constraint_keyword_is_a_column_name() {
    let mut parser = Parser::new(
        r#"CREATE TABLE "select" ("CONSTRAINT" TEXT, "value" INT, UNIQUE ("CONSTRAINT", "value"))"#,
    )
    .expect("lexer");
    let QueryExpr::CreateTable(table) = parser.parse().expect("table") else {
        panic!("table")
    };
    assert_eq!(table.columns[0].name, "CONSTRAINT");
    assert_eq!(table.unique_constraints[0].columns, ["CONSTRAINT", "value"]);
}

#[test]
fn lexer_distinguishes_identifier_and_string_quotes() {
    let mut lexer = Lexer::new(r#""key" 'value' "a""b""#);
    assert_eq!(
        lexer.next_token().expect("identifier").token,
        Token::QuotedIdent("key".into())
    );
    assert_eq!(
        lexer.next_token().expect("string").token,
        Token::String("value".into())
    );
    assert_eq!(
        lexer.next_token().expect("escaped identifier").token,
        Token::QuotedIdent("a\"b".into())
    );
}

#[test]
fn identifier_renderer_preserves_keywords_and_escaped_names() {
    for name in [
        "ordinary",
        "select",
        "CASE",
        "CONSTRAINT",
        "CURRENT_DATE",
        "a\"b",
        "white space",
        "back\\slash",
    ] {
        let rendered = reddb_rql::renderer::render_identifier(name);
        let sql = format!("SELECT {rendered} FROM example");
        let QueryExpr::Table(table) = Parser::new(&sql)
            .expect("lexer")
            .parse()
            .expect("rendered query")
        else {
            panic!("table")
        };
        let SelectItem::Expr {
            expr:
                Expr::Column {
                    field: FieldRef::TableColumn { column, .. },
                    ..
                },
            ..
        } = &table.select_items[0]
        else {
            panic!("column")
        };
        assert_eq!(column, name);
    }
}

#[test]
fn empty_json_strings_remain_valid_but_empty_or_nul_identifiers_fail() {
    assert_eq!(literal(r#"[""]"#), Value::Array(vec![Value::text("")]));
    for sql in ["SELECT \"\"", "SELECT \"a\0b\""] {
        assert!(Parser::new(sql).expect("lexer").parse().is_err());
    }
}

#[test]
fn quoted_write_names_survive_rendering() {
    for sql in [
        r#"INSERT INTO "select" ("key name", "a""b") VALUES (1, 'x') ON CONFLICT ("key name") DO UPDATE SET "a""b" = 'y'"#,
        r#"UPDATE "select" SET "a""b" = 'value' WHERE "key name" = 1"#,
        r#"DELETE FROM "select" WHERE "key name" = 1"#,
        r#"QUEUE PUSH "queue name" 'value'"#,
    ] {
        let original = Parser::new(sql).expect("lexer").parse().expect("query");
        let rendered = reddb_rql::renderer::render(&original);
        let reparsed = Parser::new(&rendered)
            .expect("rendered lexer")
            .parse()
            .expect("rendered query");
        assert_eq!(reddb_rql::renderer::render(&reparsed), rendered);
        assert_eq!(rendered, sql);
    }
}

#[test]
fn quoted_paths_keep_separators_outside_delimiters() {
    let sql = r#"UPDATE docs SET "profile name".city = 'Lisbon' WHERE name = 'ada'"#;
    let parsed = Parser::new(sql).expect("lexer").parse().expect("path");
    assert_eq!(reddb_rql::renderer::render(&parsed), sql);
    assert!(Parser::new(r#"SELECT "literal.dot" FROM docs"#)
        .expect("lexer")
        .parse()
        .is_err());
}
