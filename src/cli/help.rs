/// Help text generation for the RedDB CLI.
///
/// Consolidates all help formatting into a single module with functions
/// that accept structured data and produce consistent, well-aligned help
/// output strings.
use super::commands::{Flag, Route};

/// Minimum column width for the flag/verb label column.
const LABEL_WIDTH: usize = 20;

/// Global help: list all domains with descriptions.
///
/// `domains` is a slice of `(name, description, aliases)`.
pub fn format_global_help(domains: &[(String, String, Vec<String>)]) -> String {
    let mut out = String::with_capacity(1024);

    out.push_str("reddb -- unified multi-model database engine\n");
    out.push('\n');
    out.push_str("Usage: red <command> [args] [flags]\n");
    out.push('\n');

    out.push_str("Commands:\n");
    for (name, description, aliases) in domains {
        let alias_text = if aliases.is_empty() {
            String::new()
        } else {
            format!(" [{}]", aliases.join(", "))
        };
        out.push_str(&format!("  {:<14} {}{}\n", name, description, alias_text));
    }
    out.push('\n');

    out.push_str("Global flags:\n");
    out.push_str(&format!("  {:<20} {}\n", "-h, --help", "Show help"));
    out.push_str(&format!("  {:<20} {}\n", "-j, --json", "Force JSON output"));
    out.push_str(&format!(
        "  {:<20} {}\n",
        "-o, --output FORMAT", "Output format [text|json|yaml]"
    ));
    out.push_str(&format!("  {:<20} {}\n", "-v, --verbose", "Verbose output"));
    out.push_str(&format!(
        "  {:<20} {}\n",
        "    --no-color", "Disable colors"
    ));
    out.push_str(&format!("  {:<20} {}\n", "    --version", "Show version"));
    out.push('\n');

    out.push_str("Run 'red <command> help' for more information\n");
    out
}

/// Domain help: list resources and their verbs for a domain.
///
/// `resources` is a slice of `(name, description, routes)`.
pub fn format_domain_help(domain: &str, resources: &[(String, String, Vec<Route>)]) -> String {
    let mut out = String::with_capacity(512);

    out.push_str(&format!("red {} -- {}\n", domain, domain_label(domain)));
    out.push('\n');

    out.push_str("Resources:\n");
    for (name, description, routes) in resources {
        out.push_str(&format!("  {:<14} {}\n", name, description));
        for route in routes {
            out.push_str(&format!("    {:<12} {}\n", route.verb, route.summary));
        }
    }
    out.push('\n');

    out.push_str(&format!(
        "Run 'red {} <resource> help' for more information\n",
        domain
    ));
    out
}

/// Resource/command help: list verbs with flags for a specific resource.
pub fn format_command_help(
    domain: &str,
    resource: &str,
    routes: &[Route],
    flags: &[Flag],
) -> String {
    let mut out = String::with_capacity(512);

    out.push_str(&format!("red {} {} -- {}\n", domain, resource, resource));
    out.push('\n');

    if !routes.is_empty() {
        out.push_str("Verbs:\n");
        for route in routes {
            out.push_str(&format!("  {:<14} {}\n", route.verb, route.summary));
        }
        out.push('\n');
    }

    let all_flags = merge_with_global_flags(flags);
    if !all_flags.is_empty() {
        out.push_str("Flags:\n");
        for flag in &all_flags {
            out.push_str(&format_flag(flag));
            out.push('\n');
        }
        out.push('\n');
    }

    if !routes.is_empty() {
        out.push_str("Examples:\n");
        for route in routes {
            if !route.usage.is_empty() {
                out.push_str(&format!("  {}\n", route.usage));
            }
        }
        if routes.iter().any(|r| !r.usage.is_empty()) {
            out.push('\n');
        }
    }

    out
}

