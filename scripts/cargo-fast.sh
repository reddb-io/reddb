#!/usr/bin/env bash
set -euo pipefail

CARGO_BIN="${REDDB_CARGO_BIN:-cargo}"
RUSTC_BIN="${RUSTC:-rustc}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# A host-wide RUSTUP_TOOLCHAIN (e.g. mise's latest) must not silently
# override this project's pin. Explicit cargo +toolchain still wins.
export RUSTUP_TOOLCHAIN="${REDDB_RUST_TOOLCHAIN:-$(awk -F '"' '/^channel[[:space:]]*=/{print $2; exit}' "$ROOT/rust-toolchain.toml")}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-2}"
export NEXTEST_TEST_THREADS="${NEXTEST_TEST_THREADS:-2}"

if [ "$#" -eq 0 ]; then
  set -- build
fi

note() {
  printf '[cargo-fast] %s\n' "$*" >&2
}

append_rustflag() {
  local flag="$1"
  if [ -z "${RUSTFLAGS:-}" ]; then
    export RUSTFLAGS="$flag"
  else
    case " ${RUSTFLAGS} " in
      *" ${flag} "*) ;;
      *) export RUSTFLAGS="${RUSTFLAGS} ${flag}" ;;
    esac
  fi
}

USE_SCCACHE="${REDB_USE_SCCACHE:-auto}"
if command -v sccache >/dev/null 2>&1; then
  case "${USE_SCCACHE}" in
    1|true|yes|force)
      export CARGO_INCREMENTAL=0
      export RUSTC_WRAPPER="sccache"
      note "using sccache with incremental disabled"
      ;;
    0|false|no)
      ;;
    *)
      if [ "${CARGO_INCREMENTAL:-}" = "0" ]; then
        export RUSTC_WRAPPER="sccache"
        note "using sccache"
      else
        note "sccache skipped; set CARGO_INCREMENTAL=0 to enable it"
      fi
      ;;
  esac
fi

HOST_TRIPLE="$("$RUSTC_BIN" -vV | sed -n 's/^host: //p')"
LINKER_CHOICE=""
if command -v mold >/dev/null 2>&1 && command -v clang >/dev/null 2>&1; then
  LINKER_CHOICE="mold"
elif command -v ld.lld >/dev/null 2>&1 && command -v clang >/dev/null 2>&1; then
  LINKER_CHOICE="lld"
fi

if [ -n "${LINKER_CHOICE}" ]; then
  case "${HOST_TRIPLE}" in
    x86_64-unknown-linux-gnu)
      export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="clang"
      ;;
    aarch64-unknown-linux-gnu)
      export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="clang"
      ;;
  esac
  append_rustflag "-C link-arg=-fuse-ld=${LINKER_CHOICE}"
  note "using ${LINKER_CHOICE} via clang"
fi

# Keep independent worktree targets from multiplying the host's build load.
# Hold the lease through test execution too, while its binaries are in use.
# Long-lived cargo run servers must not hold up future builds.
if [ "${REDDB_CARGO_LOCK:-1}" != "0" ] && command -v flock >/dev/null 2>&1; then
  for arg in "$@"; do
    case "$arg" in
      run|fmt|metadata|help|--)
        break
        ;;
      build|check|clippy|test|rustc|bench|nextest|install|clean|package|publish)
        LOCK_DIR="${XDG_RUNTIME_DIR:-${HOME}/.cache/reddb}"
        mkdir -p "$LOCK_DIR"
        note "using host build/test lease (REDDB_CARGO_LOCK=0 to opt out)"
        # flock owns the descriptor; Cargo and its children do not inherit it.
        exec flock --close "$LOCK_DIR/reddb-cargo-${UID}.lock" "$CARGO_BIN" "$@"
        ;;
    esac
  done
fi

exec "$CARGO_BIN" "$@"
