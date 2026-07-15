//! Minimal JSON parser and serializer with zero dependencies.
//! Implements a subset of JSON sufficient for MCP message handling.

use std::fmt;

/// Simplified JSON value representation.
#[derive(Clone, Debug, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Integer(i64),
    Number(f64),
    /// Beyond-native-range decimal stored as exact text (emitted as raw JSON number, never quoted)
    Decimal(String),
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
            JsonValue::Integer(n) => Some(*n as f64),
            JsonValue::Decimal(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    /// Returns the value as i64 if it is an exact integer.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            JsonValue::Integer(n) => Some(*n),
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
    ///
    /// **Deprecation note (ADR 0010 / issue #177):** the canonical
    /// JSON encoder for serialization-boundary-sensitive paths
    /// (audit log, HelloAck, PayloadReply, anything reaching a
    /// downstream parser) is `crate::serde_json::Value::escape_string`
    /// using `to_string_compact`. This local encoder is correct after
    /// the F-01 hotfix (#181) but is not the canonical owner; new
    /// audit / wire emission code should not call it. Existing MCP
    /// JSON-RPC callers may keep using it pending a follow-up
    /// retirement slice.
    #[deprecated(
        note = "Use crate::serde_json::Value::to_string_compact for boundary emission; see ADR 0010 / issue #177"
    )]
    pub fn to_json_string(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            JsonValue::Null => out.push_str("null"),
            JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            JsonValue::Integer(n) => out.push_str(&format!("{n}")),
            JsonValue::Number(n) => {
                if n.fract() == 0.0 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{}", n));
                }
            }
            JsonValue::Decimal(s) => {
                // Emit as raw JSON number (no quotes) — exact decimal text
                out.push_str(s);
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
        JsonValue::Integer(value)
    }
}

impl From<usize> for JsonValue {
    fn from(value: usize) -> JsonValue {
        i64::try_from(value)
            .map(JsonValue::Integer)
            .unwrap_or_else(|_| JsonValue::Number(value as f64))
    }
}

impl fmt::Display for JsonValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Internal route — Display is the legacy entry point and
        // routes through the (now deprecated for boundary use)
        // `to_json_string`. Silence the warning here so the lint
        // surfaces only at external call sites.
        #[allow(deprecated)]
        let s = self.to_json_string();
        write!(f, "{s}")
    }
}

