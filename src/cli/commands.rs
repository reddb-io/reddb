/// RedDB command definitions.
///
/// Defines the command tree, Flag and Route types used by help and completion
/// generators, and the schema for each built-in command.
use super::types::FlagSchema;

// ============================================================================
// Help-layer types (used by help.rs and complete.rs)
// ============================================================================

/// Lightweight flag descriptor used by the help generator.
#[derive(Debug, Clone)]
pub struct Flag {
    pub short: Option<char>,
    pub long: String,
    pub description: String,
    pub default: Option<String>,
    pub arg: Option<String>,
}

impl Flag {
    pub fn new(long: &str, desc: &str) -> Self {
        Self {
            short: None,
            long: long.to_string(),
            description: desc.to_string(),
            default: None,
            arg: None,
        }
    }

    pub fn with_short(mut self, short: char) -> Self {
        self.short = Some(short);
        self
    }

    pub fn with_default(mut self, default: &str) -> Self {
        self.default = Some(default.to_string());
        self
    }

    pub fn with_arg(mut self, arg: &str) -> Self {
        self.arg = Some(arg.to_string());
        self
    }
}

/// A single routable verb within a resource.
#[derive(Debug, Clone)]
pub struct Route {
    pub verb: &'static str,
    pub summary: &'static str,
    pub usage: &'static str,
}

// ============================================================================
// RedDB command definitions
// ============================================================================

/// Command descriptor for a top-level RedDB command.
pub struct CommandDef {
    pub name: &'static str,
    pub summary: &'static str,
    pub usage: &'static str,
    pub flags: Vec<FlagSchema>,
}

/// Return all RedDB commands.
pub fn all_commands() -> Vec<CommandDef> {
    vec![
    CommandDef {
      name: "server",
      summary: "Start the database server",
      usage: "red server [--grpc|--http] [--path ./data/reddb.rdb] [--bind 127.0.0.1:50051]",
      flags: server_flags(),
    },
    CommandDef {
      name: "query",
      summary: "Execute a query against the database",
      usage: "red query \"SELECT * FROM users WHERE age > 21\"",
      flags: query_flags(),
    },
    CommandDef {
      name: "insert",
      summary: "Insert an entity into a collection",
      usage: "red insert users '{\"name\": \"Alice\", \"age\": 30}'",
      flags: insert_flags(),
    },
    CommandDef {
      name: "get",
      summary: "Get an entity by ID from a collection",
      usage: "red get users abc123",
      flags: get_flags(),
    },
    CommandDef {
      name: "delete",
      summary: "Delete an entity by ID from a collection",
      usage: "red delete users abc123",
      flags: delete_flags(),
    },
    CommandDef {
      name: "health",
      summary: "Run a health check against the server",
      usage: "red health [--bind 0.0.0.0:6380]",
      flags: health_flags(),
    },
    CommandDef {
      name: "replica",
      summary: "Start as a read replica connected to a primary",
      usage: "red replica --primary-addr http://primary:50051 [--grpc|--http] [--path ./data/reddb.rdb]",
      flags: replica_flags(),
    },
    CommandDef {
      name: "status",
      summary: "Show replication status",
      usage: "red status [--bind 0.0.0.0:6380]",
      flags: status_flags(),
    },
    CommandDef {
      name: "mcp",
      summary: "Start MCP server for AI agent integration",
      usage: "red mcp [--path /data]",
      flags: mcp_flags(),
    },
    CommandDef {
      name: "auth",
      summary: "Manage authentication (users, tokens, roles)",
      usage: "red auth <subcommand>",
      flags: auth_flags(),
    },
    CommandDef {
      name: "connect",
      summary: "Connect to a remote RedDB server (interactive REPL)",
      usage: "red connect [--token <token>] [--query <sql>] <addr>",
      flags: connect_flags(),
    },
    CommandDef {
      name: "version",
      summary: "Show RedDB version information",
      usage: "red version",
      flags: vec![],
    },
  ]
}

