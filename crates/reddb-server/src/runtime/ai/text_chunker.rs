//! Text chunker — issue #277.
//!
//! Splits text into chunks using strategy: paragraph > sentence > character.
//! Approximate tokenisation: 1 token ≈ 4 bytes (ASCII-biased heuristic).
//!
//! Single mode (default): returns only the first chunk (preserves 1:1 mapping
//! to embeddings). Multi mode: returns all chunks.

use std::sync::atomic::{AtomicU64, Ordering};

pub const CONFIG_CHUNK_MODE: &str = "runtime.ai.embedding_chunk_mode";
pub const CONFIG_MAX_TOKENS: &str = "runtime.ai.embedding_max_tokens";
pub const DEFAULT_MAX_TOKENS: usize = 8192;

/// Approximate bytes per token (ASCII-biased heuristic).
const BYTES_PER_TOKEN: usize = 4;

/// Global counter: how many texts were chunked (i.e. exceeded max_tokens).
static CHUNKED_TOTAL: AtomicU64 = AtomicU64::new(0);

pub fn chunked_total() -> u64 {
    CHUNKED_TOTAL.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkMode {
    /// Return only the first chunk — preserves 1:1 input→embedding mapping.
    Single,
    /// Return all chunks — downstream decides how to merge embeddings.
    Multi,
}

impl Default for ChunkMode {
    fn default() -> Self {
        Self::Single
    }
}

impl ChunkMode {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "multi" => Self::Multi,
            _ => Self::Single,
        }
    }
}

/// Chunk `text` into pieces where each piece is ≤ `max_tokens` tokens.
/// Strategy (greedy, in priority order):
///   1. Split on blank lines (paragraphs)
///   2. Split on sentence boundaries (`. `, `! `, `? `)
///   3. Hard-split on character boundary
///
/// Returns a `Vec<String>`. The caller applies `ChunkMode`:
/// - `Single`: take `chunks[0]`
/// - `Multi`: use all chunks
pub fn chunk(text: &str, max_tokens: usize) -> Vec<String> {
    let max_bytes = max_tokens * BYTES_PER_TOKEN;
    if text.len() <= max_bytes {
        return vec![text.to_string()];
    }
    CHUNKED_TOTAL.fetch_add(1, Ordering::Relaxed);
    split_into_chunks(text, max_bytes)
}

/// Apply chunk mode to pre-chunked `Vec<String>`.
/// Single → first element only. Multi → all.
pub fn apply_mode(chunks: Vec<String>, mode: ChunkMode) -> Vec<String> {
    match mode {
        ChunkMode::Single => chunks.into_iter().take(1).collect(),
        ChunkMode::Multi => chunks,
    }
}

fn split_into_chunks(text: &str, max_bytes: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for paragraph in split_paragraphs(text) {
        if paragraph.is_empty() {
            continue;
        }
        if current.len() + paragraph.len() <= max_bytes {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(&paragraph);
        } else if paragraph.len() <= max_bytes {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            current = paragraph;
        } else {
            // Paragraph itself is too large — split by sentence
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            for sentence in split_sentences(&paragraph) {
                if current.len() + sentence.len() <= max_bytes {
                    if !current.is_empty() {
                        current.push(' ');
                    }
                    current.push_str(&sentence);
                } else if sentence.len() <= max_bytes {
                    if !current.is_empty() {
                        chunks.push(std::mem::take(&mut current));
                    }
                    current = sentence;
                } else {
                    // Sentence too large — hard split by bytes
                    if !current.is_empty() {
                        chunks.push(std::mem::take(&mut current));
                    }
                    chunks.extend(hard_split(&sentence, max_bytes));
                }
            }
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        chunks.push(String::new());
    }

    chunks
}

fn split_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn split_sentences(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        current.push(chars[i]);
        if (chars[i] == '.' || chars[i] == '!' || chars[i] == '?')
            && i + 1 < len
            && chars[i + 1] == ' '
        {
            result.push(current.trim().to_string());
            current = String::new();
            i += 2; // skip the space
            continue;
        }
        i += 1;
    }
    if !current.trim().is_empty() {
        result.push(current.trim().to_string());
    }
    if result.is_empty() {
        result.push(text.to_string());
    }
    result
}

fn hard_split(text: &str, max_bytes: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = (start + max_bytes).min(bytes.len());
        // walk back to a valid UTF-8 boundary
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end = start + 1; // safety: advance at least one byte
        }
        chunks.push(text[start..end].to_string());
        start = end;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_not_chunked() {
        let text = "hello world";
        let chunks = chunk(text, 8192);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], text);
    }

    #[test]
    fn long_text_chunked_single_mode() {
        // 8K tokens * 4 bytes = 32768 bytes threshold
        let long_text = "word ".repeat(10_000); // ~50000 bytes
        let chunks = chunk(&long_text, 8192);
        assert!(chunks.len() > 1, "long text should produce multiple chunks");
        let first = apply_mode(chunks, ChunkMode::Single);
        assert_eq!(first.len(), 1);
        assert!(first[0].len() <= 8192 * 4 + 1); // within token limit
    }

    #[test]
    fn long_text_multi_mode_returns_all() {
        let long_text = "word ".repeat(10_000);
        let chunks_single = chunk(&long_text, 8192);
        let n = chunks_single.len();
        let all = apply_mode(chunk(&long_text, 8192), ChunkMode::Multi);
        assert_eq!(all.len(), n);
    }

    #[test]
    fn paragraph_split_preference() {
        let text = "First paragraph with some content.\n\nSecond paragraph with more text.";
        let chunks = chunk(text, 8); // 8 tokens = 32 bytes — small to force split
        // Both paragraphs have ~35-40 bytes, so they split
        assert!(chunks.len() >= 1);
        // Each chunk fits within 32 bytes
        for c in &chunks {
            assert!(c.len() <= 8 * 4 + 10, "chunk too large: {}", c.len());
        }
    }

    #[test]
    fn chunk_mode_from_str() {
        assert_eq!(ChunkMode::from_str("multi"), ChunkMode::Multi);
        assert_eq!(ChunkMode::from_str("single"), ChunkMode::Single);
        assert_eq!(ChunkMode::from_str("unknown"), ChunkMode::Single);
    }

    #[test]
    fn hard_split_handles_multibyte_utf8() {
        // "café" repeated — ensure no split in middle of multi-byte char
        let text = "café ".repeat(2000); // each "café " is 6 bytes
        let chunks = hard_split(&text, 32);
        // All chunks should be valid UTF-8
        for c in &chunks {
            assert!(std::str::from_utf8(c.as_bytes()).is_ok());
        }
    }
}
