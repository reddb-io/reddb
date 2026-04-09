#!/usr/bin/env bash
#
# RedDB installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash -s -- --channel next
#   curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash -s -- --version v0.1.0
#
set -e

REPO="forattini-dev/reddb"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
BINARY_NAME="reddb"
GRPC_BINARY_NAME="reddb-grpc"
CHANNEL="stable"
VERSION=""
STATIC=""
WITH_GRPC="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --channel)
      CHANNEL="$2"
      shift 2
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    --install-dir)
      INSTALL_DIR="$2"
      shift 2
      ;;
    --with-grpc)
      WITH_GRPC="true"
      shift
      ;;
    --static)
      STATIC="true"
      shift
      ;;
    -h|--help)
      echo "RedDB installer"
      echo ""
      echo "Usage: $0 [OPTIONS]"
      echo ""
      echo "Options:"
      echo "  --channel <stable|next|latest>  Release channel (default: stable)"
      echo "  --version <version>             Install specific version (e.g., v0.1.0)"
      echo "  --install-dir <path>            Installation directory (default: ~/.local/bin)"
      echo "  --with-grpc                     Also install reddb-grpc"
      echo "  --static                        Use static aarch64 build when available"
      echo "  -h, --help                      Show this help message"
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      exit 1
      ;;
  esac
done

if [[ "$CHANNEL" != "stable" && "$CHANNEL" != "next" && "$CHANNEL" != "latest" ]]; then
  echo "Invalid channel: $CHANNEL"
  exit 1
fi

detect_platform() {
  local os
  local arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux*) OS="linux" ;;
    Darwin*) OS="darwin" ;;
    MINGW*|MSYS*|CYGWIN*) OS="windows" ;;
    *) echo "Unsupported operating system: $os"; exit 1 ;;
  esac

  case "$arch" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    armv7l|armv7) ARCH="armv7" ;;
    *) echo "Unsupported architecture: $arch"; exit 1 ;;
  esac

  if [[ "$OS" == "linux" ]] && [[ "$STATIC" == "true" ]] && [[ "$ARCH" == "aarch64" ]]; then
    PLATFORM="${OS}-${ARCH}-static"
  else
    PLATFORM="${OS}-${ARCH}"
  fi
}

json_get_value() {
  local json="$1"
  local key="$2"
  echo "$json" | sed -n "s/.*\"${key}\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p" | head -1
}

extract_release_tag() {
  local json="$1"
  local prerelease_only="$2"

  if [[ "$prerelease_only" == "true" ]]; then
    echo "$json" | awk '
      /"tag_name"/ { gsub(/.*"tag_name"[[:space:]]*:[[:space:]]*"/, ""); gsub(/".*/, ""); tag=$0 }
      /"prerelease"[[:space:]]*:[[:space:]]*true/ { prerelease=1 }
      /^\s*\}/ {
        if (prerelease == 1 && tag != "") { print tag; exit }
        prerelease=0
        tag=""
      }
    ' | head -1
  else
    echo "$json" | awk -F'"' '/"tag_name"/ {print $4; exit}'
  fi
}

fetch_release_info() {
  local api_url
  local releases_json

  if [[ -n "$VERSION" ]]; then
    api_url="https://api.github.com/repos/$REPO/releases/tags/$VERSION"
  elif [[ "$CHANNEL" == "stable" ]]; then
    api_url="https://api.github.com/repos/$REPO/releases/latest"
  else
    api_url="https://api.github.com/repos/$REPO/releases"
  fi

  if command -v curl >/dev/null 2>&1; then
    releases_json=$(curl -fsSL "$api_url" 2>/dev/null)
  elif command -v wget >/dev/null 2>&1; then
    releases_json=$(wget -qO- "$api_url" 2>/dev/null)
  else
    echo "curl or wget is required"
    exit 1
  fi

  if [[ -z "$releases_json" ]]; then
    echo "Could not fetch release data"
    exit 1
  fi

  if [[ -n "$VERSION" ]] || [[ "$CHANNEL" == "stable" ]]; then
    RELEASE_TAG=$(json_get_value "$releases_json" "tag_name")
  elif [[ "$CHANNEL" == "next" ]]; then
    RELEASE_TAG=$(extract_release_tag "$releases_json" true)
    if [[ -z "$RELEASE_TAG" ]]; then
      RELEASE_TAG=$(extract_release_tag "$releases_json" false)
    fi
  else
    RELEASE_TAG=$(extract_release_tag "$releases_json" false)
  fi

  if [[ -z "$RELEASE_TAG" ]]; then
    echo "Could not determine release version"
    exit 1
  fi
}

download_binary() {
  local name="$1"
  local binary_name="${name}-${PLATFORM}"
  local tmp_file="/tmp/${binary_name}"

  if [[ "$OS" == "windows" ]]; then
    binary_name="${binary_name}.exe"
    tmp_file="/tmp/${binary_name}"
  fi

  local url="https://github.com/${REPO}/releases/download/${RELEASE_TAG}/${binary_name}"
  echo "Downloading ${name} from ${url}"

  if command -v curl >/dev/null 2>&1; then
    curl -fL -o "$tmp_file" "$url"
  else
    wget -O "$tmp_file" "$url"
  fi

  DOWNLOADED_FILES+=("${name}:${tmp_file}")
}

verify_checksum() {
  local asset_path="$1"
  local expected_hash=""
  local actual_hash=""
  local asset_file
  asset_file="$(basename "$asset_path")"
  local checksum_url="https://github.com/${REPO}/releases/download/${RELEASE_TAG}/${asset_file}.sha256"
  local checksum_file="/tmp/${asset_file}.sha256"

  if command -v curl >/dev/null 2>&1; then
    if ! curl -fsSL -o "$checksum_file" "$checksum_url" 2>/dev/null; then
      return 0
    fi
  else
    if ! wget -qO "$checksum_file" "$checksum_url" 2>/dev/null; then
      return 0
    fi
  fi

  expected_hash=$(cat "$checksum_file" | awk '{print $1}' | tr -d '[:space:]')
  rm -f "$checksum_file"
  if [[ -z "$expected_hash" ]]; then
    return 0
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    actual_hash=$(sha256sum "$asset_path" | awk '{print $1}')
  elif command -v shasum >/dev/null 2>&1; then
    actual_hash=$(shasum -a 256 "$asset_path" | awk '{print $1}')
  else
    return 0
  fi

  if [[ "$expected_hash" != "$actual_hash" ]]; then
    echo "Checksum mismatch for ${asset_file}"
    exit 1
  fi
}

install_file() {
  local name="$1"
  local tmp_file="$2"
  local binary_path="${INSTALL_DIR}/${name}"

  mkdir -p "$INSTALL_DIR"
  mv "$tmp_file" "$binary_path"

  if [[ "$OS" != "windows" ]]; then
    chmod +x "$binary_path"
  fi

  echo "Installed: ${binary_path}"
}

main() {
  detect_platform
  fetch_release_info

  declare -a DOWNLOADED_FILES

  download_binary "$BINARY_NAME"
  if [[ "$WITH_GRPC" == "true" ]]; then
    download_binary "$GRPC_BINARY_NAME"
  fi

  for entry in "${DOWNLOADED_FILES[@]}"; do
    name="${entry%%:*}"
    file="${entry#*:}"
    verify_checksum "$file"
    install_file "$name" "$file"
  done

  echo "✅ RedDB installed."
  echo ""
  echo "Quick start:"
  echo "  reddb --help"
  if [[ "$WITH_GRPC" == "true" ]]; then
    echo "  reddb-grpc --help"
  fi
}

main

