//! Deep-link dispatch for `red ui <uri>` (ADR 0051, issue #1046, PRD #1041).
//!
//! The default `red ui <uri>` (no `--server`) prefers the installed desktop
//! app via the `redui://` URL scheme and falls back to the served browser
//! bridge ([`super::ui_bridge`]) when no handler is registered. This module
//! owns the *decision*, the canonicalised deep-link *string*, and the OS
//! handoff — all behind a [`DeepLinkEnv`] seam so both branches (handoff vs
//! fallback) and the deep-link URL are unit-testable without touching the OS.
//!
//! The dispatched `redui://?connect=<canonical-uri>` URL carries the target
//! only — never a credential. Auth handoff is a separate slice (ADR 0051: the
//! token crosses via a local secret channel, never the deep-link URL).

use std::path::{Component, Path, PathBuf};

/// Which dispatch path `red ui` should take, before consulting the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    /// Default: prefer the desktop app when its `redui://` handler is
    /// registered, else fall back to the served browser bridge.
    Auto,
    /// `--server`: force the browser-served bridge path.
    Server,
    /// `--desktop`: force the desktop download/install path.
    Desktop,
}

impl DispatchMode {
    /// Resolve the mode from the `--server` / `--desktop` flags. `--server`
    /// wins if both are somehow set — it is the path that always works.
    pub fn from_flags(server: bool, desktop: bool) -> Self {
        if server {
            DispatchMode::Server
        } else if desktop {
            DispatchMode::Desktop
        } else {
            DispatchMode::Auto
        }
    }
}

/// What the caller (`red ui`) should do once dispatch has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Handed off to the desktop app — [`DeepLinkEnv::open_url`] was already
    /// invoked with `deep_link`. The caller prints the handoff line and exits
    /// cleanly; it must *not* start the bridge.
    HandedOff {
        /// The exact `redui://?connect=…` URL that was opened.
        deep_link: String,
    },
    /// Serve the pinned bundle and open the browser (the [`super::ui_bridge`]
    /// path). When `upsell` is set, the caller first prints the one-line nudge
    /// to install the desktop app for a faster native shell.
    ServeBrowser {
        /// True only when we fell back from [`DispatchMode::Auto`] because no
        /// handler was registered — print the install upsell.
        upsell: bool,
    },
    /// `--desktop` was requested but no `redui://` handler is registered, so
    /// there was nothing to hand off to. The caller prints install guidance.
    /// (The download/install itself lives in the `red-ui` repo — ADR 0051.)
    DesktopNotInstalled,
}

/// The OS-touching seam dispatch runs over: probing whether the `redui://`
/// handler is registered, and opening a URL with the platform handler. Both
/// branches of [`dispatch`] are driven through this trait so tests assert the
/// decision and the emitted deep-link string without any OS state.
pub trait DeepLinkEnv {
    /// Whether a handler for the `redui://` URL scheme is registered (i.e.
    /// the desktop app is installed).
    fn handler_registered(&self) -> bool;
    /// Open `url` with the platform default handler (the `xdg-open` /
    /// `open` / `start` equivalent). Returns the spawn error on failure.
    fn open_url(&self, url: &str) -> Result<(), String>;
}

/// Decide-and-dispatch. Pure decision logic over the [`DeepLinkEnv`] seam:
///
/// - [`DispatchMode::Server`] → [`DispatchOutcome::ServeBrowser`] (no upsell);
///   the handler is never probed and no URL is opened.
/// - [`DispatchMode::Auto`] → if the handler is registered, build the deep
///   link, open it, and return [`DispatchOutcome::HandedOff`]; otherwise
///   [`DispatchOutcome::ServeBrowser`] with the upsell.
/// - [`DispatchMode::Desktop`] → if the handler is registered, hand off the
///   same way; otherwise [`DispatchOutcome::DesktopNotInstalled`].
///
/// `canonical_uri` must already be canonicalised (see
/// [`canonicalize_target_uri`]); it is embedded verbatim (percent-encoded) in
/// the deep link and never carries a credential.
pub fn dispatch(
    mode: DispatchMode,
    canonical_uri: &str,
    env: &dyn DeepLinkEnv,
) -> Result<DispatchOutcome, String> {
    match mode {
        DispatchMode::Server => Ok(DispatchOutcome::ServeBrowser { upsell: false }),
        DispatchMode::Auto => {
            if env.handler_registered() {
                let deep_link = build_deep_link(canonical_uri);
                env.open_url(&deep_link)?;
                Ok(DispatchOutcome::HandedOff { deep_link })
            } else {
                Ok(DispatchOutcome::ServeBrowser { upsell: true })
            }
        }
        DispatchMode::Desktop => {
            if env.handler_registered() {
                let deep_link = build_deep_link(canonical_uri);
                env.open_url(&deep_link)?;
                Ok(DispatchOutcome::HandedOff { deep_link })
            } else {
                Ok(DispatchOutcome::DesktopNotInstalled)
            }
        }
    }
}

