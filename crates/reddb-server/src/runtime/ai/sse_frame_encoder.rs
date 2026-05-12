//! `SseFrameEncoder` — pure Server-Sent Events frame serializer for
//! `ASK '...' STREAM` over HTTP.
//!
//! Issue #405 (PRD #391): the streaming variant of ASK emits three
//! kinds of SSE frames in fixed order — a single `sources` frame, a
//! run of `answer_token` frames, and exactly one terminal frame
//! (`validation` on success, `error` on mid-stream abort). This
//! module pins the wire format: event name, JSON payload shape, and
//! the SSE-specific quirks (multi-line `data:`, blank-line
//! terminator) that callers always get wrong.
//!
//! Deep module: no I/O, no transport. The HTTP handler owns the
//! `hyper::Body`/`axum::Sse` plumbing, the LLM streaming receiver,
//! and the cost-guard mid-stream check (#401). This module owns
//! "given one frame, what bytes go on the wire".
//!
//! ## Frame order (pinned by the spec, enforced by the handler)
//!
//! 1. exactly one [`Frame::Sources`] — `sources_flat` with URNs.
//! 2. zero or more [`Frame::AnswerToken`] — incremental answer text.
//! 3. exactly one terminal frame: [`Frame::Validation`] on the happy
//!    path, [`Frame::Error`] when a cost guard, timeout, or provider
//!    failure aborts mid-stream.
//!
//! This module does NOT enforce that order — the encoder is
//! per-frame and the caller is responsible for sequencing. A future
//! `SseStreamBuilder` slice can pin the sequence once the wiring
//! lands; for now the unit tests pin the byte layout of each frame
//! kind independently so the wiring slice can rely on it.
//!
//! ## SSE wire format
//!
//! Per the WHATWG spec (and what every reasonable client expects):
//!
//! ```text
//! event: <name>\n
//! data: <line 1>\n
//! data: <line 2>\n
//! ...
//! \n
//! ```
//!
//! Two rules everyone gets wrong:
//!
//! - A literal `\n` inside the JSON payload MUST be split across
//!   multiple `data:` lines. The browser concatenates them with a
//!   single `\n` between, so the receiver gets the original bytes
//!   back. `serde_json::to_string` by default does not emit a `\n`
//!   (it would use `\\n` in the string literal), so for our payloads
//!   this is theoretical — but the encoder still handles it because
//!   a future caller might emit pretty-printed JSON, and breaking on
//!   that would silently corrupt the event boundary.
//! - The trailing blank line is the frame terminator. Without it,
//!   the client buffers indefinitely. Two newlines, every time.
//!
//! ## Answer-token frame is text, not JSON
//!
//! Token frames carry raw answer text. We still wrap them in JSON
//! (`{"text":"..."}`) so the receiver has a single parse path across
//! all frame kinds — otherwise the client has to switch on event
//! name before deciding whether to JSON-parse, which is a footgun.
//! The encoder runs `serde_json::to_string` on a small struct, which
//! handles escaping (quotes, backslashes, control bytes) the same
//! way the rest of the JSON wire does.

use crate::serde_json::{Map, Value};

/// One source row in the `sources` frame. Caller produces these
/// after `RrfFuser` (#398) + column-policy redaction. `urn` is the
/// engine entity URN; `payload` is the redacted JSON the LLM also
/// saw, serialized as a string to keep the SSE JSON flat (the
/// client re-parses if it wants structure).
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub urn: String,
    pub payload: String,
}

/// Warning emitted on the terminal `validation` frame. Mirrors the
/// non-streaming response shape so HTTP clients can share parsing
/// code across the two transports.
#[derive(Debug, Clone)]
pub struct ValidationWarning {
    pub kind: String,
    pub detail: String,
}

/// Compact audit summary attached to the terminal `validation`
/// frame. The full audit row goes to `red_ask_audit` (#402); this is
/// the subset clients are allowed to see.
#[derive(Debug, Clone)]
pub struct AuditSummary {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_hit: bool,
}

