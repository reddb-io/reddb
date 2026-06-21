//! Local computer-vision backend + image fetch (#1275, PRD #1267).
//!
//! Vision is the second async AI modality wired over the CDC enrichment
//! lane (the first being embeddings, #1272). A collection that declares a
//! `VISION (...)` policy names an *image-reference field* whose value is a
//! URL/URI. After commit, the CDC enrichment consumer fetches the
//! referenced image and runs the policy's vision provider, producing:
//!
//!   * a structured **component-detections** array
//!     (`[{label, confidence, bbox:[x,y,w,h]}]`) written to a derived
//!     field that RQL can filter, and
//!   * an optional **image-embedding** vector reusing the existing vector
//!     pipeline for image similarity search.
//!
//! Mirroring [`super::local_embedding`], the engine is a swappable,
//! process-global [`LocalVisionBackend`]. The default
//! [`DeterministicFakeVisionBackend`] derives stable detections from
//! `SHA-256(model || image-bytes)` so the end-to-end contract (fetch →
//! analyze → attach) can be exercised without a real model. Tests install
//! their own mock provider via [`install_local_vision_backend`]; a real
//! candle/onnx engine slots in the same way at server boot.

use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use crate::crypto::sha256::Sha256;
use crate::{RedDBError, RedDBResult};

/// One structured component detection: a labelled, scored bounding box.
/// `bbox` is `[x, y, w, h]` in image-relative units, matching the
/// canonical output shape recorded in the issue.
#[derive(Debug, Clone, PartialEq)]
pub struct VisionDetection {
    pub label: String,
    pub confidence: f32,
    pub bbox: [f32; 4],
}

/// Output of a single vision analysis pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VisionResult {
    /// Structured component detections (present when detections were
    /// requested by the policy).
    pub detections: Vec<VisionDetection>,
    /// Image embedding (present only when the policy requested an
    /// image-embedding output kind).
    pub embedding: Option<Vec<f32>>,
}

/// A materialised vision request handed to a backend.
#[derive(Debug, Clone)]
pub struct VisionRequest {
    /// Model name as written in the collection's VISION policy.
    pub model: String,
    /// Fetched image bytes (already resolved from the row's reference).
    pub image_bytes: Vec<u8>,
    /// Whether the policy asked for structured detections.
    pub want_detections: bool,
    /// Whether the policy asked for an image-embedding output.
    pub want_embedding: bool,
}

/// Backend abstraction so the enrichment lane does not depend on a
/// specific vision engine. Tests install a mock; production installs a
/// real engine via [`install_local_vision_backend`].
pub trait LocalVisionBackend: Send + Sync {
    fn analyze(&self, request: &VisionRequest) -> RedDBResult<VisionResult>;
}

const LOCAL_VISION_DISABLED_MESSAGE: &str =
    "local vision requires the `local-models` feature flag at engine build time, \
     or a backend installed via \
     runtime::ai::vision::install_local_vision_backend. Alternatively, declare a \
     vision-capable remote provider in the collection's VISION policy.";

/// Width (in f32 lanes) of the deterministic fake image embedding.
const FAKE_EMBEDDING_DIM: usize = 16;

/// Deterministic, dependency-free vision backend used to prove the
/// fetch → analyze → attach contract end-to-end. Output is a pure
/// function of `(model, image-bytes)` — no I/O, no clocks, no RNGs — so
/// tests get byte-identical results across runs.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeterministicFakeVisionBackend;

