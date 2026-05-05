//! Interactive REPL for RedDB client.

use super::RedDBClient;
use std::io::{self, BufRead, Write};

pub async fn run_repl(client: &mut RedDBClient) {
    let stdin = io::stdin();
    let stdout = io::stdout();

    println!("Connected to {}", client.addr);
    println!("Type SQL queries, or use commands: .health .collections .stats .help .quit");
    println!();

    loop {
        print!("red> ");
        let _ = stdout.lock().flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match trimmed {
            ".quit" | ".exit" | "\\q" => break,
            ".help" | "\\?" => print_help(),
            ".health" => match client.health().await {
                Ok(s) => println!("{}", s),
                Err(e) => eprintln!("error: {}", e),
            },
            ".collections" | "\\l" => match client.collections().await {
                Ok(cols) => {
                    if cols.is_empty() {
                        println!("(no collections)");
                    } else {
                        for c in &cols {
                            println!("  {}", c);
                        }
                    }
                }
                Err(e) => eprintln!("error: {}", e),
            },
            ".stats" => match client.stats().await {
                Ok(s) => println!("{}", s),
                Err(e) => eprintln!("error: {}", e),
            },
            ".status" => match client.replication_status().await {
                Ok(s) => println!("{}", s),
                Err(e) => eprintln!("error: {}", e),
            },
            _ if trimmed.starts_with(".explain ") => {
                let sql = &trimmed[9..];
                match client.explain(sql).await {
                    Ok(s) => println!("{}", s),
                    Err(e) => eprintln!("error: {}", e),
                }
            }
            _ if trimmed.starts_with(".scan ") => {
                let parts: Vec<&str> = trimmed[6..].splitn(2, ' ').collect();
                let collection = parts[0];
                let limit = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(10u64);
                match client.scan(collection, limit).await {
                    Ok(s) => println!("{}", s),
                    Err(e) => eprintln!("error: {}", e),
                }
            }
            _ if trimmed.starts_with(".login ") => {
                let parts: Vec<&str> = trimmed[7..].splitn(2, ' ').collect();
                if parts.len() < 2 {
                    eprintln!("usage: .login <username> <password>");
                } else {
                    match client.login(parts[0], parts[1]).await {
                        Ok(s) => {
                            println!("{}", s);
                            // Attempt to extract token from the JSON response and store it.
                            // The server returns a JSON object; we try a simple parse.
                            if let Some(token) = extract_token_from_json(&s) {
                                client.set_token(token);
                                println!("(session token saved)");
                            }
                        }
                        Err(e) => eprintln!("error: {}", e),
                    }
                }
            }
            // Default: treat as SQL query
            sql => match client.query(sql).await {
                Ok(json) => println!("{}", json),
                Err(e) => eprintln!("error: {}", e),
            },
        }
    }

    println!("Bye!");
}

fn print_help() {
    println!("Commands:");
    println!("  .health          Health check");
    println!("  .collections     List collections (\\l)");
    println!("  .stats           Server statistics");
    println!("  .status          Replication status");
    println!("  .scan <coll> [n] Scan collection (default 10 items)");
    println!("  .explain <sql>   Explain query plan");
    println!("  .login <u> <p>   Login with username/password");
    println!("  .help            Show this help (\\?)");
    println!("  .quit            Exit (\\q)");
    println!();
    println!("Or type any SQL query directly:");
    println!("  SELECT * FROM users WHERE age > 21");
    println!("  SELECT * FROM any");
}

/// Best-effort extraction of a `token` field from a JSON string.
///
/// This avoids pulling in a JSON parser dependency just for the REPL;
/// the payload is expected to be a flat object like `{"token":"..."}`.
fn extract_token_from_json(json: &str) -> Option<String> {
    let needle = "\"token\"";
    let idx = json.find(needle)?;
    let rest = &json[idx + needle.len()..];
    // Skip optional whitespace and colon
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let token = &rest[..end];
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}