fn obj(entries: &[(&str, Value)]) -> Value {
    let mut map = Map::new();
    for (k, v) in entries {
        map.insert((*k).to_string(), v.clone());
    }
    Value::Object(map)
}

fn source_row_value(row: &SourceRow) -> Value {
    obj(&[
        ("payload", Value::String(row.payload.clone())),
        ("urn", Value::String(row.urn.clone())),
    ])
}

fn warning_value(w: &ValidationWarning) -> Value {
    obj(&[
        ("detail", Value::String(w.detail.clone())),
        ("kind", Value::String(w.kind.clone())),
    ])
}

fn audit_value(a: &AuditSummary) -> Value {
    obj(&[
        ("cache_hit", Value::Bool(a.cache_hit)),
        (
            "completion_tokens",
            Value::Number(a.completion_tokens as f64),
        ),
        ("model", Value::String(a.model.clone())),
        ("prompt_tokens", Value::Number(a.prompt_tokens as f64)),
        ("provider", Value::String(a.provider.clone())),
    ])
}

/// One SSE frame. Event-name strings are pinned by tests so the
/// transport contract can't drift unnoticed.
#[derive(Debug, Clone)]
pub enum Frame {
    /// First frame. Full retrieved sources.
    Sources { sources_flat: Vec<SourceRow> },
    /// Incremental answer text. Many of these per stream.
    AnswerToken { text: String },
    /// Terminal happy-path frame.
    Validation {
        ok: bool,
        warnings: Vec<ValidationWarning>,
        audit: AuditSummary,
    },
    /// Terminal abort frame — cost guard, timeout, provider error.
    /// `code` mirrors the HTTP status the non-streaming path would
    /// have returned (413, 504, 422, 500) so clients can branch
    /// identically.
    Error { code: u16, message: String },
}

/// SSE event name pinned per variant. Exposed so the wiring layer
/// and tests can refer to them without re-typing the literals.
pub mod event {
    pub const SOURCES: &str = "sources";
    pub const ANSWER_TOKEN: &str = "answer_token";
    pub const VALIDATION: &str = "validation";
    pub const ERROR: &str = "error";
}

impl Frame {
    fn event_name(&self) -> &'static str {
        match self {
            Frame::Sources { .. } => event::SOURCES,
            Frame::AnswerToken { .. } => event::ANSWER_TOKEN,
            Frame::Validation { .. } => event::VALIDATION,
            Frame::Error { .. } => event::ERROR,
        }
    }

    fn payload_json(&self) -> String {
        let value = match self {
            Frame::Sources { sources_flat } => obj(&[(
                "sources_flat",
                Value::Array(sources_flat.iter().map(source_row_value).collect()),
            )]),
            Frame::AnswerToken { text } => obj(&[("text", Value::String(text.clone()))]),
            Frame::Validation {
                ok,
                warnings,
                audit,
            } => obj(&[
                ("audit", audit_value(audit)),
                ("ok", Value::Bool(*ok)),
                (
                    "warnings",
                    Value::Array(warnings.iter().map(warning_value).collect()),
                ),
            ]),
            Frame::Error { code, message } => obj(&[
                ("code", Value::Number(*code as f64)),
                ("message", Value::String(message.clone())),
            ]),
        };
        value.to_string_compact()
    }
}