impl LocalVisionBackend for DeterministicFakeVisionBackend {
    fn analyze(&self, request: &VisionRequest) -> RedDBResult<VisionResult> {
        let digest = {
            let mut hasher = Sha256::new();
            hasher.update(request.model.as_bytes());
            hasher.update(&[0u8]);
            hasher.update(&request.image_bytes);
            hasher.finalize()
        };

        let detections = if request.want_detections {
            // One detection per non-overlapping label, chosen
            // deterministically from a small fixed vocabulary by the
            // digest. Two distinct, stable detections so containment
            // filters have something to match.
            const VOCAB: [&str; 4] = ["person", "car", "dog", "bicycle"];
            let pick = |byte: u8| VOCAB[(byte as usize) % VOCAB.len()].to_string();
            let conf = |byte: u8| (byte as f32) / 255.0;
            let coord = |byte: u8| (byte as f32) / 255.0;
            vec![
                VisionDetection {
                    label: pick(digest[0]),
                    confidence: conf(digest[1]),
                    bbox: [
                        coord(digest[2]),
                        coord(digest[3]),
                        coord(digest[4]),
                        coord(digest[5]),
                    ],
                },
                VisionDetection {
                    label: pick(digest[6]),
                    confidence: conf(digest[7]),
                    bbox: [
                        coord(digest[8]),
                        coord(digest[9]),
                        coord(digest[10]),
                        coord(digest[11]),
                    ],
                },
            ]
        } else {
            Vec::new()
        };

        let embedding = if request.want_embedding {
            let mut out = Vec::with_capacity(FAKE_EMBEDDING_DIM);
            let mut counter: u32 = 0;
            while out.len() < FAKE_EMBEDDING_DIM {
                let mut hasher = Sha256::new();
                hasher.update(&digest);
                hasher.update(&counter.to_le_bytes());
                let chunk_digest = hasher.finalize();
                for chunk in chunk_digest.chunks(4) {
                    if out.len() >= FAKE_EMBEDDING_DIM {
                        break;
                    }
                    let mut bytes = [0u8; 4];
                    bytes.copy_from_slice(chunk);
                    let raw = u32::from_le_bytes(bytes) as f32 / u32::MAX as f32;
                    out.push(raw * 2.0 - 1.0);
                }
                counter = counter.wrapping_add(1);
            }
            Some(out)
        } else {
            None
        };

        Ok(VisionResult {
            detections,
            embedding,
        })
    }
}

type BackendSlot = Arc<dyn LocalVisionBackend>;

fn backend_slot() -> &'static RwLock<Option<BackendSlot>> {
    static SLOT: OnceLock<RwLock<Option<BackendSlot>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install (or replace) the process-global local vision backend.
///
/// Production servers built with `--features local-models` call this once
/// at boot with their real engine. Tests use it to swap in a mock vision
/// provider. Safe to call from any thread; the most recent install wins.
pub fn install_local_vision_backend(backend: Arc<dyn LocalVisionBackend>) {
    let mut guard = backend_slot()
        .write()
        .expect("vision backend slot poisoned");
    *guard = Some(backend);
}

/// Test-only: clear the installed backend so a subsequent call exercises
/// the feature-disabled path again.
#[doc(hidden)]
pub fn clear_local_vision_backend_for_tests() {
    let mut guard = backend_slot()
        .write()
        .expect("vision backend slot poisoned");
    *guard = None;
}

fn current_backend() -> Option<BackendSlot> {
    backend_slot()
        .read()
        .expect("vision backend slot poisoned")
        .as_ref()
        .map(Arc::clone)
}

/// Resolve and run a local vision request end-to-end. Falls back to the
/// deterministic fake when the `local-models` feature is on but no engine
/// was installed; errors with a clear message when neither is available.
pub fn analyze_local(
    model: &str,
    image_bytes: Vec<u8>,
    want_detections: bool,
    want_embedding: bool,
) -> RedDBResult<VisionResult> {
    let backend = match current_backend() {
        Some(b) => b,
        None => {
            if cfg!(feature = "local-models") {
                let fake: Arc<dyn LocalVisionBackend> = Arc::new(DeterministicFakeVisionBackend);
                install_local_vision_backend(Arc::clone(&fake));
                fake
            } else {
                return Err(RedDBError::FeatureNotEnabled(
                    LOCAL_VISION_DISABLED_MESSAGE.to_string(),
                ));
            }
        }
    };

    backend.analyze(&VisionRequest {
        model: model.to_string(),
        image_bytes,
        want_detections,
        want_embedding,
    })
}

