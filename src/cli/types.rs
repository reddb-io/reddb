use std::collections::HashMap;

/// Value type for flag schema
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Bool,
    String,
    Integer,
    Float,
    Count, // -vvv => 3
}

/// Parsed flag value
#[derive(Debug, Clone, PartialEq)]
pub enum FlagValue {
    Bool(bool),
    Str(String),
    Int(i64),
    Float(f64),
    Count(u32),
}

impl FlagValue {
    pub fn as_str_value(&self) -> String {
        match self {
            FlagValue::Bool(b) => b.to_string(),
            FlagValue::Str(s) => s.clone(),
            FlagValue::Int(n) => n.to_string(),
            FlagValue::Float(f) => f.to_string(),
            FlagValue::Count(n) => n.to_string(),
        }
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            FlagValue::Bool(b) => *b,
            FlagValue::Str(s) => !s.is_empty(),
            FlagValue::Int(n) => *n != 0,
            FlagValue::Float(f) => *f != 0.0,
            FlagValue::Count(n) => *n > 0,
        }
    }
}

impl std::fmt::Display for FlagValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str_value())
    }
}

/// Schema for a single flag/option
#[derive(Debug, Clone)]
pub struct FlagSchema {
    pub long: String,
    pub short: Option<char>,
    pub description: String,
    pub value_type: ValueType,
    pub expects_value: bool,
    pub default: Option<String>,
    pub choices: Option<Vec<String>>,
    pub required: bool,
    pub hidden: bool,
}

impl FlagSchema {
    pub fn new(long: &str) -> Self {
        Self {
            long: long.to_string(),
            short: None,
            description: String::new(),
            value_type: ValueType::String,
            expects_value: true,
            default: None,
            choices: None,
            required: false,
            hidden: false,
        }
    }

    pub fn boolean(long: &str) -> Self {
        Self {
            long: long.to_string(),
            short: None,
            description: String::new(),
            value_type: ValueType::Bool,
            expects_value: false,
            default: None,
            choices: None,
            required: false,
            hidden: false,
        }
    }

    pub fn with_short(mut self, short: char) -> Self {
        self.short = Some(short);
        self
    }

    pub fn with_description(mut self, desc: &str) -> Self {
        self.description = desc.to_string();
        self
    }

    pub fn with_default(mut self, default: &str) -> Self {
        self.default = Some(default.to_string());
        self
    }

    pub fn with_choices(mut self, choices: &[&str]) -> Self {
        self.choices = Some(choices.iter().map(|s| s.to_string()).collect());
        self
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }
}

/// Resolved command path
#[derive(Debug, Clone, Default)]
pub struct CommandPath {
    pub domain: String,
    pub resource: Option<String>,
    pub verb: Option<String>,
}

impl CommandPath {
    pub fn is_complete(&self) -> bool {
        self.resource.is_some() && self.verb.is_some()
    }

    pub fn canonical(&self) -> String {
        let mut parts = vec![self.domain.clone()];
        if let Some(ref r) = self.resource {
            parts.push(r.clone());
        }
        if let Some(ref v) = self.verb {
            parts.push(v.clone());
        }
        parts.join("/")
    }
}

/// Result of parsing a complete command line
#[derive(Debug, Clone)]
pub struct ParsedCommand {
    pub path: CommandPath,
    pub target: Option<String>,
    pub positional_args: Vec<String>,
    pub flags: HashMap<String, FlagValue>,
    pub raw: Vec<String>,
}

impl ParsedCommand {
    pub fn new() -> Self {
        Self {
            path: CommandPath::default(),
            target: None,
            positional_args: Vec::new(),
            flags: HashMap::new(),
            raw: Vec::new(),
        }
    }

