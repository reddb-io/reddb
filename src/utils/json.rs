//! Minimal JSON parser and serializer with zero dependencies.
//! Implements a subset of JSON sufficient for MCP message handling.

use std::fmt;

/// Simplified JSON value representation.
#[derive(Clone, Debug, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    /// Returns the value as string reference if it is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Returns the value as f64 if it is a number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            JsonValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Returns the value as boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns the value as array.
    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(items) => Some(items.as_slice()),
            _ => None,
        }
    }

    /// Returns the value as mutable array.
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<JsonValue>> {
        match self {
            JsonValue::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Returns the object entries if the value is an object.
    pub fn as_object(&self) -> Option<&[(String, JsonValue)]> {
        match self {
            JsonValue::Object(entries) => Some(entries.as_slice()),
            _ => None,
        }
    }

    /// Returns a mutable reference to the object entries.
    pub fn as_object_mut(&mut self) -> Option<&mut Vec<(String, JsonValue)>> {
        match self {
            JsonValue::Object(entries) => Some(entries),
            _ => None,
        }
    }

    /// Retrieves field value from object by key.
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        if let JsonValue::Object(entries) = self {
            for (k, v) in entries {
                if k == key {
                    return Some(v);
                }
            }
        }
        None
    }

    /// Retrieves mutable field value from object by key.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut JsonValue> {
        if let JsonValue::Object(entries) = self {
            for (k, v) in entries.iter_mut() {
                if k == key {
                    return Some(v);
                }
            }
        }
        None
    }

    /// Convenience constructor for JSON objects.
    pub fn object(entries: Vec<(String, JsonValue)>) -> JsonValue {
        JsonValue::Object(entries)
    }

    /// Convenience constructor for JSON arrays.
    pub fn array(items: Vec<JsonValue>) -> JsonValue {
        JsonValue::Array(items)
    }

    /// Serializes the value into a compact JSON string.
    pub fn to_json_string(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            JsonValue::Null => out.push_str("null"),
            JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            JsonValue::Number(n) => {
                if n.fract() == 0.0 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{}", n));
                }
            }
            JsonValue::String(s) => {
                out.push('"');
                for ch in s.chars() {
                    match ch {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if c.is_control() => {
                            out.push_str(&format!("\\u{:04x}", c as u32));
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            JsonValue::Array(items) => {
                out.push('[');
                for (idx, item) in items.iter().enumerate() {
                    if idx > 0 {
                        out.push(',');
                    }
                    item.write_json(out);
                }
                out.push(']');
            }
            JsonValue::Object(entries) => {
                out.push('{');
                for (idx, (key, value)) in entries.iter().enumerate() {
                    if idx > 0 {
                        out.push(',');
                    }
                    JsonValue::String(key.clone()).write_json(out);
                    out.push(':');
                    value.write_json(out);
                }
                out.push('}');
            }
        }
    }
}

impl From<&str> for JsonValue {
    fn from(value: &str) -> JsonValue {
        JsonValue::String(value.to_string())
    }
}

impl From<String> for JsonValue {
    fn from(value: String) -> JsonValue {
        JsonValue::String(value)
    }
}

impl From<bool> for JsonValue {
    fn from(value: bool) -> JsonValue {
        JsonValue::Bool(value)
    }
}

impl From<f64> for JsonValue {
    fn from(value: f64) -> JsonValue {
        JsonValue::Number(value)
    }
}

impl From<i64> for JsonValue {
    fn from(value: i64) -> JsonValue {
        JsonValue::Number(value as f64)
    }
}

impl From<usize> for JsonValue {
    fn from(value: usize) -> JsonValue {
        JsonValue::Number(value as f64)
    }
}

impl fmt::Display for JsonValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_json_string())
    }
}