/// Count the significant digits in a decimal number's textual form.
///
/// Ignores the sign, the decimal point, any exponent suffix, and leading
/// zeros. Used to decide whether a float literal exceeds f64's exact
/// precision (17 significant decimal digits) and must be preserved as
/// exact decimal text instead of a lossy `f64`.
fn count_significant_digits(s: &str) -> usize {
    let s = s.trim_start_matches('-');
    // Split off any exponent — its digits do not count toward the mantissa's
    // significant digits (e.g. `1.5e10` has 2 significant digits, not 12).
    let s = match s.split_once(|c| c == 'e' || c == 'E') {
        Some((mantissa, _)) => mantissa,
        None => s,
    };
    // Remove the decimal point, then strip leading zeros.
    let s = s.replace('.', "");
    let s = s.trim_start_matches('0');
    if s.is_empty() {
        return 1;
    }
    s.len()
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
                while !self.eof() && self.current_char().is_ascii_digit() {
                    self.pos += 1;
                }
            }
            _ => return Err("invalid number literal".to_string()),
        }

        let mut is_float = false;

        if !self.eof() && self.current_char() == b'.' {
            is_float = true;
            self.pos += 1;
            if self.eof() || !self.current_char().is_ascii_digit() {
                return Err("invalid number literal".to_string());
            }
            while !self.eof() && self.current_char().is_ascii_digit() {
                self.pos += 1;
            }
        }

        if !self.eof() && (self.current_char() == b'e' || self.current_char() == b'E') {
            is_float = true;
            self.pos += 1;
            if !self.eof() && (self.current_char() == b'+' || self.current_char() == b'-') {
                self.pos += 1;
            }
            if self.eof() || !self.current_char().is_ascii_digit() {
                return Err("invalid number literal".to_string());
            }
            while !self.eof() && self.current_char().is_ascii_digit() {
                self.pos += 1;
            }
        }

        let slice = &self.input[start..self.pos];
        let s = std::str::from_utf8(slice).map_err(|_| "invalid UTF-8 in number".to_string())?;
        if !is_float {
            if let Ok(value) = s.parse::<i64>() {
                return Ok(JsonValue::Integer(value));
            }
            // Beyond i64 range: preserve as exact decimal text.
            return Ok(JsonValue::Decimal(s.to_string()));
        }
        // Floats with more than 17 significant digits exceed f64's exact
        // precision — keep them as exact decimal text rather than losing digits.
        let sig_digits = count_significant_digits(s);
        let value = s
            .parse::<f64>()
            .map_err(|_| "failed to parse number".to_string())?;
        if sig_digits > 17 {
            return Ok(JsonValue::Decimal(s.to_string()));
        }
        Ok(JsonValue::Number(value))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect_char(b'"')?;
        let mut result = String::new();
        while !self.eof() {
            let ch = self.current_char();
            match ch {
                b'"' => {
                    self.pos += 1;
                    return Ok(result);
                }
                b'\\' => {
                    self.pos += 1;
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
                            // RFC 8259 §7: `\uXXXX` covers code points in
                            // the BMP. Code points above U+FFFF (e.g. emoji,
                            // U+1F4A9 PILE OF POO) are represented in JSON
                            // as a UTF-16 surrogate pair `💩` and
                            // MUST be decoded as one Unicode scalar.
                            let code = self.parse_unicode_escape()?;
                            if (0xD800..=0xDBFF).contains(&code) {
                                // High surrogate — must be followed by a
                                // `\uXXXX` low surrogate.
                                if self.pos + 2 > self.input.len()
                                    || self.input[self.pos] != b'\\'
                                    || self.input[self.pos + 1] != b'u'
                                {
                                    return Err(
                                        "expected low surrogate after high surrogate".to_string()
                                    );
                                }
                                self.pos += 2;
                                let low = self.parse_unicode_escape()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err("invalid low surrogate".to_string());
                                }
                                let scalar = 0x10000 + (((code - 0xD800) << 10) | (low - 0xDC00));
                                if let Some(chr) = char::from_u32(scalar) {
                                    result.push(chr);
                                } else {
                                    return Err("invalid unicode escape".to_string());
                                }
                            } else if (0xDC00..=0xDFFF).contains(&code) {
                                return Err(
                                    "unexpected low surrogate without preceding high surrogate"
                                        .to_string(),
                                );
                            } else if let Some(chr) = char::from_u32(code) {
                                result.push(chr);
                            } else {
                                return Err("invalid unicode escape".to_string());
                            }
                        }
                        _ => return Err("invalid escape sequence".to_string()),
                    }
                }
                _ if ch < 0x80 => {
                    // ASCII fast path: byte == codepoint.
                    self.pos += 1;
                    result.push(ch as char);
                }
                _ => {
                    // Multi-byte UTF-8 sequence. The parser's input came
                    // from `&str` (validated UTF-8 at JsonParser::new), so
                    // a complete codepoint starts here. Decoding byte-by-
                    // byte via `ch as char` would map each continuation
                    // byte to a Latin-1 codepoint, producing mojibake (the
                    // root cause of issue #191 — `é` → `Ã©`, `🦀` → four
                    // garbled chars). Instead, lift a full codepoint out
                    // of the underlying UTF-8 stream.
                    let next = std::str::from_utf8(&self.input[self.pos..])
                        .map_err(|_| "invalid utf-8 in string body".to_string())?
                        .chars()
                        .next()
                        .ok_or_else(|| "unterminated string literal".to_string())?;
                    self.pos += next.len_utf8();
                    result.push(next);
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
        let json = r#"{"name":"reddb","active":true,"count":3}"#;
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
        #[allow(deprecated)]
        let output = value.to_json_string();
        assert!(output.contains("hello"));
    }

    #[test]
    fn constructors_accessors_mutators_and_display_cover_all_shapes() {
        let mut value = JsonValue::object(vec![
            ("name".to_string(), JsonValue::from("reddb")),
            ("active".to_string(), JsonValue::from(true)),
            ("count".to_string(), JsonValue::from(3usize)),
            ("ratio".to_string(), JsonValue::from(1.5f64)),
            (
                "items".to_string(),
                JsonValue::array(vec![JsonValue::Null, JsonValue::from(2i64)]),
            ),
        ]);

        assert_eq!(value.get("name").and_then(JsonValue::as_str), Some("reddb"));
        assert_eq!(value.get("active").and_then(JsonValue::as_bool), Some(true));
        assert_eq!(value.get("count").and_then(JsonValue::as_f64), Some(3.0));
        assert_eq!(
            value
                .get("items")
                .and_then(JsonValue::as_array)
                .map(<[_]>::len),
            Some(2)
        );
        assert_eq!(value.as_object().map(<[_]>::len), Some(5));
        assert!(JsonValue::Null.as_str().is_none());
        assert!(JsonValue::Null.as_bool().is_none());
        assert!(JsonValue::Null.as_array().is_none());
        assert!(JsonValue::Null.as_object().is_none());

        value
            .get_mut("count")
            .map(|slot| *slot = JsonValue::from(4usize))
            .unwrap();
        assert_eq!(value.get("count").and_then(JsonValue::as_f64), Some(4.0));

        value
            .as_object_mut()
            .unwrap()
            .push(("extra".to_string(), JsonValue::from("ok".to_string())));
        let mut array = JsonValue::array(vec![JsonValue::from(1usize)]);
        array.as_array_mut().unwrap().push(JsonValue::from(2usize));
        assert_eq!(array.as_array().unwrap().len(), 2);

        #[allow(deprecated)]
        let encoded =
            JsonValue::String("quote\" slash\\ tab\t ctrl\x01".to_string()).to_json_string();
        assert!(encoded.contains("\\\""));
        assert!(encoded.contains("\\\\"));
        assert!(encoded.contains("\\t"));
        assert!(encoded.contains("\\u0001"));
        assert_eq!(format!("{}", JsonValue::Bool(false)), "false");
    }

    #[test]
    fn parser_accepts_scalars_escapes_empty_containers_and_trailing_space() {
        assert_eq!(parse_json(" null \n").unwrap(), JsonValue::Null);
        assert_eq!(parse_json("true").unwrap(), JsonValue::Bool(true));
        assert_eq!(parse_json("false").unwrap(), JsonValue::Bool(false));
        assert_eq!(parse_json("-12.5e+2").unwrap(), JsonValue::Number(-1250.0));
        assert_eq!(parse_json("[]").unwrap(), JsonValue::Array(Vec::new()));
        assert_eq!(parse_json("{}").unwrap(), JsonValue::Object(Vec::new()));

        let escaped = parse_json(r#""\"\\\/\b\f\n\r\t\u00e9\uD83D\uDCA9""#).unwrap();
        let text = escaped.as_str().unwrap();
        assert!(text.contains('"'));
        assert!(text.contains('\\'));
        assert!(text.contains('/'));
        assert!(text.contains('\x08'));
        assert!(text.contains('\x0c'));
        assert!(text.contains('\n'));
        assert!(text.contains('\r'));
        assert!(text.contains('\t'));
        assert!(text.chars().any(|ch| ch as u32 == 0x00E9));
        assert_eq!(text.chars().last(), char::from_u32(0x1F4A9));
    }

    #[test]
    fn beyond_range_integers_and_high_precision_floats_parse_as_decimal() {
        // Beyond i64::MAX
        let v = parse_json("18446744073709551615").unwrap();
        assert!(matches!(v, JsonValue::Decimal(_)));

        // Negative beyond i64::MIN
        let v = parse_json("-9999999999999999999").unwrap();
        assert!(matches!(v, JsonValue::Decimal(_)));

        // High-precision decimal (>17 significant digits)
        let v = parse_json("3.14159265358979323846").unwrap();
        assert!(matches!(v, JsonValue::Decimal(_)));

        // Normal integer within i64 range → Integer
        let v = parse_json("42").unwrap();
        assert!(matches!(v, JsonValue::Integer(42)));

        // Normal float with few sig digits → Number
        let v = parse_json("3.14").unwrap();
        assert!(matches!(v, JsonValue::Number(_)));
    }

    #[test]
    fn parser_rejects_invalid_tokens_numbers_strings_and_separators() {
        let cases = [
            "",
            "truth",
            "nul",
            "-",
            "01",
            "1.",
            "1e",
            r#""unterminated"#,
            r#""\x""#,
            r#""\u12""#,
            r#""\u12zz""#,
            r#""\uD800""#,
            r#""\uD800\u0000""#,
            r#""\uDC00""#,
            "[1 2]",
            "[1,]",
            r#"{"a" 1}"#,
            r#"{"a":1 "b":2}"#,
            "{} trailing",
            "?",
        ];

        for case in cases {
            assert!(parse_json(case).is_err(), "{case:?}");
        }
    }
}