/// Route help: detailed help for a single verb.
pub fn format_route_help(domain: &str, resource: &str, route: &Route, flags: &[Flag]) -> String {
    let mut out = String::with_capacity(512);

    out.push_str(&format!(
        "red {} {} {} -- {}\n",
        domain, resource, route.verb, route.summary
    ));
    out.push('\n');

    // Usage line
    out.push_str(&format!(
        "Usage: red {} {} {} <target>",
        domain, resource, route.verb
    ));
    for flag in flags {
        if let Some(ref arg_name) = flag.arg {
            let token = if let Some(ch) = flag.short {
                format!("-{}", ch)
            } else {
                format!("--{}", flag.long)
            };
            out.push_str(&format!(" [{}  {}]", token, arg_name.to_uppercase()));
        }
    }
    out.push('\n');
    out.push('\n');

    // Command-specific flags
    if !flags.is_empty() {
        out.push_str("Flags:\n");
        for flag in flags {
            out.push_str(&format_flag(flag));
            out.push('\n');
        }
        out.push('\n');
    }

    // Global flags section
    out.push_str("Global flags:\n");
    for flag in &global_flag_list() {
        out.push_str(&format_flag(flag));
        out.push('\n');
    }
    out.push('\n');

    out
}

/// Format a single flag line for help display.
fn format_flag(flag: &Flag) -> String {
    let short_part = match flag.short {
        Some(ch) => format!("-{}, ", ch),
        None => "    ".to_string(),
    };

    let arg_part = match flag.arg {
        Some(ref name) => format!(" {}", name.to_uppercase()),
        None => String::new(),
    };

    let label = format!("{}--{}{}", short_part, flag.long, arg_part);

    // Pad label to LABEL_WIDTH; if it exceeds, use the actual length + 2 spaces
    let padding = if label.len() < LABEL_WIDTH {
        LABEL_WIDTH - label.len()
    } else {
        2
    };

    let default_text = match flag.default {
        Some(ref d) => format!(" (default: {})", d),
        None => String::new(),
    };

    format!(
        "  {}{}{}{}",
        label,
        " ".repeat(padding),
        flag.description,
        default_text,
    )
}

/// Produce a minimal domain label from the domain name.
fn domain_label(domain: &str) -> &str {
    match domain {
        "server" => "Start the database server/router",
        "query" => "Execute queries",
        "insert" => "Insert entities",
        "get" => "Retrieve entities",
        "delete" => "Delete entities",
        "health" => "Health check",
        "status" => "Replication status",
        "version" => "Version information",
        "data" => "Data operations",
        "index" => "Index management",
        "graph" => "Graph operations",
        "replica" => "Start as read replica",
        _ => domain,
    }
}

/// Merge command-specific flags with global flags, avoiding duplicates.
fn merge_with_global_flags(command_flags: &[Flag]) -> Vec<Flag> {
    let mut result: Vec<Flag> = command_flags.to_vec();
    let existing_longs: Vec<&str> = command_flags.iter().map(|f| f.long.as_str()).collect();

    for gf in global_flag_list() {
        if !existing_longs.contains(&gf.long.as_str()) {
            result.push(gf);
        }
    }

    result
}