/// Fetch the bytes of an image referenced by a row field.
///
/// The reference is a URL/URI (ADR 0057 stores the reference, never the
/// bytes). Supported schemes:
///   * `file://<path>` and bare filesystem paths — read from disk;
///   * `http://` / `https://` — fetched via `ureq` (rustls).
///
/// Network/IO failures surface as [`RedDBError`] so the enrichment lane's
/// retry-with-backoff and dead-letter machinery handles them like any
/// other provider failure.
pub fn fetch_image_bytes(reference: &str) -> RedDBResult<Vec<u8>> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(RedDBError::Query(
            "vision image reference is empty".to_string(),
        ));
    }

    if let Some(rest) = reference.strip_prefix("file://") {
        // Tolerate the `file://localhost/path` authority form.
        let path = rest.strip_prefix("localhost").unwrap_or(rest);
        return std::fs::read(path)
            .map_err(|err| RedDBError::Internal(format!("read image '{path}': {err}")));
    }

    if reference.starts_with("http://") || reference.starts_with("https://") {
        return fetch_http_image(reference);
    }

    // Bare filesystem path.
    std::fs::read(reference)
        .map_err(|err| RedDBError::Internal(format!("read image '{reference}': {err}")))
}

fn fetch_http_image(url: &str) -> RedDBResult<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_send_request(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(120)))
        .build()
        .into();

    let mut resp = agent
        .get(url)
        .call()
        .map_err(|err| RedDBError::Internal(format!("HTTP GET image '{url}': {err}")))?;

    let status = resp.status().as_u16();
    if status != 200 {
        return Err(RedDBError::Internal(format!(
            "HTTP GET image '{url}': status {status}"
        )));
    }

    resp.body_mut()
        .read_to_vec()
        .map_err(|err| RedDBError::Internal(format!("read image body from '{url}': {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_fake_is_pure() {
        let req = VisionRequest {
            model: "fake-vision".to_string(),
            image_bytes: b"some image bytes".to_vec(),
            want_detections: true,
            want_embedding: true,
        };
        let a = DeterministicFakeVisionBackend.analyze(&req).expect("a");
        let b = DeterministicFakeVisionBackend.analyze(&req).expect("b");
        assert_eq!(a, b, "fake vision backend must be pure");
        assert_eq!(a.detections.len(), 2);
        assert_eq!(a.embedding.as_ref().map(Vec::len), Some(FAKE_EMBEDDING_DIM));
    }

    #[test]
    fn detections_and_embedding_are_gated_by_request() {
        let base = VisionRequest {
            model: "m".to_string(),
            image_bytes: b"img".to_vec(),
            want_detections: false,
            want_embedding: false,
        };
        let none = DeterministicFakeVisionBackend.analyze(&base).expect("none");
        assert!(none.detections.is_empty());
        assert!(none.embedding.is_none());

        let detect_only = DeterministicFakeVisionBackend
            .analyze(&VisionRequest {
                want_detections: true,
                ..base.clone()
            })
            .expect("detect");
        assert!(!detect_only.detections.is_empty());
        assert!(detect_only.embedding.is_none());
    }

    #[test]
    fn fetch_reads_file_uri_and_bare_path() {
        let dir = std::env::temp_dir();
        let path = dir.join("reddb_vision_fetch_fixture.bin");
        std::fs::write(&path, b"\x89PNG fixture").expect("write fixture");

        let via_bare = fetch_image_bytes(path.to_str().expect("utf8 path")).expect("bare");
        assert_eq!(via_bare, b"\x89PNG fixture");

        let uri = format!("file://{}", path.to_str().expect("utf8 path"));
        let via_uri = fetch_image_bytes(&uri).expect("file uri");
        assert_eq!(via_uri, b"\x89PNG fixture");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fetch_rejects_empty_reference() {
        assert!(fetch_image_bytes("   ").is_err());
    }
}