/// Build the `redui://?connect=<canonical-uri>` deep link (ADR 0051). The
/// canonical URI is percent-encoded so query-breaking characters (spaces,
/// `&`, `?`, `#`, …) survive, while the readable URI shape (`file:///…`,
/// `red://…`) is preserved. The link carries the target only — never a token.
pub fn build_deep_link(canonical_uri: &str) -> String {
    format!("redui://?connect={}", percent_encode_connect(canonical_uri))
}

/// Build a deep link that *also* tells the desktop app where to fetch the
/// held credential from (issue #1048). The `handoff` query value is the
/// **loopback handoff URL** — `http://127.0.0.1:<port>/handoff/<nonce>` — which
/// carries the single-use nonce, never the secret. The desktop app fetches the
/// token from there over the local secret channel; nothing sensitive rides the
/// deep link, so the token never lands in `ps`, shell history, or URL logs.
pub fn build_deep_link_with_handoff(canonical_uri: &str, handoff_url: &str) -> String {
    format!(
        "redui://?connect={}&handoff={}",
        percent_encode_connect(canonical_uri),
        percent_encode_connect(handoff_url),
    )
}

/// Percent-encode a target URI for the `connect=` query value. Keeps the
/// characters that are already query-safe and shape-significant for our
/// supported schemes (`A-Za-z0-9` and `-._~:/`), and encodes everything else —
/// including `%` itself — as upper-case `%XX`.
fn percent_encode_connect(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        let keep =
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':' | b'/');
        if keep {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(hex_upper(byte >> 4));
            out.push(hex_upper(byte & 0x0f));
        }
    }
    out
}

fn hex_upper(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

/// Canonicalise a `red ui` target URI for embedding in the deep link.
///
/// A `file://` / bare-path target is resolved to an absolute, lexically
/// normalised `file://` URI — the OS handler runs with a different cwd, so a
/// relative path would break. Any other supported scheme (`red://`, `reds://`,
/// `red+ws://`, `red+wss://`) is already location-independent and passes
/// through unchanged. `cwd` is injected so the file branch is testable without
/// depending on the process working directory.
pub fn canonicalize_target_uri(uri: &str, cwd: &Path) -> Result<String, String> {
    match super::ui_bridge::classify_ui_target(uri)? {
        super::ui_bridge::UiTarget::File => canonicalize_file_uri(uri, cwd),
        _ => Ok(uri.to_string()),
    }
}

/// Resolve a `file://` / bare-path target to an absolute `file://` URI using
/// `cwd` as the base for relative paths. Folds `.`/`..` lexically so the
/// result never depends on the target existing on disk.
fn canonicalize_file_uri(input: &str, cwd: &Path) -> Result<String, String> {
    let path_part = input.strip_prefix("file://").unwrap_or(input);
    if path_part.is_empty() {
        return Err("file:// URI has no path".to_string());
    }

    let raw = Path::new(path_part);
    let absolute = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    };

    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    let rendered = normalized
        .to_str()
        .ok_or_else(|| "resolved path is not valid UTF-8".to_string())?;
    Ok(format!("file://{rendered}"))
}

/// The production [`DeepLinkEnv`]: probes the OS for the `redui://` handler and
/// opens URLs via the platform default handler.
///
/// The probe can be overridden with `RED_UI_DEEPLINK_REGISTERED` (truthy =>
/// registered, `0`/`false`/`no` => not registered) — useful for forcing a
/// branch in manual testing or on platforms without a cheap probe. When unset,
/// it falls back to a best-effort per-OS check.
pub struct OsDeepLinkEnv;

impl DeepLinkEnv for OsDeepLinkEnv {
    fn handler_registered(&self) -> bool {
        if let Some(forced) = env_override("RED_UI_DEEPLINK_REGISTERED") {
            return forced;
        }
        os_handler_registered()
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        open_url_with_os_handler(url)
    }
}

