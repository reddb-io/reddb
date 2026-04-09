#!/usr/bin/env bash
#
# RedDB uninstaller
#
set -e

PRIMARY_BINARY_NAME="red"
LEGACY_BINARIES=("reddb" "reddb-grpc")
COMMON_INSTALL_DIRS=(
  "$HOME/.local/bin"
  "/usr/local/bin"
  "/usr/bin"
  "$HOME/bin"
)

FOUND=()

find_installations() {
  for dir in "${COMMON_INSTALL_DIRS[@]}"; do
    for b in "$PRIMARY_BINARY_NAME" "${LEGACY_BINARIES[@]}"; do
      if [ -f "$dir/$b" ]; then
        FOUND+=("$dir/$b")
      fi
    done
  done

  if [ ${#FOUND[@]} -eq 0 ]; then
    echo "No RedDB binaries found."
    exit 1
  fi
}

remove_binary() {
  local p="$1"
  if [ -L "$p" ] || [ -f "$p" ]; then
    if [ -w "$p" ]; then
      rm -f "$p"
    else
      sudo rm -f "$p"
    fi
    echo "Removed: $p"
  fi
}

main() {
  find_installations

  echo "The following files will be removed:"
  printf " - %s\n" "${FOUND[@]}"

  if [ -t 0 ]; then
    read -p "Continue? (y/N) " -r
    [[ "$REPLY" =~ ^[Yy]$ ]] || { echo "Cancelled."; exit 0; }
  fi

  for p in "${FOUND[@]}"; do
    remove_binary "$p"
  done

  echo "RedDB uninstalled."
}

main
