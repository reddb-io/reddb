//! Decode the producer's two hexadecimal fields while bytes arrive. Large rows
//! retain their binary payload once, without a JSON buffer and two hex strings.
use super::{invalid, json_whitespace, AppendOnlySegmentRow};
use std::io;

#[derive(Clone, Copy)]
enum State {
    Start,
    First,
    Next,
    Key,
    KeyString,
    Colon,
    Value,
    Hex,
    FieldEnd,
    RowEnd,
    Done,
}

pub(super) struct RowParser {
    state: State,
    key: Vec<u8>,
    key_escaped: bool,
    primary: Option<Vec<u8>>,
    payload: Option<Vec<u8>>,
    current: Vec<u8>,
    is_primary: bool,
    high: Option<u8>,
    escape: u8,
    unicode: u16,
    row_bytes: usize,
    max_row_bytes: usize,
}

impl RowParser {
    pub fn new(max_row_bytes: usize) -> Self {
        Self {
            state: State::Start,
            key: Vec::new(),
            key_escaped: false,
            primary: None,
            payload: None,
            current: Vec::new(),
            is_primary: false,
            high: None,
            escape: 0,
            unicode: 0,
            row_bytes: 0,
            max_row_bytes,
        }
    }

    fn digit(byte: u8) -> io::Result<u8> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(invalid("invalid segment hexadecimal string")),
        }
    }

    fn hex_byte(&mut self, byte: u8) -> io::Result<()> {
        let digit = Self::digit(byte)?;
        if let Some(high) = self.high.take() {
            if self.current.len() == self.current.capacity() {
                self.current
                    .try_reserve(64 * 1024)
                    .map_err(io::Error::other)?;
            }
            self.current.push((high << 4) | digit);
        } else {
            self.high = Some(digit);
        }
        Ok(())
    }

    pub fn consume(
        &mut self,
        bytes: &[u8],
        rows_left: &mut usize,
        visit: &mut impl FnMut(AppendOnlySegmentRow) -> io::Result<()>,
    ) -> io::Result<usize> {
        let mut consumed = 0;
        for &byte in bytes {
            if *rows_left == 0 {
                break;
            }
            consumed += 1;
            if matches!(
                self.state,
                State::Key
                    | State::KeyString
                    | State::Colon
                    | State::Value
                    | State::Hex
                    | State::FieldEnd
            ) {
                self.row_bytes = self
                    .row_bytes
                    .checked_add(1)
                    .ok_or_else(|| invalid("segment row size overflow"))?;
                if self.row_bytes > self.max_row_bytes {
                    return Err(invalid("segment row exceeds validation memory limit"));
                }
            }
            match self.state {
                State::Start if json_whitespace(byte) => {}
                State::Start if byte == b'[' => self.state = State::First,
                State::First | State::Next if json_whitespace(byte) => {}
                State::First if byte == b']' => self.state = State::Done,
                State::First | State::Next if byte == b'{' => {
                    self.row_bytes = 1;
                    self.primary = None;
                    self.payload = None;
                    self.state = State::Key;
                }
                State::Key if json_whitespace(byte) => {}
                State::Key if byte == b'"' => {
                    self.key.clear();
                    self.key.push(byte);
                    self.key_escaped = false;
                    self.state = State::KeyString;
                }
                State::KeyString => {
                    if self.key.len() >= 128 {
                        return Err(invalid("invalid segment row field"));
                    }
                    self.key.push(byte);
                    if self.key_escaped {
                        self.key_escaped = false;
                    } else if byte == b'\\' {
                        self.key_escaped = true;
                    } else if byte == b'"' {
                        let key: String = serde_json::from_slice(&self.key)
                            .map_err(|error| invalid(error.to_string()))?;
                        self.is_primary = match key.as_str() {
                            "primary_key_hex" if self.primary.is_none() => true,
                            "payload_hex" if self.payload.is_none() => false,
                            _ => return Err(invalid("unknown or duplicate segment row field")),
                        };
                        self.state = State::Colon;
                    }
                }
                State::Colon if json_whitespace(byte) => {}
                State::Colon if byte == b':' => self.state = State::Value,
                State::Value if json_whitespace(byte) => {}
                State::Value if byte == b'"' => {
                    self.current.clear();
                    self.high = None;
                    self.escape = 0;
                    self.state = State::Hex;
                }
                State::Hex => match self.escape {
                    1 if byte == b'u' => {
                        self.escape = 2;
                        self.unicode = 0;
                    }
                    1 => return Err(invalid("invalid escaped hexadecimal character")),
                    2..=5 => {
                        self.unicode = (self.unicode << 4) | u16::from(Self::digit(byte)?);
                        self.escape += 1;
                        if self.escape == 6 {
                            let decoded = u8::try_from(self.unicode)
                                .map_err(|_| invalid("non-ASCII hexadecimal character"))?;
                            self.hex_byte(decoded)?;
                            self.escape = 0;
                        }
                    }
                    _ if byte == b'\\' => self.escape = 1,
                    _ if byte == b'"' => {
                        if self.high.is_some() {
                            return Err(invalid("odd hexadecimal string length"));
                        }
                        let value = Some(std::mem::take(&mut self.current));
                        if self.is_primary {
                            self.primary = value;
                        } else {
                            self.payload = value;
                        }
                        self.state = State::FieldEnd;
                    }
                    _ => self.hex_byte(byte)?,
                },
                State::FieldEnd if json_whitespace(byte) => {}
                State::FieldEnd if byte == b',' => self.state = State::Key,
                State::FieldEnd if byte == b'}' => {
                    let primary_key = self
                        .primary
                        .take()
                        .ok_or_else(|| invalid("missing segment primary key"))?;
                    let payload = self
                        .payload
                        .take()
                        .ok_or_else(|| invalid("missing segment payload"))?;
                    visit(AppendOnlySegmentRow {
                        primary_key,
                        payload,
                    })?;
                    *rows_left -= 1;
                    self.state = State::RowEnd;
                }
                State::RowEnd if json_whitespace(byte) => {}
                State::RowEnd if byte == b',' => self.state = State::Next,
                State::RowEnd if byte == b']' => self.state = State::Done,
                State::Done if json_whitespace(byte) => {}
                _ => return Err(invalid("invalid append-only row array")),
            }
        }
        Ok(consumed)
    }

    pub fn finished(&self) -> bool {
        matches!(self.state, State::Done)
    }
}
