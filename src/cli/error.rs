/// CLI parse error with context for helpful messages.
///
/// Self-contained ANSI color constants -- no external terminal dependency.
///
/// Minimal ANSI escape codes for error formatting.
mod ansi {
    pub const BOLD: &str = "\x1b[1m";
    pub const RESET: &str = "\x1b[0m";
    pub const CYAN: &str = "\x1b[36m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const GREEN: &str = "\x1b[32m";
}

#[derive(Debug, Clone)]
pub enum ParseError {
    /// Unknown flag not in schema
    UnknownFlag {
        flag: String,
        suggestions: Vec<String>,
    },
    /// Flag expects a value but none provided
    MissingFlagValue { flag: String, expected_type: String },
    /// Flag value doesn't match expected type
    InvalidValue {
        flag: String,
        value: String,
        expected_type: String,
        reason: String,
    },
    /// Flag value not in allowed choices
    InvalidChoice {
        flag: String,
        value: String,
        allowed: Vec<String>,
    },
    /// Required flag not provided
    MissingRequired { flag: String },
    /// Unknown command (domain/resource/verb not found)
    UnknownCommand {
        tokens: Vec<String>,
        suggestions: Vec<String>,
    },
    /// Domain found but missing resource
    MissingResource {
        domain: String,
        available: Vec<String>,
    },
    /// Domain+resource found but missing verb
    MissingVerb {
        domain: String,
        resource: String,
        available: Vec<String>,
    },
    /// Help was requested (not really an error)
    HelpRequested { text: String },
    /// Version was requested (not really an error)
    VersionRequested { text: String },
    /// Generic error with message
    Other(String),
}

impl ParseError {
    /// Format as human-readable error message with colors
    pub fn format_human(&self) -> String {
        match self {
            ParseError::UnknownFlag { flag, suggestions } => {
                let mut out = format!(
                    "{}error:{} unknown flag '{}{}{}'\n",
                    ansi::BOLD,
                    ansi::RESET,
                    ansi::CYAN,
                    flag,
                    ansi::RESET,
                );
                if !suggestions.is_empty() {
                    out.push_str(&format!(
                        "\n  {}Did you mean:{}\n",
                        ansi::YELLOW,
                        ansi::RESET
                    ));
                    for s in suggestions {
                        out.push_str(&format!("    {}{}{}\n", ansi::GREEN, s, ansi::RESET));
                    }
                }
                out
            }
            ParseError::MissingFlagValue {
                flag,
                expected_type,
            } => {
                format!(
                    "{}error:{} flag '{}{}{}' requires a value of type {}{}{}\n",
                    ansi::BOLD,
                    ansi::RESET,
                    ansi::CYAN,
                    flag,
                    ansi::RESET,
                    ansi::YELLOW,
                    expected_type,
                    ansi::RESET,
                )
            }
            ParseError::InvalidValue {
                flag,
                value,
                expected_type,
                reason,
            } => {
                format!(
                    "{}error:{} invalid value '{}{}{}' for {}{}{}\n\n  Expected {}{}{}: {}\n",
                    ansi::BOLD,
                    ansi::RESET,
                    ansi::CYAN,
                    value,
                    ansi::RESET,
                    ansi::CYAN,
                    flag,
                    ansi::RESET,
                    ansi::YELLOW,
                    expected_type,
                    ansi::RESET,
                    reason,
                )
            }
            ParseError::InvalidChoice {
                flag,
                value,
                allowed,
            } => {
                let mut out = format!(
                    "{}error:{} invalid value '{}{}{}' for {}{}{}\n",
                    ansi::BOLD,
                    ansi::RESET,
                    ansi::CYAN,
                    value,
                    ansi::RESET,
                    ansi::CYAN,
                    flag,
                    ansi::RESET,
                );
                out.push_str(&format!(
                    "\n  {}Allowed values:{} {}\n",
                    ansi::YELLOW,
                    ansi::RESET,
                    allowed.join(", "),
                ));
                out
            }
            ParseError::MissingRequired { flag } => {
                format!(
                    "{}error:{} missing required flag '{}{}{}'\n",
                    ansi::BOLD,
                    ansi::RESET,
                    ansi::CYAN,
                    flag,
                    ansi::RESET,
                )
            }
            ParseError::UnknownCommand {
                tokens,
                suggestions,
            } => {
                let cmd = tokens.join(" ");
                let mut out = format!(
                    "{}error:{} unknown command '{}{}{}'\n",
                    ansi::BOLD,
                    ansi::RESET,
                    ansi::CYAN,
                    cmd,
                    ansi::RESET,
                );
                if !suggestions.is_empty() {
                    out.push_str(&format!(
                        "\n  {}Did you mean:{}\n",
                        ansi::YELLOW,
                        ansi::RESET
                    ));
                    for s in suggestions {
                        out.push_str(&format!("    {}red {}{}\n", ansi::GREEN, s, ansi::RESET));
                    }
                }
                out
            }
            ParseError::MissingResource { domain, available } => {
                let mut out = format!(
                    "{}error:{} missing resource for '{}{}{}'\n",
                    ansi::BOLD,
                    ansi::RESET,
                    ansi::CYAN,
                    domain,
                    ansi::RESET,
                );
                if !available.is_empty() {
                    out.push_str(&format!(
                        "\n  {}Available resources:{}\n",
                        ansi::YELLOW,
                        ansi::RESET,
                    ));
                    for r in available {
                        out.push_str(&format!("    {}{}{}\n", ansi::GREEN, r, ansi::RESET));
                    }
                }
                out
            }
            ParseError::MissingVerb {
                domain,
                resource,
                available,
            } => {
                let mut out = format!(
                    "{}error:{} missing verb for '{}{} {}{}'\n",
                    ansi::BOLD,
                    ansi::RESET,
                    ansi::CYAN,
                    domain,
                    resource,
                    ansi::RESET,
                );
                if !available.is_empty() {
                    out.push_str(&format!(
                        "\n  {}Available verbs:{}\n",
                        ansi::YELLOW,
                        ansi::RESET,
                    ));
                    for v in available {
                        out.push_str(&format!("    {}{}{}\n", ansi::GREEN, v, ansi::RESET));
                    }
                }
                out
            }
            ParseError::HelpRequested { text } | ParseError::VersionRequested { text } => {
                text.clone()
            }
            ParseError::Other(msg) => {
                format!("{}error:{} {}\n", ansi::BOLD, ansi::RESET, msg)
            }
        }
    }