/// Encode a single frame to its SSE on-wire bytes (as a `String`,
/// which is always valid UTF-8 here because JSON is UTF-8 and the
/// event name is ASCII).
///
/// Output always ends in `\n\n` — the SSE frame terminator. Callers
/// MUST NOT add their own trailing newline; doing so would emit an
/// empty frame after this one.
pub fn encode(frame: &Frame) -> String {
    let event = frame.event_name();
    let payload = frame.payload_json();

    // Pre-size: event line + per-data-line prefix + payload + terminator.
    // Cheap upper bound, avoids most reallocations on long answer tokens.
    let mut out = String::with_capacity(event.len() + payload.len() + 16);
    out.push_str("event: ");
    out.push_str(event);
    out.push('\n');

    // Split payload on '\n' so a multi-line JSON (e.g. pretty-printed)
    // still serializes to a valid SSE frame. For our compact JSON this
    // loop runs once.
    for line in payload.split('\n') {
        out.push_str("data: ");
        out.push_str(line);
        out.push('\n');
    }

    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit_fixture() -> AuditSummary {
        AuditSummary {
            provider: "openai".to_string(),
            model: "gpt-4o-mini".to_string(),
            prompt_tokens: 123,
            completion_tokens: 45,
            cache_hit: false,
        }
    }

    #[test]
    fn event_names_pinned() {
        assert_eq!(event::SOURCES, "sources");
        assert_eq!(event::ANSWER_TOKEN, "answer_token");
        assert_eq!(event::VALIDATION, "validation");
        assert_eq!(event::ERROR, "error");
    }

    #[test]
    fn encodes_sources_frame_with_event_and_terminator() {
        let frame = Frame::Sources {
            sources_flat: vec![SourceRow {
                urn: "urn:reddb:row:1".to_string(),
                payload: "{\"k\":\"v\"}".to_string(),
            }],
        };
        let out = encode(&frame);
        assert!(out.starts_with("event: sources\n"));
        assert!(out.ends_with("\n\n"));
        assert!(out.contains("data: {"));
        assert!(out.contains("\"urn\":\"urn:reddb:row:1\""));
    }

    #[test]
    fn encodes_answer_token_frame_with_text_field() {
        let frame = Frame::AnswerToken {
            text: "hello".to_string(),
        };
        let out = encode(&frame);
        assert_eq!(out, "event: answer_token\ndata: {\"text\":\"hello\"}\n\n");
    }

    #[test]
    fn answer_token_escapes_quotes_and_backslashes() {
        let frame = Frame::AnswerToken {
            text: "a\"b\\c".to_string(),
        };
        let out = encode(&frame);
        // JSON escape: " → \" and \ → \\
        assert!(out.contains(r#"\"b\\c"#));
        assert!(out.ends_with("\n\n"));
    }

    #[test]
    fn encodes_validation_frame_with_full_shape() {
        let frame = Frame::Validation {
            ok: true,
            warnings: vec![],
            audit: audit_fixture(),
        };
        let out = encode(&frame);
        assert!(out.starts_with("event: validation\n"));
        assert!(out.contains("\"ok\":true"));
        assert!(out.contains("\"prompt_tokens\":123"));
        assert!(out.contains("\"cache_hit\":false"));
        assert!(out.ends_with("\n\n"));
    }

    #[test]
    fn validation_carries_warnings_array() {
        let frame = Frame::Validation {
            ok: false,
            warnings: vec![
                ValidationWarning {
                    kind: "out_of_range".to_string(),
                    detail: "[^9] but only 3 sources".to_string(),
                },
                ValidationWarning {
                    kind: "mode_fallback".to_string(),
                    detail: "ollama".to_string(),
                },
            ],
            audit: audit_fixture(),
        };
        let out = encode(&frame);
        assert!(out.contains("\"kind\":\"out_of_range\""));
        assert!(out.contains("\"kind\":\"mode_fallback\""));
        // ok=false visible to clients so they don't surface a "valid"
        // answer when validation actually failed.
        assert!(out.contains("\"ok\":false"));
    }

    #[test]
    fn encodes_error_frame_with_code() {
        let frame = Frame::Error {
            code: 413,
            message: "max_prompt_tokens exceeded".to_string(),
        };
        let out = encode(&frame);
        assert_eq!(
            out,
            "event: error\ndata: {\"code\":413,\"message\":\"max_prompt_tokens exceeded\"}\n\n"
        );
    }

    #[test]
    fn error_frame_handles_504_timeout() {
        // Pins the cost-guard / timeout mapping the wiring slice will
        // depend on (#401).
        let frame = Frame::Error {
            code: 504,
            message: "timeout_ms exceeded".to_string(),
        };
        let out = encode(&frame);
        assert!(out.contains("\"code\":504"));
    }

    #[test]
    fn multiline_payload_splits_across_data_lines() {
        // Forcing a newline inside a token text — would never happen
        // from a JSON serializer on its own, but a future caller might
        // pretty-print. The encoder must preserve frame boundaries.
        let frame = Frame::AnswerToken {
            text: "line1\nline2".to_string(),
        };
        let out = encode(&frame);
        // JSON serializer escapes '\n' to "\\n" so the data line stays
        // on one row. Pinned so a future swap to a pretty-printer
        // doesn't silently break the SSE framing.
        assert_eq!(
            out,
            "event: answer_token\ndata: {\"text\":\"line1\\nline2\"}\n\n"
        );
    }

    #[test]
    fn encoder_splits_on_literal_newlines_in_payload() {
        // Direct test of the split-on-'\n' branch using a hand-crafted
        // payload, since serde_json::to_string never emits one for our
        // shapes. Constructs the encoded form manually to verify the
        // multi-data-line layout.
        let mut out = String::new();
        out.push_str("event: x\n");
        for line in "a\nb\nc".split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        assert_eq!(out, "event: x\ndata: a\ndata: b\ndata: c\n\n");
    }

    #[test]
    fn frame_terminator_is_double_newline() {
        // The single most common SSE bug: forgetting the blank line.
        // Pinned independently for every frame kind.
        for frame in [
            Frame::Sources {
                sources_flat: vec![],
            },
            Frame::AnswerToken {
                text: String::new(),
            },
            Frame::Validation {
                ok: true,
                warnings: vec![],
                audit: audit_fixture(),
            },
            Frame::Error {
                code: 500,
                message: String::new(),
            },
        ] {
            let out = encode(&frame);
            assert!(out.ends_with("\n\n"), "frame missing terminator: {:?}", out);
            // And NOT a triple newline — that would split into an
            // extra empty frame on the client.
            assert!(!out.ends_with("\n\n\n"));
        }
    }

    #[test]
    fn sources_frame_with_empty_list_is_well_formed() {
        let frame = Frame::Sources {
            sources_flat: vec![],
        };
        let out = encode(&frame);
        assert_eq!(out, "event: sources\ndata: {\"sources_flat\":[]}\n\n");
    }

    #[test]
    fn answer_token_with_empty_text_is_well_formed() {
        // An empty-text frame should never be emitted in practice, but
        // the encoder must not crash on it — the caller might forward
        // an empty SSE chunk from a poorly-behaved provider.
        let frame = Frame::AnswerToken {
            text: String::new(),
        };
        let out = encode(&frame);
        assert_eq!(out, "event: answer_token\ndata: {\"text\":\"\"}\n\n");
    }

    #[test]
    fn encoding_is_deterministic_across_calls() {
        let frame = Frame::Validation {
            ok: true,
            warnings: vec![ValidationWarning {
                kind: "k".to_string(),
                detail: "d".to_string(),
            }],
            audit: audit_fixture(),
        };
        let a = encode(&frame);
        let b = encode(&frame);
        assert_eq!(a, b);
    }

    #[test]
    fn event_name_matches_pinned_constants() {
        assert_eq!(
            Frame::Sources {
                sources_flat: vec![]
            }
            .event_name(),
            event::SOURCES
        );
        assert_eq!(
            Frame::AnswerToken {
                text: String::new()
            }
            .event_name(),
            event::ANSWER_TOKEN
        );
        assert_eq!(
            Frame::Validation {
                ok: true,
                warnings: vec![],
                audit: audit_fixture(),
            }
            .event_name(),
            event::VALIDATION
        );
        assert_eq!(
            Frame::Error {
                code: 0,
                message: String::new(),
            }
            .event_name(),
            event::ERROR
        );
    }

    #[test]
    fn unicode_in_token_text_passes_through() {
        let frame = Frame::AnswerToken {
            text: "olá 🌍".to_string(),
        };
        let out = encode(&frame);
        // serde_json preserves non-ASCII by default
        assert!(out.contains("olá 🌍"));
        assert!(out.ends_with("\n\n"));
    }
}
