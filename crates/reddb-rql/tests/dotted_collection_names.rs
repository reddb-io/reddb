use reddb_rql::ast::QueryExpr;
use reddb_rql::parser::Parser;

fn parse(sql: &str) -> QueryExpr {
    Parser::new(sql)
        .expect("lexer")
        .parse()
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

// Render expression-bearing statements to ignore source positions, which
// change with quoting. Other commands in this matrix contain no expressions.
fn semantic_ast(query: &QueryExpr) -> String {
    let rendered = reddb_rql::renderer::render(query);
    if rendered.is_empty() {
        format!("{query:?}")
    } else {
        rendered
    }
}

#[test]
fn dotted_collection_spellings_have_the_same_ast() {
    for template in [
        "CREATE TABLE {} (id INT)",
        "CREATE KV {}",
        "CREATE VAULT {}",
        "CREATE DOCUMENT {}",
        "CREATE GRAPH {}",
        "CREATE VECTOR {} DIM 3",
        "CREATE QUEUE {}",
        "CREATE TIMESERIES {}",
        "CREATE METRICS {}",
        "CREATE HLL {}",
        "CREATE SKETCH {}",
        "CREATE FILTER {}",
        "INSERT INTO {} (id) VALUES (1)",
        "SELECT id FROM {}",
        "UPDATE {} SET id=2",
        "DELETE FROM {}",
        "ALTER TABLE {} ADD COLUMN note TEXT",
        "CREATE INDEX ix ON {} (id)",
        "DROP INDEX ix ON {}",
        "SHOW INDEXES ON {}",
        "SHOW SCHEMA {}",
        "DROP COLLECTION {}",
        "QUEUE PUSH {} 'value'",
        "QUEUE POP {}",
        "QUEUE LEN {}",
        "HLL COUNT {}",
        "HLL MERGE {} other.source",
        "SKETCH MERGE {} other.source",
        "QUEUE MOVE FROM other.source TO {}",
        "CREATE QUEUE jobs WITH DLQ {}",
        "GET CONFIG {} theme",
        "KV LIST {}",
        "SEARCH SIMILAR [0.1, 0.2] COLLECTION {}",
        "SEARCH HYBRID TEXT 'hello' COLLECTION {}",
        "SEARCH SPATIAL RADIUS 0 0 1 COLLECTION {} COLUMN location",
        "EVENTS BACKFILL STATUS {}",
    ] {
        let expected = semantic_ast(&parse(&template.replace("{}", "app.audit.items")));
        for name in ["\"app\".\"audit\".\"items\"", "\"app.audit.items\""] {
            let sql = template.replace("{}", name);
            assert_eq!(semantic_ast(&parse(&sql)), expected, "{sql}");
        }
    }
}

#[test]
fn collection_rendering_preserves_the_catalog_name() {
    for sql in [
        "SELECT id FROM \"app.audit.items\"",
        "INSERT INTO \"app.audit.items\" (id) VALUES (1)",
        "UPDATE \"app.audit.items\" SET id = 2",
        "DELETE FROM \"app.audit.items\"",
        "QUEUE PUSH \"app.audit.items\" 'value'",
        "SELECT id FROM \"app\".\"select\".\"white space\"",
    ] {
        let original = parse(sql);
        let rendered = reddb_rql::renderer::render(&original);
        assert_eq!(
            semantic_ast(&parse(&rendered)),
            semantic_ast(&original),
            "{sql} -> {rendered}"
        );
    }
}

#[test]
fn invalid_collection_segments_and_dotted_columns_remain_rejected() {
    for name in [
        "app..items",
        "app.",
        "app.*",
        "app.123",
        "app.'items'",
        "app.%",
        "app.!=",
        "\"app..items\"",
        "\".items\"",
        "\"app.\"",
        "\"app.*\"",
        "\"app\0items\"",
    ] {
        let sql = format!("CREATE TABLE {name} (id INT)");
        assert!(Parser::new(&sql).expect("lexer").parse().is_err(), "{sql}");
    }
    for sql in [
        "CREATE TABLE ordinary (\"column.dot\" INT)",
        "SELECT \"column.dot\" FROM ordinary",
        "SELECT id AS \"alias.dot\" FROM ordinary",
    ] {
        assert!(Parser::new(sql).expect("lexer").parse().is_err(), "{sql}");
    }
    parse("DROP COLLECTION app.*");
    parse("SELECT id FROM g.components");
}