    /// Format as JSON error object
    pub fn format_json(&self) -> String {
        // Manual JSON construction -- no serde, no external crates
        let escape = |s: &str| -> String {
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\t', "\\t")
        };

        match self {
            ParseError::UnknownFlag { flag, suggestions } => {
                let suggestions_json: Vec<String> = suggestions
                    .iter()
                    .map(|s| format!("\"{}\"", escape(s)))
                    .collect();
                format!(
                    "{{\"error\":\"unknown_flag\",\"flag\":\"{}\",\"suggestions\":[{}]}}",
                    escape(flag),
                    suggestions_json.join(","),
                )
            }
            ParseError::MissingFlagValue {
                flag,
                expected_type,
            } => {
                format!(
                    "{{\"error\":\"missing_flag_value\",\"flag\":\"{}\",\"expected_type\":\"{}\"}}",
                    escape(flag),
                    escape(expected_type),
                )
            }
            ParseError::InvalidValue {
                flag,
                value,
                expected_type,
                reason,
            } => {
                format!(
                    "{{\"error\":\"invalid_value\",\"flag\":\"{}\",\"value\":\"{}\",\"expected_type\":\"{}\",\"reason\":\"{}\"}}",
                    escape(flag),
                    escape(value),
                    escape(expected_type),
                    escape(reason),
                )
            }
            ParseError::InvalidChoice {
                flag,
                value,
                allowed,
            } => {
                let allowed_json: Vec<String> = allowed
                    .iter()
                    .map(|s| format!("\"{}\"", escape(s)))
                    .collect();
                format!(
          "{{\"error\":\"invalid_choice\",\"flag\":\"{}\",\"value\":\"{}\",\"allowed\":[{}]}}",
          escape(flag),
          escape(value),
          allowed_json.join(","),
        )
            }
            ParseError::MissingRequired { flag } => {
                format!(
                    "{{\"error\":\"missing_required\",\"flag\":\"{}\"}}",
                    escape(flag),
                )
            }
            ParseError::UnknownCommand {
                tokens,
                suggestions,
            } => {
                let tokens_json: Vec<String> = tokens
                    .iter()
                    .map(|s| format!("\"{}\"", escape(s)))
                    .collect();
                let suggestions_json: Vec<String> = suggestions
                    .iter()
                    .map(|s| format!("\"{}\"", escape(s)))
                    .collect();
                format!(
                    "{{\"error\":\"unknown_command\",\"tokens\":[{}],\"suggestions\":[{}]}}",
                    tokens_json.join(","),
                    suggestions_json.join(","),
                )
            }
            ParseError::MissingResource { domain, available } => {
                let available_json: Vec<String> = available
                    .iter()
                    .map(|s| format!("\"{}\"", escape(s)))
                    .collect();
                format!(
                    "{{\"error\":\"missing_resource\",\"domain\":\"{}\",\"available\":[{}]}}",
                    escape(domain),
                    available_json.join(","),
                )
            }
            ParseError::MissingVerb {
                domain,
                resource,
                available,
            } => {
                let available_json: Vec<String> = available
                    .iter()
                    .map(|s| format!("\"{}\"", escape(s)))
                    .collect();
                format!(
          "{{\"error\":\"missing_verb\",\"domain\":\"{}\",\"resource\":\"{}\",\"available\":[{}]}}",
          escape(domain),
          escape(resource),
          available_json.join(","),
        )
            }
            ParseError::HelpRequested { text } => {
                format!("{{\"type\":\"help\",\"text\":\"{}\"}}", escape(text))
            }
            ParseError::VersionRequested { text } => {
                format!("{{\"type\":\"version\",\"text\":\"{}\"}}", escape(text))
            }
            ParseError::Other(msg) => {
                format!("{{\"error\":\"other\",\"message\":\"{}\"}}", escape(msg))
            }
        }
    }