/// Parse a truthy/falsy override env var. Returns `None` when unset/empty so
/// the caller falls back to the real probe.
fn env_override(key: &str) -> Option<bool> {
    match std::env::var(key) {
        Ok(value) => {
            let v = value.trim().to_ascii_lowercase();
            if v.is_empty() {
                None
            } else {
                Some(!matches!(v.as_str(), "0" | "false" | "no" | "off"))
            }
        }
        Err(_) => None,
    }
}

/// Best-effort, per-OS probe for a registered `redui://` scheme handler.
#[cfg(target_os = "linux")]
fn os_handler_registered() -> bool {
    // `xdg-mime query default x-scheme-handler/redui` prints the handler's
    // .desktop file name (and exits 0) when one is registered, nothing
    // otherwise.
    std::process::Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/redui"])
        .output()
        .map(|out| out.status.success() && !out.stdout.is_empty())
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn os_handler_registered() -> bool {
    // A registered URL-scheme handler lives under HKCU/HKCR\Software\Classes\redui.
    let user = std::process::Command::new("reg")
        .args(["query", "HKCU\\Software\\Classes\\redui", "/ve"])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    user || std::process::Command::new("reg")
        .args(["query", "HKCR\\redui", "/ve"])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn os_handler_registered() -> bool {
    // macOS (and others) have no cheap CLI probe for a Launch Services URL
    // handler; default to "not registered" so first contact still works via
    // the browser fallback. Force the desktop path with `--desktop` or the
    // `RED_UI_DEEPLINK_REGISTERED` override.
    false
}

/// Open `url` with the platform default handler (`xdg-open` / `open` /
/// `start`). Best-effort: a spawn failure is returned so the caller can react.
fn open_url_with_os_handler(url: &str) -> Result<(), String> {
    let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };
    std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Test seam: a scripted [`DeepLinkEnv`] that records every `open_url`
    /// call so both branches can be asserted without the OS.
    struct FakeEnv {
        registered: bool,
        opened: RefCell<Vec<String>>,
    }

    impl FakeEnv {
        fn new(registered: bool) -> Self {
            Self {
                registered,
                opened: RefCell::new(Vec::new()),
            }
        }
    }

    impl DeepLinkEnv for FakeEnv {
        fn handler_registered(&self) -> bool {
            self.registered
        }
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.opened.borrow_mut().push(url.to_string());
            Ok(())
        }
    }

    #[test]
    fn mode_from_flags_resolves_precedence() {
        assert_eq!(DispatchMode::from_flags(false, false), DispatchMode::Auto);
        assert_eq!(DispatchMode::from_flags(true, false), DispatchMode::Server);
        assert_eq!(DispatchMode::from_flags(false, true), DispatchMode::Desktop);
        // --server wins over --desktop: the always-works path.
        assert_eq!(DispatchMode::from_flags(true, true), DispatchMode::Server);
    }

    #[test]
    fn build_deep_link_keeps_file_uri_shape() {
        assert_eq!(
            build_deep_link("file:///home/user/data.rdb"),
            "redui://?connect=file:///home/user/data.rdb"
        );
    }

    #[test]
    fn build_deep_link_encodes_query_breaking_chars() {
        // Spaces, `&`, `?`, `#` would corrupt the query — must be encoded.
        assert_eq!(
            build_deep_link("file:///tmp/my db?x&y#z.rdb"),
            "redui://?connect=file:///tmp/my%20db%3Fx%26y%23z.rdb"
        );
    }

    #[test]
    fn build_deep_link_passes_remote_scheme() {
        assert_eq!(
            build_deep_link("reds://db.internal:5050"),
            "redui://?connect=reds://db.internal:5050"
        );
    }

    #[test]
    fn canonicalize_resolves_relative_file_uri_against_cwd() {
        let cwd = Path::new("/work/project");
        assert_eq!(
            canonicalize_target_uri("file://./data.rdb", cwd).unwrap(),
            "file:///work/project/data.rdb"
        );
        // `..` folds lexically; no filesystem touch.
        assert_eq!(
            canonicalize_target_uri("file://../sib/data.rdb", cwd).unwrap(),
            "file:///work/sib/data.rdb"
        );
    }

    #[test]
    fn canonicalize_keeps_absolute_file_uri() {
        let cwd = Path::new("/elsewhere");
        assert_eq!(
            canonicalize_target_uri("file:///abs/data.rdb", cwd).unwrap(),
            "file:///abs/data.rdb"
        );
    }

    #[test]
    fn canonicalize_passes_remote_targets_through() {
        let cwd = Path::new("/work");
        assert_eq!(
            canonicalize_target_uri("red://db.internal:6000", cwd).unwrap(),
            "red://db.internal:6000"
        );
        assert_eq!(
            canonicalize_target_uri("red+wss://edge.example/redwire", cwd).unwrap(),
            "red+wss://edge.example/redwire"
        );
    }

    // ----------------------------------------------------------------
    // The two dispatch branches over the seam (acceptance criteria).
    // ----------------------------------------------------------------

    #[test]
    fn auto_with_handler_hands_off_with_canonical_deep_link() {
        let env = FakeEnv::new(true);
        let canonical = canonicalize_target_uri("file://./data.rdb", Path::new("/work")).unwrap();
        let outcome = dispatch(DispatchMode::Auto, &canonical, &env).unwrap();
        assert_eq!(
            outcome,
            DispatchOutcome::HandedOff {
                deep_link: "redui://?connect=file:///work/data.rdb".to_string(),
            }
        );
        // The seam's open_url was driven with exactly that deep link.
        assert_eq!(
            *env.opened.borrow(),
            vec!["redui://?connect=file:///work/data.rdb".to_string()]
        );
    }

    #[test]
    fn auto_without_handler_falls_back_with_upsell_and_opens_nothing() {
        let env = FakeEnv::new(false);
        let outcome = dispatch(DispatchMode::Auto, "file:///work/data.rdb", &env).unwrap();
        assert_eq!(outcome, DispatchOutcome::ServeBrowser { upsell: true });
        assert!(env.opened.borrow().is_empty());
    }

    #[test]
    fn server_mode_forces_browser_without_probing_or_opening() {
        let env = FakeEnv::new(true); // handler present, but --server overrides
        let outcome = dispatch(DispatchMode::Server, "file:///work/data.rdb", &env).unwrap();
        assert_eq!(outcome, DispatchOutcome::ServeBrowser { upsell: false });
        assert!(env.opened.borrow().is_empty());
    }

    #[test]
    fn desktop_mode_with_handler_hands_off() {
        let env = FakeEnv::new(true);
        let outcome = dispatch(DispatchMode::Desktop, "file:///work/data.rdb", &env).unwrap();
        assert_eq!(
            outcome,
            DispatchOutcome::HandedOff {
                deep_link: "redui://?connect=file:///work/data.rdb".to_string(),
            }
        );
        assert_eq!(env.opened.borrow().len(), 1);
    }

    #[test]
    fn desktop_mode_without_handler_reports_not_installed() {
        let env = FakeEnv::new(false);
        let outcome = dispatch(DispatchMode::Desktop, "file:///work/data.rdb", &env).unwrap();
        assert_eq!(outcome, DispatchOutcome::DesktopNotInstalled);
        assert!(env.opened.borrow().is_empty());
    }

    #[test]
    fn handoff_deep_link_carries_the_nonce_url_not_the_secret() {
        // The handoff URL holds the single-use nonce; the secret token is
        // never an argument to the builder, so it cannot appear in the link.
        let handoff = "http://127.0.0.1:54321/handoff/0123456789abcdef0123456789abcdef";
        let link = build_deep_link_with_handoff("red://db.internal:5050", handoff);
        assert_eq!(
            link,
            "redui://?connect=red://db.internal:5050\
             &handoff=http://127.0.0.1:54321/handoff/0123456789abcdef0123456789abcdef"
        );
        // No credential material rides the link.
        assert!(!link.contains("token"));
        assert!(!link.contains("Bearer"));
        assert!(link.contains("/handoff/"));
    }

    #[test]
    fn deep_link_never_carries_a_credential() {
        // Even if a token-looking string is appended to the path, the deep
        // link only ever contains the connect target — dispatch never adds
        // auth, and the builder has no token parameter at all.
        let env = FakeEnv::new(true);
        let outcome = dispatch(DispatchMode::Auto, "red://db.internal:5050", &env).unwrap();
        if let DispatchOutcome::HandedOff { deep_link } = outcome {
            assert!(!deep_link.contains("token"));
            assert!(!deep_link.contains("password"));
            assert!(!deep_link.contains("secret"));
            assert!(!deep_link.contains("auth"));
            assert_eq!(deep_link, "redui://?connect=red://db.internal:5050");
        } else {
            panic!("expected handoff");
        }
    }
}
