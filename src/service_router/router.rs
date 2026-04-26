//! Composable protocol-detection router.
//!
//! Holds an ordered list of `ProtocolDetector`s plus a fallback. Iterates the
//! list once per peek; first `Match` wins. If at least one detector returns
//! `Pending`, the router waits for more bytes (bounded by `probe_timeout`).
//! If all detectors return `NoMatch`, the fallback protocol is selected
//! immediately.

use std::time::{Duration, Instant};

use tokio::io;
use tokio::net::TcpStream;
use tokio::time::sleep;

use super::detector::{
    DetectOutcome, HTTP_2_PREFACE, HttpDetector, H2Detector, Protocol, ProtocolDetector,
};

const PROTOCOL_PROBE_TIMEOUT: Duration = Duration::from_millis(200);
const PROTOCOL_PROBE_RETRY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeStep {
    Match(Protocol),
    Pending,
    NoMatch,
}

pub(crate) struct Router {
    detectors: Vec<Box<dyn ProtocolDetector>>,
    fallback: Protocol,
    probe_timeout: Duration,
    probe_retry: Duration,
    peek_buf_size: usize,
}

impl Router {
    pub(crate) fn new(
        detectors: Vec<Box<dyn ProtocolDetector>>,
        fallback: Protocol,
    ) -> Self {
        Self {
            detectors,
            fallback,
            probe_timeout: PROTOCOL_PROBE_TIMEOUT,
            probe_retry: PROTOCOL_PROBE_RETRY,
            peek_buf_size: HTTP_2_PREFACE.len(),
        }
    }

    /// Default router: H2 (gRPC) first, then HTTP/1.x; Wire as fallback.
    pub(crate) fn default_tcp() -> Self {
        Self::new(
            vec![Box::new(H2Detector), Box::new(HttpDetector)],
            Protocol::Wire,
        )
    }

    pub(crate) fn classify(&self, peek: &[u8]) -> ProbeStep {
        let mut any_pending = false;
        for det in &self.detectors {
            match det.detect(peek) {
                DetectOutcome::Match(p) => return ProbeStep::Match(p),
                DetectOutcome::Pending => any_pending = true,
                DetectOutcome::NoMatch => {}
            }
        }
        if any_pending {
            ProbeStep::Pending
        } else {
            ProbeStep::NoMatch
        }
    }

    pub(crate) async fn detect(&self, stream: &TcpStream) -> io::Result<Protocol> {
        let started_at = Instant::now();
        let mut peek_buf = vec![0_u8; self.peek_buf_size];

        loop {
            let read = stream.peek(&mut peek_buf).await?;
            if read == 0 {
                return Ok(self.fallback);
            }
            let bytes = &peek_buf[..read];
            match self.classify(bytes) {
                ProbeStep::Match(p) => return Ok(p),
                ProbeStep::NoMatch => return Ok(self.fallback),
                ProbeStep::Pending => {
                    if read == peek_buf.len()
                        || started_at.elapsed() >= self.probe_timeout
                    {
                        return Ok(self.fallback);
                    }
                    sleep(self.probe_retry).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_router_classifies_h2_preface_as_grpc() {
        let router = Router::default_tcp();
        assert_eq!(
            router.classify(HTTP_2_PREFACE),
            ProbeStep::Match(Protocol::Grpc)
        );
    }

    #[test]
    fn default_router_classifies_http_methods() {
        let router = Router::default_tcp();
        assert_eq!(
            router.classify(b"POST /query HTTP/1.1\r\n"),
            ProbeStep::Match(Protocol::Http)
        );
        assert_eq!(
            router.classify(b"GET /health HTTP/1.1\r\n"),
            ProbeStep::Match(Protocol::Http)
        );
    }

    #[test]
    fn default_router_falls_back_to_wire_for_binary_frames() {
        let router = Router::default_tcp();
        assert_eq!(
            router.classify(&[0x10, 0x00, 0x00, 0x00, 0x01, b'S', b'E', b'L']),
            ProbeStep::NoMatch
        );
    }

    #[test]
    fn default_router_keeps_partial_h2_preface_pending() {
        let router = Router::default_tcp();
        assert_eq!(router.classify(b"PRI * HTTP/2.0\r\n"), ProbeStep::Pending);
    }

    #[test]
    fn default_router_keeps_partial_http_method_pending() {
        let router = Router::default_tcp();
        assert_eq!(router.classify(b"PO"), ProbeStep::Pending);
    }

    #[test]
    fn first_match_wins_when_multiple_detectors_could_match() {
        // A pathological detector that always claims HTTP. Place it first;
        // an H2 preface should still be classified as Http because the first
        // Match wins.
        struct AlwaysHttp;
        impl ProtocolDetector for AlwaysHttp {
            fn detect(&self, _: &[u8]) -> DetectOutcome {
                DetectOutcome::Match(Protocol::Http)
            }
        }
        let router = Router::new(
            vec![Box::new(AlwaysHttp), Box::new(H2Detector)],
            Protocol::Wire,
        );
        assert_eq!(
            router.classify(HTTP_2_PREFACE),
            ProbeStep::Match(Protocol::Http)
        );
    }
}