    pub fn get_flag(&self, name: &str) -> Option<&str> {
        match self.flags.get(name)? {
            FlagValue::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn has_flag(&self, name: &str) -> bool {
        self.flags.get(name).map_or(false, |v| v.is_truthy())
    }
}

/// Global flags that apply to all RedDB commands
pub fn global_flags() -> Vec<FlagSchema> {
    vec![
        FlagSchema::boolean("json")
            .with_short('j')
            .with_description("Force JSON output"),
        FlagSchema::boolean("help")
            .with_short('h')
            .with_description("Show help"),
        FlagSchema::boolean("version").with_description("Show version"),
        FlagSchema::new("output")
            .with_short('o')
            .with_description("Output format")
            .with_choices(&["text", "json", "yaml"]),
        FlagSchema::boolean("no-color").with_description("Disable colors"),
        FlagSchema::boolean("verbose")
            .with_short('v')
            .with_description("Verbose output"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flag_schema_builder() {
        let flag = FlagSchema::new("output")
            .with_short('o')
            .with_description("Output format")
            .with_default("text")
            .with_choices(&["text", "json"])
            .required();

        assert_eq!(flag.long, "output");
        assert_eq!(flag.short, Some('o'));
        assert_eq!(flag.description, "Output format");
        assert_eq!(flag.default, Some("text".to_string()));
        assert_eq!(
            flag.choices,
            Some(vec!["text".to_string(), "json".to_string()])
        );
        assert!(flag.required);
        assert!(flag.expects_value);
        assert_eq!(flag.value_type, ValueType::String);

        let bool_flag = FlagSchema::boolean("verbose").hidden();
        assert_eq!(bool_flag.value_type, ValueType::Bool);
        assert!(!bool_flag.expects_value);
        assert!(bool_flag.hidden);
    }

    #[test]
    fn test_flag_value_as_str() {
        assert_eq!(FlagValue::Bool(true).as_str_value(), "true");
        assert_eq!(FlagValue::Bool(false).as_str_value(), "false");
        assert_eq!(FlagValue::Str("hello".into()).as_str_value(), "hello");
        assert_eq!(FlagValue::Int(42).as_str_value(), "42");
        assert_eq!(FlagValue::Float(3.14).as_str_value(), "3.14");
        assert_eq!(FlagValue::Count(3).as_str_value(), "3");
    }

    #[test]
    fn test_flag_value_is_truthy() {
        assert!(FlagValue::Bool(true).is_truthy());
        assert!(!FlagValue::Bool(false).is_truthy());
        assert!(FlagValue::Str("yes".into()).is_truthy());
        assert!(!FlagValue::Str(String::new()).is_truthy());
        assert!(FlagValue::Int(1).is_truthy());
        assert!(!FlagValue::Int(0).is_truthy());
        assert!(FlagValue::Float(0.1).is_truthy());
        assert!(!FlagValue::Float(0.0).is_truthy());
        assert!(FlagValue::Count(1).is_truthy());
        assert!(!FlagValue::Count(0).is_truthy());
    }

    #[test]
    fn test_command_path_canonical() {
        let full = CommandPath {
            domain: "server".into(),
            resource: Some("grpc".into()),
            verb: Some("start".into()),
        };
        assert_eq!(full.canonical(), "server/grpc/start");

        let partial = CommandPath {
            domain: "query".into(),
            resource: Some("sql".into()),
            verb: None,
        };
        assert_eq!(partial.canonical(), "query/sql");

        let domain_only = CommandPath {
            domain: "health".into(),
            resource: None,
            verb: None,
        };
        assert_eq!(domain_only.canonical(), "health");
    }

    #[test]
    fn test_command_path_is_complete() {
        let complete = CommandPath {
            domain: "server".into(),
            resource: Some("grpc".into()),
            verb: Some("start".into()),
        };
        assert!(complete.is_complete());

        let incomplete = CommandPath {
            domain: "server".into(),
            resource: Some("grpc".into()),
            verb: None,
        };
        assert!(!incomplete.is_complete());

        let minimal = CommandPath {
            domain: "health".into(),
            resource: None,
            verb: None,
        };
        assert!(!minimal.is_complete());
    }

    #[test]
    fn test_parsed_command_get_flag() {
        let mut cmd = ParsedCommand::new();
        cmd.flags
            .insert("output".into(), FlagValue::Str("json".into()));
        cmd.flags.insert("verbose".into(), FlagValue::Bool(true));

        assert_eq!(cmd.get_flag("output"), Some("json"));
        assert_eq!(cmd.get_flag("verbose"), None); // not a Str variant
        assert_eq!(cmd.get_flag("missing"), None);
    }

    #[test]
    fn test_parsed_command_has_flag() {
        let mut cmd = ParsedCommand::new();
        cmd.flags.insert("verbose".into(), FlagValue::Bool(true));
        cmd.flags.insert("quiet".into(), FlagValue::Bool(false));
        cmd.flags.insert("count".into(), FlagValue::Count(3));

        assert!(cmd.has_flag("verbose"));
        assert!(!cmd.has_flag("quiet"));
        assert!(cmd.has_flag("count"));
        assert!(!cmd.has_flag("missing"));
    }

    #[test]
    fn test_global_flags_defined() {
        let flags = global_flags();
        assert!(flags.len() >= 6);

        let names: Vec<&str> = flags.iter().map(|f| f.long.as_str()).collect();
        assert!(names.contains(&"json"));
        assert!(names.contains(&"help"));
        assert!(names.contains(&"version"));
        assert!(names.contains(&"output"));
        assert!(names.contains(&"no-color"));
        assert!(names.contains(&"verbose"));

        let output = flags.iter().find(|f| f.long == "output").unwrap();
        assert!(output.expects_value);
        assert!(output.choices.is_some());

        let help = flags.iter().find(|f| f.long == "help").unwrap();
        assert_eq!(help.short, Some('h'));
        assert!(!help.expects_value);
    }
}
