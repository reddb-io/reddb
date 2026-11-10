//! Conservative client classification. Unknown SQL stays on the writer.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlRoute {
    Replica,
    Primary,
    Begin,
    End,
    Session,
}

pub fn sql_route(sql: &str) -> SqlRoute {
    let Some(mut tokens) = sql_tokens(sql) else { return SqlRoute::Primary; };
    if tokens.last().is_some_and(|token| token == ";") { tokens.pop(); }
    let Some(first) = tokens.first().map(String::as_str) else { return SqlRoute::Primary; };
    if first == "BEGIN" || first == "START" && tokens.get(1).is_some_and(|token| token == "TRANSACTION") {
        return SqlRoute::Begin;
    }
    if matches!(first, "SET" | "USE" | "PREPARE" | "DECLARE" | "LISTEN") {
        return SqlRoute::Session;
    }
    if tokens.iter().any(|token| token == ";") { return SqlRoute::Primary; }
    if first == "COMMIT" || first == "ROLLBACK" && !tokens.iter().any(|token| token == "TO") {
        return SqlRoute::End;
    }
    if first != "SELECT" { return SqlRoute::Primary; }
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token.as_str(), "INSERT" | "UPDATE" | "DELETE" | "CREATE" | "ALTER" | "DROP"
            | "INTO" | "FOR" | "LOCK" | "CALL" | "EXECUTE" | "SET" | "QUEUE" | "COPY") {
            return SqlRoute::Primary;
        }
        if tokens.get(index + 1).is_some_and(|next| next == "(")
            && token.as_bytes().first().is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_' || *byte == b'#')
            && !matches!(token.as_str(), "COUNT" | "SUM" | "MIN" | "MAX" | "AVG" | "LOWER" | "UPPER"
                | "LENGTH" | "COALESCE" | "ABS" | "ROUND" | "CAST" | "IN" | "EXISTS"
                | "SELECT" | "WHERE" | "AND" | "OR" | "NOT" | "FROM" | "#value") {
            return SqlRoute::Primary;
        }
    }
    SqlRoute::Replica
}

fn sql_tokens(sql: &str) -> Option<Vec<String>> {
    if sql.len() > 1_048_576 { return None; }
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut tokens = Vec::new();
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() { index += 1; continue; }
        if bytes[index..].starts_with(b"--") {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' { index += 1; }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            let mut depth = 1;
            index += 2;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") { depth += 1; index += 2; }
                else if bytes[index..].starts_with(b"*/") { depth -= 1; index += 2; }
                else { index += 1; }
                if depth > 64 { return None; }
            }
            if depth != 0 { return None; }
            continue;
        }
        if matches!(byte, b'\'' | b'"' | b'`') {
            index += 1;
            let mut closed = false;
            while index < bytes.len() {
                if bytes[index] == b'\\' { return None; }
                let current = bytes[index];
                index += 1;
                if current == byte {
                    if bytes.get(index) == Some(&byte) { index += 1; continue; }
                    closed = true;
                    break;
                }
            }
            if !closed { return None; }
            tokens.push(if byte == b'\'' { "#value" } else { "#identifier" }.to_string());
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_') { index += 1; }
            tokens.push(sql[start..index].to_ascii_uppercase());
        } else if byte.is_ascii() {
            tokens.push(char::from(byte).to_string());
            index += 1;
        } else { return None; }
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_locks_and_unknown_functions_stay_on_writer() {
        for sql in ["UPDATE users SET name = $1", "SELECT * FROM users FOR UPDATE",
            "SELECT 1; DELETE FROM users", "SELECT nextval('counter')", "WITH x AS (DELETE FROM users) SELECT * FROM x"] {
            assert_eq!(sql_route(sql), SqlRoute::Primary, "{sql}");
        }
        assert_eq!(sql_route("/* read */ SELECT COUNT(*) FROM users;"), SqlRoute::Replica);
        assert_eq!(sql_route("SELECT 'UPDATE; DELETE' AS text"), SqlRoute::Replica);
        assert_eq!(sql_route("/* transaction */ BEGIN"), SqlRoute::Begin);
        assert_eq!(sql_route("ROLLBACK TO SAVEPOINT nested"), SqlRoute::Primary);
        assert_eq!(sql_route("COMMIT"), SqlRoute::End);
    }
}