/// Return the help text for the main `red` command.
pub fn main_help_text() -> String {
    let mut out = String::with_capacity(1024);

    out.push_str("reddb -- unified multi-model database engine\n");
    out.push('\n');
    out.push_str("Usage: red <command> [args] [flags]\n");
    out.push('\n');

    out.push_str("Commands:\n");
    for cmd in all_commands() {
        out.push_str(&format!("  {:<14} {}\n", cmd.name, cmd.summary));
    }
    out.push_str(&format!("  {:<14} {}\n", "help", "Show help for a command"));
    out.push('\n');

    out.push_str("Global flags:\n");
    out.push_str(&format!("  {:<24} {}\n", "-h, --help", "Show help"));
    out.push_str(&format!("  {:<24} {}\n", "-j, --json", "Force JSON output"));
    out.push_str(&format!(
        "  {:<24} {}\n",
        "-o, --output FORMAT", "Output format [text|json|yaml]"
    ));
    out.push_str(&format!("  {:<24} {}\n", "-v, --verbose", "Verbose output"));
    out.push_str(&format!(
        "  {:<24} {}\n",
        "    --no-color", "Disable colors"
    ));
    out.push_str(&format!("  {:<24} {}\n", "    --version", "Show version"));
    out.push('\n');

    out.push_str("Examples:\n");
    out.push_str("  red server --grpc --path ./data/reddb.rdb --bind 127.0.0.1:50051\n");
    out.push_str("  red server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080\n");
    out.push_str("  red replica --primary-addr http://primary:50051 --path ./data/replica.rdb\n");
    out.push_str("  red query \"SELECT * FROM users\"\n");
    out.push_str("  red insert users '{\"name\": \"Alice\"}'\n");
    out.push_str("  red get users abc123\n");
    out.push_str("  red health\n");
    out.push_str("  red auth create-user alice --password secret --role admin\n");
    out.push_str("  red auth create-api-key alice --name \"ci-token\" --role write\n");
    out.push_str("  red auth list-users\n");
    out.push_str("  red auth login alice --password secret\n");
    out.push_str("  red connect localhost:6380\n");
    out.push_str("  red connect --query \"SELECT * FROM users\" localhost:6380\n");
    out.push('\n');

    out.push_str("Run 'red <command> --help' for more information on a command.\n");
    out
}

/// Return help text for a specific command.
pub fn command_help_text(name: &str) -> Option<String> {
    let cmds = all_commands();
    let cmd = cmds.iter().find(|c| c.name == name)?;

    let mut out = String::with_capacity(512);

    out.push_str(&format!("red {} -- {}\n", cmd.name, cmd.summary));
    out.push('\n');
    out.push_str(&format!("Usage: {}\n", cmd.usage));
    out.push('\n');

    if !cmd.flags.is_empty() {
        out.push_str("Flags:\n");
        for flag in &cmd.flags {
            let short_part = match flag.short {
                Some(ch) => format!("-{}, ", ch),
                None => "    ".to_string(),
            };
            let value_part = if flag.expects_value {
                format!(" <{}>", flag.long.to_uppercase())
            } else {
                String::new()
            };
            let label = format!("{}--{}{}", short_part, flag.long, value_part);
            let padding = if label.len() < 24 {
                24 - label.len()
            } else {
                2
            };
            let default_text = match &flag.default {
                Some(d) => format!(" (default: {})", d),
                None => String::new(),
            };
            out.push_str(&format!(
                "  {}{}{}{}\n",
                label,
                " ".repeat(padding),
                flag.description,
                default_text,
            ));
        }
        out.push('\n');
    }

    Some(out)
}

// ============================================================================
// Per-command flag schemas
// ============================================================================

fn server_flags() -> Vec<FlagSchema> {
    vec![
    FlagSchema::new("path")
      .with_short('d')
      .with_description("Persistent database file path (omit for in-memory)")
      .with_default("./data/reddb.rdb"),
    FlagSchema::new("bind")
      .with_short('b')
      .with_description("Bind address (host:port); defaults to 127.0.0.1:50051 for gRPC or 127.0.0.1:8080 for HTTP"),
    FlagSchema::boolean("grpc")
      .with_description("Serve the gRPC API (default transport)"),
    FlagSchema::boolean("http")
      .with_description("Serve the HTTP API"),
    FlagSchema::new("role")
      .with_short('r')
      .with_description("Replication role")
      .with_choices(&["standalone", "primary", "replica"])
      .with_default("standalone"),
    FlagSchema::new("primary-addr")
      .with_description("Primary gRPC address for replica mode"),
    FlagSchema::boolean("read-only")
      .with_description("Open the database in read-only mode"),
    FlagSchema::boolean("no-create-if-missing")
      .with_description("Fail instead of creating the database file"),
    FlagSchema::new("vault")
      .with_description("Enable encrypted auth vault (reserved pages in main .rdb file)")
      .with_default("false"),
  ]
}

