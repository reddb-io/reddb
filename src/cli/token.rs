/// Pure lexer that tokenizes raw CLI arguments into typed tokens.
///
/// No schema knowledge -- just lexical classification. The tokenizer
/// distinguishes positionals, long flags, short flags, short clusters,
/// and the end-of-options separator (`--`).

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// Bare positional value (no leading dash)
    Positional(String),
    /// Long flag: --flag (no value) or --flag=value
    LongFlag { name: String, value: Option<String> },
    /// Short flag: -f (no value) or -f=value
    ShortFlag { name: char, value: Option<String> },
    /// Short cluster: -abc (multiple short flags combined)
    ShortCluster(Vec<char>),
    /// End-of-options separator: --
    EndOfOptions,
}

/// Tokenize a slice of CLI arguments into a vector of typed tokens.
///
/// Rules:
/// 1. `--` alone emits `EndOfOptions`; everything after becomes `Positional`.
/// 2. `--name` or `--name=val` emits `LongFlag`.
/// 3. `-x` emits `ShortFlag`; `-x=val` emits `ShortFlag` with value;
///    `-abc` (len > 2, no `=`) emits `ShortCluster`.
/// 4. `-` alone or `-42` (dash + digit) emits `Positional`.
/// 5. Everything else emits `Positional`.
pub fn tokenize(args: &[String]) -> Vec<Token> {
    let mut tokens = Vec::with_capacity(args.len());
    let mut past_eoo = false;

    for arg in args {
        // After `--`, everything is positional regardless of prefix.
        if past_eoo {
            tokens.push(Token::Positional(arg.clone()));
            continue;
        }

        // Exact `--` is the end-of-options sentinel.
        if arg == "--" {
            tokens.push(Token::EndOfOptions);
            past_eoo = true;
            continue;
        }

        // Long flags: starts with `--` (already ruled out bare `--` above).
        if let Some(rest) = arg.strip_prefix("--") {
            if let Some(eq_pos) = rest.find('=') {
                tokens.push(Token::LongFlag {
                    name: rest[..eq_pos].to_string(),
                    value: Some(rest[eq_pos + 1..].to_string()),
                });
            } else {
                tokens.push(Token::LongFlag {
                    name: rest.to_string(),
                    value: None,
                });
            }
            continue;
        }

        // Short flags / clusters: starts with `-`, length > 1.
        if arg.starts_with('-') && arg.len() > 1 {
            let chars: Vec<char> = arg.chars().collect();

            // `-42` -- dash followed by a digit is a negative number, treat as positional.
            if chars[1].is_ascii_digit() {
                tokens.push(Token::Positional(arg.clone()));
                continue;
            }

            let flag_char = chars[1];

            // Exactly `-x` (length 2): single short flag, no value.
            if arg.len() == 2 {
                tokens.push(Token::ShortFlag {
                    name: flag_char,
                    value: None,
                });
                continue;
            }

            // `-x=` (length 3, third char is `=`): short flag with empty value.
            if arg.len() == 3 && chars[2] == '=' {
                tokens.push(Token::ShortFlag {
                    name: flag_char,
                    value: Some(String::new()),
                });
                continue;
            }

            // `-x=val`: short flag with value (split at first `=`).
            if let Some(eq_pos) = arg.find('=') {
                tokens.push(Token::ShortFlag {
                    name: flag_char,
                    value: Some(arg[eq_pos + 1..].to_string()),
                });
                continue;
            }

            // `-abc` (length > 2, no `=`): short cluster.
            tokens.push(Token::ShortCluster(chars[1..].to_vec()));
            continue;
        }

        // Everything else is positional (bare words, `-` alone, empty strings).
        tokens.push(Token::Positional(arg.clone()));
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> String {
        v.to_string()
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|a| s(a)).collect()
    }

    #[test]
    fn test_long_flag() {
        let tokens = tokenize(&args(&["--verbose"]));
        assert_eq!(
            tokens,
            vec![Token::LongFlag {
                name: s("verbose"),
                value: None,
            }]
        );
    }

    #[test]
    fn test_long_flag_with_value() {
        let tokens = tokenize(&args(&["--output=json"]));
        assert_eq!(
            tokens,
            vec![Token::LongFlag {
                name: s("output"),
                value: Some(s("json")),
            }]
        );
    }

    #[test]
    fn test_long_flag_empty_value() {
        let tokens = tokenize(&args(&["--output="]));
        assert_eq!(
            tokens,
            vec![Token::LongFlag {
                name: s("output"),
                value: Some(s("")),
            }]
        );
    }

    #[test]
    fn test_long_flag_value_with_equals() {
        // --config=key=value should split only at the first `=`
        let tokens = tokenize(&args(&["--config=key=value"]));
        assert_eq!(
            tokens,
            vec![Token::LongFlag {
                name: s("config"),
                value: Some(s("key=value")),
            }]
        );
    }

    #[test]
    fn test_long_flag_empty_name_with_value() {
        // --=value is weird but handled gracefully
        let tokens = tokenize(&args(&["--=value"]));
        assert_eq!(
            tokens,
            vec![Token::LongFlag {
                name: s(""),
                value: Some(s("value")),
            }]
        );
    }

    #[test]
    fn test_short_flag() {
        let tokens = tokenize(&args(&["-v"]));
        assert_eq!(
            tokens,
            vec![Token::ShortFlag {
                name: 'v',
                value: None,
            }]
        );
    }

    #[test]
    fn test_short_flag_with_value() {
        let tokens = tokenize(&args(&["-o=json"]));
        assert_eq!(
            tokens,
            vec![Token::ShortFlag {
                name: 'o',
                value: Some(s("json")),
            }]
        );
    }

    #[test]
    fn test_short_flag_empty_value() {
        // `-o=` (length 3, third char is `=`) -> empty value
        let tokens = tokenize(&args(&["-o="]));
        assert_eq!(
            tokens,
            vec![Token::ShortFlag {
                name: 'o',
                value: Some(s("")),
            }]
        );
    }

    #[test]
    fn test_short_cluster() {
        let tokens = tokenize(&args(&["-vvv"]));
        assert_eq!(tokens, vec![Token::ShortCluster(vec!['v', 'v', 'v'])]);
    }

    #[test]
    fn test_short_cluster_mixed() {
        let tokens = tokenize(&args(&["-abc"]));
        assert_eq!(tokens, vec![Token::ShortCluster(vec!['a', 'b', 'c'])]);
    }

    #[test]
    fn test_end_of_options() {
        let tokens = tokenize(&args(&["--"]));
        assert_eq!(tokens, vec![Token::EndOfOptions]);
    }

    #[test]
    fn test_after_end_of_options() {
        let tokens = tokenize(&args(&["--", "--verbose"]));
        assert_eq!(
            tokens,
            vec![Token::EndOfOptions, Token::Positional(s("--verbose")),]
        );
    }

    #[test]
    fn test_after_end_of_options_multiple() {
        let tokens = tokenize(&args(&["--", "-v", "--flag=val", "pos"]));
        assert_eq!(
            tokens,
            vec![
                Token::EndOfOptions,
                Token::Positional(s("-v")),
                Token::Positional(s("--flag=val")),
                Token::Positional(s("pos")),
            ]
        );
    }

    #[test]
    fn test_positional() {
        let tokens = tokenize(&args(&["example.com"]));
        assert_eq!(tokens, vec![Token::Positional(s("example.com"))]);
    }

    #[test]
    fn test_negative_number() {
        let tokens = tokenize(&args(&["-42"]));
        assert_eq!(tokens, vec![Token::Positional(s("-42"))]);
    }

    #[test]
    fn test_negative_number_float() {
        let tokens = tokenize(&args(&["-3.14"]));
        assert_eq!(tokens, vec![Token::Positional(s("-3.14"))]);
    }

    #[test]
    fn test_single_dash() {
        let tokens = tokenize(&args(&["-"]));
        assert_eq!(tokens, vec![Token::Positional(s("-"))]);
    }

    #[test]
    fn test_empty_string() {
        let tokens = tokenize(&args(&[""]));
        assert_eq!(tokens, vec![Token::Positional(s(""))]);
    }

    #[test]
    fn test_empty_args() {
        let tokens = tokenize(&args(&[]));
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_mixed() {
        let tokens = tokenize(&args(&[
            "server",
            "--path",
            "/data",
            "--bind",
            "0.0.0.0:6380",
            "-v",
        ]));
        assert_eq!(
            tokens,
            vec![
                Token::Positional(s("server")),
                Token::LongFlag {
                    name: s("path"),
                    value: None,
                },
                Token::Positional(s("/data")),
                Token::LongFlag {
                    name: s("bind"),
                    value: None,
                },
                Token::Positional(s("0.0.0.0:6380")),
                Token::ShortFlag {
                    name: 'v',
                    value: None,
                },
            ]
        );
    }

    #[test]
    fn test_mixed_with_eoo() {
        let tokens = tokenize(&args(&[
            "--output=json",
            "-v",
            "--",
            "--not-a-flag",
            "target",
        ]));
        assert_eq!(
            tokens,
            vec![
                Token::LongFlag {
                    name: s("output"),
                    value: Some(s("json")),
                },
                Token::ShortFlag {
                    name: 'v',
                    value: None,
                },
                Token::EndOfOptions,
                Token::Positional(s("--not-a-flag")),
                Token::Positional(s("target")),
            ]
        );
    }

    #[test]
    fn test_realistic_command() {
        // red serve --path /data --bind 0.0.0.0:6380 --role primary -v
        let tokens = tokenize(&args(&[
            "server",
            "--path",
            "/data",
            "--bind=0.0.0.0:6380",
            "--role",
            "primary",
            "-v",
        ]));
        assert_eq!(
            tokens,
            vec![
                Token::Positional(s("server")),
                Token::LongFlag {
                    name: s("path"),
                    value: None,
                },
                Token::Positional(s("/data")),
                Token::LongFlag {
                    name: s("bind"),
                    value: Some(s("0.0.0.0:6380")),
                },
                Token::LongFlag {
                    name: s("role"),
                    value: None,
                },
                Token::Positional(s("primary")),
                Token::ShortFlag {
                    name: 'v',
                    value: None,
                },
            ]
        );
    }
}
