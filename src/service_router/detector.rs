//! Pluggable protocol detection for the TCP router.
//!
//! Each detector inspects an inbound peek buffer and returns one of:
//! - `Match(Protocol)` — definite identification, router routes immediately.
//! - `Pending` — buffer is a prefix of this detector's signature; need more bytes.
//! - `NoMatch` — definitely not this protocol.
//!
//! The router composes detectors in a fixed order and the first `Match` wins.
//! Adding a new protocol means writing a new struct, not editing a centralized
//! `match` in the router.

pub(crate) const HTTP_2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub(crate) const HTTP_METHOD_PREFIXES: [&[u8]; 9] = [
    b"GET ",
    b"POST ",
    b"PUT ",
    b"PATCH ",
    b"DELETE ",
    b"HEAD ",
    b"OPTIONS ",
    b"TRACE ",
    b"CONNECT ",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Grpc,
    Http,
    Wire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectOutcome {
    Match(Protocol),
    Pending,
    NoMatch,
}

pub trait ProtocolDetector: Send + Sync {
    fn detect(&self, peek: &[u8]) -> DetectOutcome;
}

/// RedWire v2 detector — keys off the `0xFE` magic byte the v2
/// client sends as its first byte. v1 wire clients never set this
/// byte (their first byte is the low byte of a u32 length field,
/// which for any reasonable frame size is well below 0xFE), so
/// this never aliases the legacy listener.
pub struct RedWireDetector;

impl ProtocolDetector for RedWireDetector {
    fn detect(&self, peek: &[u8]) -> DetectOutcome {
        if peek.is_empty() {
            return DetectOutcome::Pending;
        }
        if peek[0] == 0xFE {
            DetectOutcome::Match(Protocol::Wire)
        } else {
            DetectOutcome::NoMatch
        }
    }
}

pub struct H2Detector;

impl ProtocolDetector for H2Detector {
    fn detect(&self, peek: &[u8]) -> DetectOutcome {
        if peek.starts_with(HTTP_2_PREFACE) {
            DetectOutcome::Match(Protocol::Grpc)
        } else if !peek.is_empty() && HTTP_2_PREFACE.starts_with(peek) {
            DetectOutcome::Pending
        } else {
            DetectOutcome::NoMatch
        }
    }
}

pub struct HttpDetector;

impl ProtocolDetector for HttpDetector {
    fn detect(&self, peek: &[u8]) -> DetectOutcome {
        if HTTP_METHOD_PREFIXES.iter().any(|p| peek.starts_with(p)) {
            DetectOutcome::Match(Protocol::Http)
        } else if !peek.is_empty()
            && HTTP_METHOD_PREFIXES.iter().any(|p| p.starts_with(peek))
        {
            DetectOutcome::Pending
        } else {
            DetectOutcome::NoMatch
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h2_detector_matches_full_preface() {
        assert_eq!(
            H2Detector.detect(HTTP_2_PREFACE),
            DetectOutcome::Match(Protocol::Grpc)
        );
    }

    #[test]
    fn h2_detector_pending_on_partial_preface() {
        assert_eq!(H2Detector.detect(b"PRI * HTTP/2.0\r\n"), DetectOutcome::Pending);
    }

    #[test]
    fn h2_detector_no_match_on_garbage() {
        assert_eq!(
            H2Detector.detect(&[0x10, 0x00, 0x00, 0x00, 0x01, b'S', b'E', b'L']),
            DetectOutcome::NoMatch
        );
    }

    #[test]
    fn http_detector_matches_methods() {
        assert_eq!(
            HttpDetector.detect(b"POST /query HTTP/1.1\r\n"),
            DetectOutcome::Match(Protocol::Http)
        );
        assert_eq!(
            HttpDetector.detect(b"GET /health HTTP/1.1\r\n"),
            DetectOutcome::Match(Protocol::Http)
        );
    }

    #[test]
    fn http_detector_pending_on_partial_method() {
        assert_eq!(HttpDetector.detect(b"PO"), DetectOutcome::Pending);
    }

    #[test]
    fn http_detector_no_match_on_binary_frame() {
        assert_eq!(
            HttpDetector.detect(&[0x10, 0x00, 0x00, 0x00, 0x01, b'S', b'E', b'L']),
            DetectOutcome::NoMatch
        );
    }
}