fn replica_flags() -> Vec<FlagSchema> {
    vec![
        FlagSchema::new("primary-addr")
            .with_short('p')
            .with_description("Primary gRPC address (e.g. http://primary:50051)"),
        FlagSchema::new("path")
            .with_short('d')
            .with_description("Local replica database file path")
            .with_default("./data/reddb.rdb"),
        FlagSchema::new("bind")
            .with_short('b')
            .with_description("Bind address (host:port); defaults by transport"),
        FlagSchema::boolean("grpc").with_description("Serve the gRPC API (default transport)"),
        FlagSchema::boolean("http").with_description("Serve the HTTP API"),
        FlagSchema::new("vault")
            .with_description("Enable encrypted auth vault (reserved pages in main .rdb file)")
            .with_default("false"),
    ]
}

fn query_flags() -> Vec<FlagSchema> {
    vec![FlagSchema::new("bind")
        .with_short('b')
        .with_description("Server address")
        .with_default("0.0.0.0:6380")]
}

fn insert_flags() -> Vec<FlagSchema> {
    vec![FlagSchema::new("bind")
        .with_short('b')
        .with_description("Server address")
        .with_default("0.0.0.0:6380")]
}

fn get_flags() -> Vec<FlagSchema> {
    vec![FlagSchema::new("bind")
        .with_short('b')
        .with_description("Server address")
        .with_default("0.0.0.0:6380")]
}

fn delete_flags() -> Vec<FlagSchema> {
    vec![FlagSchema::new("bind")
        .with_short('b')
        .with_description("Server address")
        .with_default("0.0.0.0:6380")]
}

fn health_flags() -> Vec<FlagSchema> {
    vec![
        FlagSchema::new("bind")
            .with_short('b')
            .with_description("Server address; defaults by transport"),
        FlagSchema::boolean("grpc").with_description("Probe a gRPC listener (default transport)"),
        FlagSchema::boolean("http").with_description("Probe an HTTP listener"),
    ]
}

fn status_flags() -> Vec<FlagSchema> {
    vec![FlagSchema::new("bind")
        .with_short('b')
        .with_description("Server address")
        .with_default("0.0.0.0:6380")]
}

fn mcp_flags() -> Vec<FlagSchema> {
    vec![FlagSchema::new("path")
        .with_short('d')
        .with_description("Data directory path (omit for in-memory)")
        .with_default("")]
}

fn connect_flags() -> Vec<FlagSchema> {
    vec![
        FlagSchema::new("token")
            .with_short('t')
            .with_description("Auth token (session or API key)"),
        FlagSchema::new("query")
            .with_short('q')
            .with_description("Execute a single query and exit"),
        FlagSchema::new("user")
            .with_short('u')
            .with_description("Username for login"),
        FlagSchema::new("password")
            .with_short('p')
            .with_description("Password for login"),
    ]
}

fn auth_flags() -> Vec<FlagSchema> {
    vec![
        FlagSchema::new("bind")
            .with_short('b')
            .with_description("Server address")
            .with_default("0.0.0.0:6380"),
        FlagSchema::new("password")
            .with_short('p')
            .with_description("User password"),
        FlagSchema::new("role")
            .with_short('r')
            .with_description("User role")
            .with_choices(&["read", "write", "admin"]),
        FlagSchema::new("name")
            .with_short('n')
            .with_description("API key name"),
        FlagSchema::new("user")
            .with_short('u')
            .with_description("Target username"),
    ]
}

// ============================================================================
// Completion data helpers
// ============================================================================