/// Canonical global flags expressed as Flag instances.
fn global_flag_list() -> Vec<Flag> {
    vec![
        Flag::new("help", "Show help").with_short('h'),
        Flag::new("json", "Force JSON output").with_short('j'),
        Flag::new("output", "Output format")
            .with_short('o')
            .with_arg("FORMAT"),
        Flag::new("verbose", "Verbose output").with_short('v'),
        Flag::new("no-color", "Disable colors"),
        Flag::new("version", "Show version"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_global_help() {
        let domains = vec![
            (
                "server".to_string(),
                "Start database server".to_string(),
                vec![],
            ),
            (
                "query".to_string(),
                "Execute queries".to_string(),
                vec!["q".to_string()],
            ),
        ];
        let help = format_global_help(&domains);

        assert!(help.contains("reddb"));
        assert!(help.contains("Usage: red <command>"));
        assert!(help.contains("Commands:"));
        assert!(help.contains("server"));
        assert!(help.contains("Start database server"));
        assert!(help.contains("[q]"));
        assert!(help.contains("query"));
        assert!(help.contains("Global flags:"));
        assert!(help.contains("-h, --help"));
        assert!(help.contains("-j, --json"));
        assert!(help.contains("Run 'red <command> help'"));
    }

    #[test]
    fn test_format_domain_help() {
        let resources = vec![(
            "collection".to_string(),
            "Collection operations".to_string(),
            vec![Route {
                verb: "list",
                summary: "List all collections",
                usage: "red data collection list",
            }],
        )];
        let help = format_domain_help("data", &resources);

        assert!(help.contains("red data"));
        assert!(help.contains("Resources:"));
        assert!(help.contains("collection"));
        assert!(help.contains("Collection operations"));
        assert!(help.contains("list"));
        assert!(help.contains("List all collections"));
        assert!(help.contains("Run 'red data <resource> help'"));
    }

    #[test]
    fn test_format_command_help() {
        let routes = vec![Route {
            verb: "list",
            summary: "List all collections",
            usage: "red data collection list --output json",
        }];
        let flags = vec![Flag::new("format", "Output format")
            .with_short('f')
            .with_arg("FORMAT")];
        let help = format_command_help("data", "collection", &routes, &flags);

        assert!(help.contains("red data collection"));
        assert!(help.contains("Verbs:"));
        assert!(help.contains("list"));
        assert!(help.contains("Flags:"));
        assert!(help.contains("-f, --format FORMAT"));
        assert!(help.contains("Output format"));
        assert!(help.contains("Examples:"));
        assert!(help.contains("red data collection list --output json"));
    }

    #[test]
    fn test_format_route_help() {
        let route = Route {
            verb: "list",
            summary: "List all collections",
            usage: "red data collection list",
        };
        let flags = vec![Flag::new("format", "Output format")
            .with_short('f')
            .with_arg("FORMAT")];
        let help = format_route_help("data", "collection", &route, &flags);

        assert!(help.contains("red data collection list -- List all collections"));
        assert!(help.contains("Usage: red data collection list <target>"));
        assert!(help.contains("Flags:"));
        assert!(help.contains("-f, --format FORMAT"));
        assert!(help.contains("Global flags:"));
        assert!(help.contains("-h, --help"));
    }

    #[test]
    fn test_format_flag_with_short_and_arg() {
        let flag = Flag::new("type", "Record type")
            .with_short('t')
            .with_arg("TYPE");
        let formatted = format_flag(&flag);

        assert!(formatted.contains("-t, --type TYPE"));
        assert!(formatted.contains("Record type"));
    }

    #[test]
    fn test_format_flag_boolean() {
        let flag = Flag::new("verbose", "Enable verbose output");
        let formatted = format_flag(&flag);

        assert!(formatted.starts_with("      --verbose"));
        assert!(formatted.contains("Enable verbose output"));
        // No arg name after the flag
        assert!(!formatted.contains("VERBOSE"));
    }

    #[test]
    fn test_format_flag_with_default() {
        let flag = Flag::new("format", "Output format")
            .with_short('f')
            .with_arg("FORMAT")
            .with_default("text");
        let formatted = format_flag(&flag);

        assert!(formatted.contains("-f, --format FORMAT"));
        assert!(formatted.contains("Output format"));
        assert!(formatted.contains("(default: text)"));
    }

    #[test]
    fn test_format_flag_long_only_no_arg() {
        let flag = Flag::new("no-color", "Disable colors");
        let formatted = format_flag(&flag);

        // Long-only: 4-space indent where short flag would be
        assert!(formatted.contains("    --no-color"));
        assert!(formatted.contains("Disable colors"));
    }

    #[test]
    fn test_format_global_help_empty_domains() {
        let domains: Vec<(String, String, Vec<String>)> = vec![];
        let help = format_global_help(&domains);
        assert!(help.contains("Commands:"));
        assert!(help.contains("Global flags:"));
    }

    #[test]
    fn test_format_command_help_no_flags() {
        let routes = vec![Route {
            verb: "start",
            summary: "Start the server",
            usage: "red serve start",
        }];
        let help = format_command_help("server", "grpc", &routes, &[]);

        assert!(help.contains("Verbs:"));
        assert!(help.contains("start"));
        // Global flags are always merged in
        assert!(help.contains("Flags:"));
        assert!(help.contains("--help"));
    }
}
