//! Issue #1046 / PRD #1041 (ADR 0051) — `red ui <uri>` deep-link dispatch.
//!
//! CLI-level coverage that the deep-link seam is wired through `main()`:
//! the handler-registered probe is overridden via `RED_UI_DEEPLINK_REGISTERED`
//! so both branches are deterministic without OS handler state.
//!
//!   1. `--desktop` with a registered handler → hands off, emitting the
//!      canonicalized `redui://?connect=<abs-file-uri>` deep link, carrying
//!      no credential, and never starting the bridge.
//!   2. `--desktop` with no handler registered → errors with install
//!      guidance (no silent browser fallback for the forced desktop path).
//!
//! The exhaustive decision matrix (Auto/Server/Desktop × registered) lives in
//! the `reddb_server::server::ui_deeplink` unit tests, which drive the seam
//! directly; these tests pin the `main()` wiring end-to-end.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn red_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

fn xdg_open_present() -> bool {
    Command::new("xdg-open")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| true)
        .unwrap_or(false)
}

#[test]
fn desktop_with_registered_handler_emits_canonical_deep_link() {
    if cfg!(not(target_os = "linux")) || !xdg_open_present() {
        // The handoff path spawns the OS opener; only assert it where a
        // spawnable `xdg-open` exists so the test stays deterministic.
        eprintln!("skipping: no spawnable xdg-open on this platform");
        return;
    }

    // Relative file:// → must be canonicalized to an absolute path, because
    // the OS handler runs with a different cwd (ADR 0051).
    let output = Command::new(red_binary())
        .args(["ui", "--desktop", "--json", "file://./data.rdb"])
        .env("RED_UI_DEEPLINK_REGISTERED", "1")
        .current_dir(std::env::temp_dir())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run red ui --desktop");

    assert!(
        output.status.success(),
        "expected clean handoff exit; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"dispatch\":\"desktop\""),
        "stdout: {stdout}"
    );
    // The deep link points at an absolute file:// URI (canonicalized).
    assert!(
        stdout.contains("redui://?connect=file:///"),
        "deep link not canonicalized to an absolute path: {stdout}"
    );
    assert!(stdout.contains("/data.rdb"), "stdout: {stdout}");
    // The deep link carries the target only — never a credential.
    assert!(
        !stdout.contains("token"),
        "deep link leaked a token: {stdout}"
    );
    assert!(
        !stdout.contains("password") && !stdout.contains("secret"),
        "deep link leaked a credential: {stdout}"
    );
}

#[test]
fn desktop_without_handler_errors_with_install_guidance() {
    let output = Command::new(red_binary())
        .args(["ui", "--desktop", "--json", "file://./data.rdb"])
        .env("RED_UI_DEEPLINK_REGISTERED", "0")
        .current_dir(std::env::temp_dir())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run red ui --desktop with no handler");

    assert!(
        !output.status.success(),
        "expected non-zero exit when the desktop app is not installed"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("redui:// handler") || combined.contains("not installed"),
        "expected install guidance; got: {combined}"
    );
}

#[test]
fn ui_help_documents_desktop_and_server_dispatch() {
    let output = Command::new(red_binary())
        .args(["ui", "--help"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run red ui --help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--desktop"),
        "help missing --desktop: {stdout}"
    );
    assert!(
        stdout.contains("--server"),
        "help missing --server: {stdout}"
    );
    assert!(
        stdout.contains("deep link") || stdout.contains("redui://"),
        "help should mention the deep link: {stdout}"
    );
}
