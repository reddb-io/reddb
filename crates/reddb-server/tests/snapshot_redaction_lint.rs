//! Snapshot redaction lint (issue #98).
//!
//! Walks every committed `*.snap` file under
//! `crates/reddb-server/tests/snapshots/` and re-greps it with the
//! same patterns the [`secret_redactor`] module installs as insta
//! filters. A single unmasked secret-shaped substring fails CI with
//! a precise message naming the file, byte offset, pattern label,
//! and the offending substring.
//!
//! The lint is the safety net for the redactor: if a future test
//! author forgets the `let _guard = secret_redactor::install_
//! redactions();` line, the resulting `*.snap` file will contain
//! the raw secret and this test will fail before it merges.
//!
//! See `tests/support/parser_hardening/secret_redactor.rs` for the
//! pattern definitions.

mod support {
    pub mod parser_hardening;
}

use std::fs;
use std::path::{Path, PathBuf};

use support::parser_hardening::secret_redactor::{find_unmasked_secrets, UnmaskedHit};

/// Directory the lint walks. Resolved relative to `CARGO_MANIFEST_
/// DIR` so the test works regardless of where `cargo test` is
/// invoked from.
fn snapshots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
}

/// Recursively collect every `*.snap` file under `dir`. The
/// existing snapshot tree is flat, but a recursive walk future-
/// proofs the lint against per-suite subdirectories.
fn collect_snap_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => panic!(
            "snapshot_redaction_lint: failed to read {}: {}",
            dir.display(),
            err
        ),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_snap_files(&path));
        } else if path.extension().is_some_and(|ext| ext == "snap") {
            out.push(path);
        }
    }
    out
}

/// Render an unmasked-secret hit at a precise file + line for a
/// readable failure message.
fn format_violation(path: &Path, content: &str, hit: &UnmaskedHit) -> String {
    // Convert byte offset to (1-based line, 1-based column). Cheap
    // because snapshots are short.
    let prefix = &content[..hit.offset.min(content.len())];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = prefix.rsplit('\n').next().map(|s| s.len()).unwrap_or(0) + 1;
    format!(
        "  {}:{}:{} — pattern={} matched={:?}",
        path.display(),
        line,
        col,
        hit.pattern,
        hit.matched
    )
}

#[test]
fn no_snapshot_contains_unmasked_secret() {
    let dir = snapshots_dir();
    let files = collect_snap_files(&dir);
    assert!(
        !files.is_empty(),
        "snapshot_redaction_lint: no `*.snap` files found under {} — did the snapshots tree move?",
        dir.display()
    );

    let mut violations: Vec<String> = Vec::new();
    for path in &files {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(err) => panic!(
                "snapshot_redaction_lint: failed to read {}: {}",
                path.display(),
                err
            ),
        };
        for hit in find_unmasked_secrets(&content) {
            violations.push(format_violation(path, &content, &hit));
        }
    }

    if !violations.is_empty() {
        panic!(
            "snapshot_redaction_lint: {} unmasked secret-shaped substring(s) found in committed \
             `*.snap` files. Each hit is a candidate for issue #97 (snapshot backfill). Either:\n\
             \n\
             1. Add `let _guard = secret_redactor::install_redactions();` to the test that emits \
                the snapshot, then `cargo insta accept` to refresh.\n\
             2. If the match is a known false-positive, narrow the regex in `secret_redactor.rs` \
                rather than allowlisting the file.\n\
             \n\
             Violations:\n{}",
            violations.len(),
            violations.join("\n"),
        );
    }
}