/// Parser for JSON strings into [`JsonValue`].
pub struct JsonParser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    /// Creates a new parser from input slice.
    pub fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    /// Parses a JSON value from the current position.
    pub fn parse_value(&mut self) -> Result<JsonValue, String> {
        self.skip_whitespace();
        if self.eof() {
            return Err("unexpected end of input".to_string());
        }
        let ch = self.current_char();
        match ch {
            b'n' => self.parse_null(),
            b't' | b'f' => self.parse_bool(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            b'"' => self.parse_string().map(JsonValue::String),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            _ => Err(format!("unexpected character '{}'", ch as char)),
        }
    }

    fn parse_null(&mut self) -> Result<JsonValue, String> {
        self.expect_bytes(b"null")?;
        Ok(JsonValue::Null)
    }

    fn parse_bool(&mut self) -> Result<JsonValue, String> {
        if self.matches_bytes(b"true") {
            self.pos += 4;
            Ok(JsonValue::Bool(true))
        } else if self.matches_bytes(b"false") {
            self.pos += 5;
            Ok(JsonValue::Bool(false))
        } else {
            Err("invalid boolean literal".to_string())
        }
    }

    fn parse_number(&mut self) -> Result<JsonValue, String> {
        let start = self.pos;
        if self.current_char() == b'-' {
            self.pos += 1;
        }
        if self.eof() {
            return Err("invalid number literal".to_string());
        }

        match self.current_char() {
            b'0' => {
                self.pos += 1;
            }
            b'1'..=b'9' => {
                self.pos += 1;
                while !self.eof() && matches!(self.current_char(), b'0'..=b'9') {
                    self.pos += 1;
                }
            }
            _ => return Err("invalid number literal".to_string()),
        }

        if !self.eof() && self.current_char() == b'.' {
            self.pos += 1;
            if self.eof() || !matches!(self.current_char(), b'0'..=b'9') {
                return Err("invalid number literal".to_string());
            }
            while !self.eof() && matches!(self.current_char(), b'0'..=b'9') {
                self.pos += 1;
            }
        }

        if !self.eof() && (self.current_char() == b'e' || self.current_char() == b'E') {
            self.pos += 1;
            if !self.eof() && (self.current_char() == b'+' || self.current_char() == b'-') {
                self.pos += 1;
            }
            if self.eof() || !matches!(self.current_char(), b'0'..=b'9') {
                return Err("invalid number literal".to_string());
            }
            while !self.eof() && matches!(self.current_char(), b'0'..=b'9') {
                self.pos += 1;
            }
        }

        let slice = &self.input[start..self.pos];
        let s = std::str::from_utf8(slice).map_err(|_| "invalid UTF-8 in number".to_string())?;
        let value = s
            .parse::<f64>()
            .map_err(|_| "failed to parse number".to_string())?;
        Ok(JsonValue::Number(value))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect_char(b'"')?;
        let mut result = String::new();
        while !self.eof() {
            let ch = self.current_char();
            self.pos += 1;
            match ch {
                b'"' => return Ok(result),
                b'\\' => {
                    if self.eof() {
                        return Err("unexpected end of input in escape".to_string());
                    }
                    let esc = self.current_char();
                    self.pos += 1;
                    match esc {
                        b'"' => result.push('"'),
                        b'\\' => result.push('\\'),
                        b'/' => result.push('/'),
                        b'b' => result.push('\x08'),
                        b'f' => result.push('\x0c'),
                        b'n' => result.push('\n'),
                        b'r' => result.push('\r'),
                        b't' => result.push('\t'),
                        b'u' => {
                            let code = self.parse_unicode_escape()?;
                            if let Some(chr) = char::from_u32(code) {
                                result.push(chr);
                            } else {
                                return Err("invalid unicode escape".to_string());
                            }
                        }
                        _ => return Err("invalid escape sequence".to_string()),
                    }
                }
                _ => {
                    result.push(ch as char);
                }
            }
        }
        Err("unterminated string literal".to_string())
    }

    fn parse_unicode_escape(&mut self) -> Result<u32, String> {
        if self.pos + 4 > self.input.len() {
            return Err("invalid unicode escape".to_string());
        }
        let mut value = 0u32;
        for _ in 0..4 {
            let ch = self.current_char();
            self.pos += 1;
            value <<= 4;
            value |= match ch {
                b'0'..=b'9' => (ch - b'0') as u32,
                b'a'..=b'f' => (ch - b'a' + 10) as u32,
                b'A'..=b'F' => (ch - b'A' + 10) as u32,
                _ => return Err("invalid unicode escape".to_string()),
            };
        }
        Ok(value)
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.expect_char(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek_char() == Some(b']') {
            self.pos += 1;
            return Ok(JsonValue::Array(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_whitespace();
            match self.peek_char() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err("expected ',' or ']' in array".to_string()),
            }
        }
        Ok(JsonValue::Array(items))
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.expect_char(b'{')?;
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.peek_char() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonValue::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect_char(b':')?;
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.peek_char() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err("expected ',' or '}' in object".to_string()),
            }
        }
        Ok(JsonValue::Object(entries))
    }

    fn skip_whitespace(&mut self) {
        while !self.eof() {
            match self.current_char() {
                b' ' | b'\n' | b'\r' | b'\t' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn expect_bytes(&mut self, expected: &[u8]) -> Result<(), String> {
        if self.remaining().starts_with(expected) {
            self.pos += expected.len();
            Ok(())
        } else {
            Err("unexpected token".to_string())
        }
    }

    fn matches_bytes(&self, expected: &[u8]) -> bool {
        self.remaining().starts_with(expected)
    }

    fn expect_char(&mut self, expected: u8) -> Result<(), String> {
        if self.peek_char() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected '{}'", expected as char))
        }
    }

    fn peek_char(&self) -> Option<u8> {
        if self.pos >= self.input.len() {
            None
        } else {
            Some(self.input[self.pos])
        }
    }

    fn current_char(&self) -> u8 {
        self.input[self.pos]
    }

    fn remaining(&self) -> &[u8] {
        &self.input[self.pos..]
    }

    fn eof(&self) -> bool {
        self.pos >= self.input.len()
    }
}

/// Parses the provided JSON string into a [`JsonValue`].
pub fn parse_json(input: &str) -> Result<JsonValue, String> {
    let mut parser = JsonParser::new(input);
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.eof() {
        Ok(value)
    } else {
        Err("unexpected trailing data".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_object() {
        let json = r#"{"name":"redblue","active":true,"count":3}"#;
        let value = parse_json(json).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj.len(), 3);
    }

    #[test]
    fn parse_nested_array() {
        let json = r#"{"items":[1,2,{"flag":false}]}"#;
        let value = parse_json(json).unwrap();
        assert!(value.get("items").unwrap().as_array().is_some());
    }

    #[test]
    fn stringify_roundtrip() {
        let json = r#"{"message":"hello","value":42}"#;
        let value = parse_json(json).unwrap();
        let output = value.to_json_string();
        assert!(output.contains("hello"));
    }
}