/// Return domain data for completion scripts.
pub fn completion_domains() -> Vec<(String, Vec<String>)> {
    vec![
        ("server".to_string(), vec![]),
        ("replica".to_string(), vec![]),
        ("query".to_string(), vec!["q".to_string()]),
        ("insert".to_string(), vec!["i".to_string()]),
        ("get".to_string(), vec![]),
        ("delete".to_string(), vec!["del".to_string()]),
        ("health".to_string(), vec![]),
        ("status".to_string(), vec![]),
        ("mcp".to_string(), vec![]),
        ("auth".to_string(), vec![]),
        ("connect".to_string(), vec![]),
        ("version".to_string(), vec![]),
    ]
}

/// Return global flag data for completion scripts.
pub fn completion_global_flags() -> Vec<(&'static str, Option<char>)> {
    vec![
        ("help", Some('h')),
        ("json", Some('j')),
        ("output", Some('o')),
        ("verbose", Some('v')),
        ("no-color", None),
        ("version", None),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_commands_defined() {
        let cmds = all_commands();
        let names: Vec<&str> = cmds.iter().map(|c| c.name).collect();
        assert!(names.contains(&"server"));
        assert!(names.contains(&"query"));
        assert!(names.contains(&"insert"));
        assert!(names.contains(&"get"));
        assert!(names.contains(&"delete"));
        assert!(names.contains(&"health"));
        assert!(names.contains(&"status"));
        assert!(names.contains(&"connect"));
        assert!(names.contains(&"version"));
    }

    #[test]
    fn test_server_has_flags() {
        let cmds = all_commands();
        let server = cmds.iter().find(|c| c.name == "server").unwrap();
        let flag_names: Vec<&str> = server.flags.iter().map(|f| f.long.as_str()).collect();
        assert!(flag_names.contains(&"path"));
        assert!(flag_names.contains(&"bind"));
    }

    #[test]
    fn test_replica_has_flags() {
        let cmds = all_commands();
        let replica = cmds.iter().find(|c| c.name == "replica").unwrap();
        let flag_names: Vec<&str> = replica.flags.iter().map(|f| f.long.as_str()).collect();
        assert!(flag_names.contains(&"primary-addr"));
        assert!(flag_names.contains(&"path"));
        assert!(flag_names.contains(&"bind"));
    }

    #[test]
    fn test_main_help_text() {
        let help = main_help_text();
        assert!(help.contains("reddb"));
        assert!(help.contains("Usage: red"));
        assert!(help.contains("Commands:"));
        assert!(help.contains("server"));
        assert!(help.contains("query"));
        assert!(help.contains("Global flags:"));
        assert!(help.contains("--help"));
        assert!(help.contains("Examples:"));
    }

    #[test]
    fn test_command_help_text() {
        let help = command_help_text("server").unwrap();
        assert!(help.contains("red server"));
        assert!(help.contains("--path"));
        assert!(help.contains("--bind"));
    }

    #[test]
    fn test_replica_command_help() {
        let help = command_help_text("replica").unwrap();
        assert!(help.contains("red replica"));
        assert!(help.contains("--primary-addr"));
    }

    #[test]
    fn test_command_help_text_unknown() {
        assert!(command_help_text("nonexistent").is_none());
    }

    #[test]
    fn test_flag_builder() {
        let flag = Flag::new("output", "Output format")
            .with_short('o')
            .with_default("text")
            .with_arg("FORMAT");

        assert_eq!(flag.long, "output");
        assert_eq!(flag.short, Some('o'));
        assert_eq!(flag.description, "Output format");
        assert_eq!(flag.default, Some("text".to_string()));
        assert_eq!(flag.arg, Some("FORMAT".to_string()));
    }

    #[test]
    fn test_completion_domains() {
        let domains = completion_domains();
        let names: Vec<&str> = domains.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"server"));
        assert!(names.contains(&"query"));
        assert!(names.contains(&"health"));
    }

    #[test]
    fn test_completion_global_flags() {
        let flags = completion_global_flags();
        assert!(flags.contains(&("help", Some('h'))));
        assert!(flags.contains(&("json", Some('j'))));
        assert!(flags.contains(&("verbose", Some('v'))));
        assert!(flags.contains(&("no-color", None)));
    }
}
