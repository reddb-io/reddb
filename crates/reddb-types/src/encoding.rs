const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Encode bytes with the padded RFC 4648 standard base64 alphabet.
pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for chunk in chunks.by_ref() {
        let bits = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
        out.push(BASE64_ALPHABET[((bits >> 18) & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[((bits >> 12) & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[((bits >> 6) & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[(bits & 0x3f) as usize] as char);
    }
    match chunks.remainder() {
        [] => {}
        [first] => {
            let bits = (*first as u32) << 16;
            out.push(BASE64_ALPHABET[((bits >> 18) & 0x3f) as usize] as char);
            out.push(BASE64_ALPHABET[((bits >> 12) & 0x3f) as usize] as char);
            out.push_str("==");
        }
        [first, second] => {
            let bits = ((*first as u32) << 16) | ((*second as u32) << 8);
            out.push(BASE64_ALPHABET[((bits >> 18) & 0x3f) as usize] as char);
            out.push(BASE64_ALPHABET[((bits >> 12) & 0x3f) as usize] as char);
            out.push(BASE64_ALPHABET[((bits >> 6) & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => unreachable!(),
    }
    out
}

/// Escape string content for insertion between JSON double quotes.
pub fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for character in input.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                let byte = control as usize;
                out.push_str("\\u00");
                out.push(HEX_DIGITS[byte >> 4] as char);
                out.push(HEX_DIGITS[byte & 0x0f] as char);
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_standard_vectors() {
        let vectors: &[(&[u8], &str)] = &[
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
            (&[0xfb, 0xff, 0xbf], "+/+/"),
        ];

        for (input, expected) in vectors {
            assert_eq!(base64_encode(input), *expected);
        }
    }

    #[test]
    fn json_escape_vectors() {
        let vectors = [
            ("", ""),
            ("plain / text", "plain / text"),
            ("\"\\\n\r\t", "\\\"\\\\\\n\\r\\t"),
            ("\0\u{0008}\u{000c}\u{001f}", "\\u0000\\u0008\\u000c\\u001f"),
            ("olá 🧪", "olá 🧪"),
        ];

        for (input, expected) in vectors {
            assert_eq!(json_escape(input), expected);
        }
    }
}
