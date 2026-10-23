//! Lehman-Yao concurrent B-tree support — runtime switch + helpers.
//!
//! The reddb B-tree already carries `prev_leaf` / `next_leaf`
//! pointers on every leaf page, which covers the "right-link" half
//! of the Lehman-Yao contract (Lehman & Yao, ACM TODS 1981). The
//! missing pieces are:
//!
//! 1. A `high_key` separator on each leaf so readers can detect
//!    that a concurrent split moved their target key to the right
//!    sibling without restarting from the root.
//! 2. A reader descent path that pins pages rather than locking,
//!    following `next_leaf` when `key > high_key`.
//! 3. Split routines that hold the page-exclusive lock locally and
//!    release it the moment the right-link is wired up (so other
//!    readers can immediately follow through).
//!
//! Items (1)–(3) require an on-disk format bump (`STORE_VERSION_V8`)
//! plus a migration that fills `high_key` on existing leaves from
//! their current highest key, followed by lock protocol changes
//! across `nbtree`-analogous modules.
//!
//! This module is the coordination surface for that work:
//!
//! - `is_enabled()` reports the effective runtime value of
//!   `storage.btree.lehman_yao` so read / split callers can branch
//!   on the future path without sprinkling `config_bool` everywhere
//!   on the hot path.
//! - `set_enabled(bool)` is called by the runtime at boot after it
//!   has resolved the matrix value (env > file > red_config >
//!   default). Defaults to `true` so a library-only user gets
//!   Lehman-Yao semantics once the storage changes land.
//! - `HighKey` is the on-disk shape for the leaf-page upper bound.
//!   Today it's only written by callers that opt in; the full
//!   wire-up lands with `STORE_VERSION_V8`.

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide runtime flag. Atomics so the read / split paths
/// can branch without taking a lock on every probe. `true` on
/// start — callers that want legacy semantics flip it during boot.
static LEHMAN_YAO_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn is_enabled() -> bool {
    LEHMAN_YAO_ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    LEHMAN_YAO_ENABLED.store(on, Ordering::Relaxed);
}

/// On-disk shape for a leaf page's upper bound. Contiguous
/// `len` + `bytes` layout — matches the cell encoding the
/// rest of the leaf uses so the migration can copy the last
/// live cell's key verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HighKey {
    /// Bytes of the largest key currently in the page. Empty
    /// means "unbounded" (rightmost leaf of its level).
    pub bytes: Vec<u8>,
}

impl HighKey {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

    /// Rightmost-leaf marker: a reader descending past every
    /// `high_key` comparison has nowhere further to go.
    pub fn is_unbounded(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Lehman-Yao reader probe. Returns `true` when `key` is
    /// strictly greater than `high_key` — the reader should follow
    /// `next_leaf` rather than search this page. Unbounded
    /// high-key means "always reside here".
    pub fn should_follow_right_link(&self, key: &[u8]) -> bool {
        if self.bytes.is_empty() {
            return false;
        }
        key > self.bytes.as_slice()
    }
}