    /// Is this a "not really an error" variant (help/version)?
    pub fn is_info(&self) -> bool {
        matches!(
            self,
            ParseError::HelpRequested { .. } | ParseError::VersionRequested { .. }
        )
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.format_human())
    }
}

impl std::error::Error for ParseError {}

/// Levenshtein distance for suggestion generation.
/// Standard dynamic programming O(n*m) with a flat Vec.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_len = a.len();
    let b_len = b.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let width = b_len + 1;
    let mut matrix = vec![0usize; (a_len + 1) * width];

    // Initialize first row and column
    for i in 0..=a_len {
        matrix[i * width] = i;
    }
    for (j, item) in matrix.iter_mut().enumerate().take(b_len + 1) {
        *item = j;
    }

    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = if a_bytes[i - 1] == b_bytes[j - 1] {
                0
            } else {
                1
            };

            let delete = matrix[(i - 1) * width + j] + 1;
            let insert = matrix[i * width + (j - 1)] + 1;
            let substitute = matrix[(i - 1) * width + (j - 1)] + cost;

            matrix[i * width + j] = delete.min(insert).min(substitute);
        }
    }

    matrix[a_len * width + b_len]
}

/// Generate suggestions for a mistyped string from candidates.
/// Returns up to `max_results` candidates sorted by ascending distance.
pub fn suggest(input: &str, candidates: &[&str], max_results: usize) -> Vec<String> {
    let threshold = 3.max(input.len() / 2);
    let mut scored: Vec<(usize, &str)> = candidates
        .iter()
        .map(|&c| (levenshtein(input, c), c))
        .filter(|(d, _)| *d <= threshold)
        .collect();

    scored.sort_by_key(|(d, _)| *d);
    scored
        .into_iter()
        .take(max_results)
        .map(|(_, c)| c.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn test_levenshtein_one_char() {
        assert_eq!(levenshtein("cat", "hat"), 1);
    }

    #[test]
    fn test_levenshtein_transposition() {
        // Standard Levenshtein treats transposition as 2 ops (delete+insert)
        assert_eq!(levenshtein("ab", "ba"), 2);
    }

    #[test]
    fn test_levenshtein_empty() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("xyz", ""), 3);
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn test_suggest_finds_close_match() {
        let candidates = &["json", "yaml", "text"];
        let results = suggest("jsno", candidates, 3);
        assert!(!results.is_empty());
        assert_eq!(results[0], "json");
    }

    #[test]
    fn test_suggest_no_match_too_far() {
        let candidates = &["json", "yaml", "text"];
        let results = suggest("zzzzzzzzz", candidates, 3);
        assert!(results.is_empty());
    }

    #[test]
    fn test_suggest_respects_max_results() {
        let candidates = &["scan", "span", "stan", "plan", "swan"];
        let results = suggest("sca", candidates, 2);
        assert!(results.len() <= 2);
    }

    #[test]
    fn test_format_unknown_flag() {
        let err = ParseError::UnknownFlag {
            flag: "--jsno".to_string(),
            suggestions: vec!["--json".to_string()],
        };
        let msg = err.format_human();
        assert!(msg.contains("unknown flag"));
        assert!(msg.contains("--jsno"));
        assert!(msg.contains("--json"));
    }

    #[test]
    fn test_format_unknown_command() {
        let err = ParseError::UnknownCommand {
            tokens: vec!["serv".to_string(), "start".to_string()],
            suggestions: vec!["server".to_string()],
        };
        let msg = err.format_human();
        assert!(msg.contains("unknown command"));
        assert!(msg.contains("serv start"));
        assert!(msg.contains("server"));
    }

    #[test]
    fn test_format_invalid_choice() {
        let err = ParseError::InvalidChoice {
            flag: "--output".to_string(),
            value: "xml".to_string(),
            allowed: vec!["text".to_string(), "json".to_string(), "yaml".to_string()],
        };
        let msg = err.format_human();
        assert!(msg.contains("invalid value"));
        assert!(msg.contains("xml"));
        assert!(msg.contains("--output"));
        assert!(msg.contains("text"));
        assert!(msg.contains("json"));
        assert!(msg.contains("yaml"));
    }

    #[test]
    fn test_is_info_for_help() {
        let err = ParseError::HelpRequested {
            text: "usage: red ...".to_string(),
        };
        assert!(err.is_info());
    }

    #[test]
    fn test_is_info_for_version() {
        let err = ParseError::VersionRequested {
            text: "red 0.1.0".to_string(),
        };
        assert!(err.is_info());
    }

    #[test]
    fn test_is_info_false_for_real_errors() {
        let err = ParseError::Other("something went wrong".to_string());
        assert!(!err.is_info());

        let err = ParseError::MissingRequired {
            flag: "--target".to_string(),
        };
        assert!(!err.is_info());
    }

    #[test]
    fn test_display_delegates_to_format_human() {
        let err = ParseError::Other("boom".to_string());
        let display = format!("{}", err);
        assert_eq!(display, err.format_human());
    }

    #[test]
    fn test_format_json_unknown_flag() {
        let err = ParseError::UnknownFlag {
            flag: "--jsno".to_string(),
            suggestions: vec!["--json".to_string()],
        };
        let json = err.format_json();
        assert!(json.contains("\"error\":\"unknown_flag\""));
        assert!(json.contains("\"flag\":\"--jsno\""));
        assert!(json.contains("\"--json\""));
    }

    #[test]
    fn test_format_json_escapes_special_chars() {
        let err = ParseError::Other("line1\nline2\t\"quoted\"".to_string());
        let json = err.format_json();
        assert!(json.contains("\\n"));
        assert!(json.contains("\\t"));
        assert!(json.contains("\\\"quoted\\\""));
    }
}
