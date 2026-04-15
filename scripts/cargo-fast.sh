#!/usr/bin/env bash
set -euo pipefail

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

if [ -z "${CARGO_INCREMENTAL:-}" ]; then
  export CARGO_INCREMENTAL=1
fi

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
      if [ "${CARGO_INCREMENTAL}" = "0" ]; then
        export RUSTC_WRAPPER="sccache"
        note "using sccache"
      else
        note "sccache available but skipped because incremental is enabled"
      fi
      ;;
  esac
fi

HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
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

exec cargo "$@"
