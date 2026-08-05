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
