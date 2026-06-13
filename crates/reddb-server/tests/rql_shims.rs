use reddb_server::storage::query as server_query;

fn assert_same_type<T>(_: &T, _: &T) {}

#[test]
fn server_query_frontend_is_backed_by_reddb_rql() {
    let mut server_lexer = server_query::Lexer::new("SELECT * FROM users");
    let mut rql_lexer = reddb_rql::Lexer::new("SELECT * FROM users");
    let server_tokens = server_lexer.tokenize().expect("server shim lexes");
    let rql_tokens = rql_lexer.tokenize().expect("rql crate lexes");
    assert_same_type(&server_tokens, &rql_tokens);
    let server_token_kinds: Vec<_> = server_tokens
        .into_iter()
        .map(|spanned| spanned.token)
        .collect();
    let rql_token_kinds: Vec<_> = rql_tokens
        .into_iter()
        .map(|spanned| spanned.token)
        .collect();
    assert_eq!(server_token_kinds, rql_token_kinds);

    let server_query = server_query::parse("SELECT * FROM users").expect("server shim parses");
    let rql_query = reddb_rql::parse("SELECT * FROM users").expect("rql crate parses");
    assert_same_type(&server_query, &rql_query);
    assert_eq!(format!("{server_query:?}"), format!("{rql_query:?}"));

    let server_frontend =
        server_query::parse_frontend("RESET TENANT").expect("server shim routes frontend command");
    let rql_frontend =
        reddb_rql::parse_frontend("RESET TENANT").expect("rql crate routes frontend command");
    assert_same_type(&server_frontend, &rql_frontend);
    assert_eq!(format!("{server_frontend:?}"), format!("{rql_frontend:?}"));

    let server_expr: server_query::QueryExpr = server_frontend.into_query_expr();
    let rql_expr: reddb_rql::ast::QueryExpr = rql_frontend.into_query_expr();
    assert_same_type(&server_expr, &rql_expr);
    assert!(matches!(
        server_expr,
        server_query::QueryExpr::SetTenant(None)
    ));
}
