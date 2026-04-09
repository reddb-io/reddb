#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="${SERVICE_NAME:-reddb}"
BINARY_PATH="${BINARY_PATH:-/usr/local/bin/red}"
RUN_USER="${RUN_USER:-reddb}"
RUN_GROUP="${RUN_GROUP:-reddb}"
DATA_PATH="${DATA_PATH:-/var/lib/reddb/data.rdb}"
BIND_ADDR="${BIND_ADDR:-0.0.0.0:50051}"
TRANSPORT="${TRANSPORT:-grpc}"

usage() {
  cat <<'EOF'
Install RedDB as a systemd service.

Usage:
  sudo ./scripts/install-systemd-service.sh [options]

Options:
  --binary <path>         Path to the red binary (default: /usr/local/bin/red)
  --service-name <name>   systemd unit name (default: reddb)
  --user <name>           Service user (default: reddb)
  --group <name>          Service group (default: reddb)
  --path <file>           Persistent database file (default: /var/lib/reddb/data.rdb)
  --bind <addr>           Listen address (default: 0.0.0.0:50051)
  --grpc                  Run the gRPC server (default)
  --http                  Run the HTTP server
  -h, --help              Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      BINARY_PATH="$2"
      shift 2
      ;;
    --service-name)
      SERVICE_NAME="$2"
      shift 2
      ;;
    --user)
      RUN_USER="$2"
      shift 2
      ;;
    --group)
      RUN_GROUP="$2"
      shift 2
      ;;
    --path)
      DATA_PATH="$2"
      shift 2
      ;;
    --bind)
      BIND_ADDR="$2"
      shift 2
      ;;
    --grpc)
      TRANSPORT="grpc"
      shift
      ;;
    --http)
      TRANSPORT="http"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ $EUID -ne 0 ]]; then
  echo "Run this script as root (sudo)." >&2
  exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
  echo "systemctl not found. This script requires systemd." >&2
  exit 1
fi

if [[ ! -x "$BINARY_PATH" ]]; then
  echo "Binary not found or not executable: $BINARY_PATH" >&2
  exit 1
fi

DATA_DIR="$(dirname "$DATA_PATH")"
UNIT_PATH="/etc/systemd/system/${SERVICE_NAME}.service"

if ! getent group "$RUN_GROUP" >/dev/null 2>&1; then
  groupadd --system "$RUN_GROUP"
fi

if ! id -u "$RUN_USER" >/dev/null 2>&1; then
  useradd --system --gid "$RUN_GROUP" --home-dir "$DATA_DIR" --shell /usr/sbin/nologin "$RUN_USER"
fi

install -d -o "$RUN_USER" -g "$RUN_GROUP" -m 0750 "$DATA_DIR"

cat >"$UNIT_PATH" <<EOF
[Unit]
Description=RedDB unified database service
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${RUN_USER}
Group=${RUN_GROUP}
WorkingDirectory=${DATA_DIR}
ExecStart=${BINARY_PATH} server --${TRANSPORT} --path ${DATA_PATH} --bind ${BIND_ADDR}
Restart=always
RestartSec=2
LimitSTACK=16M
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ProtectControlGroups=true
ProtectKernelTunables=true
ProtectKernelModules=true
RestrictNamespaces=true
LockPersonality=true
MemoryDenyWriteExecute=true
ReadWritePaths=${DATA_DIR}

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now "${SERVICE_NAME}.service"

echo "Installed and started ${SERVICE_NAME}.service"
echo "Status: systemctl status ${SERVICE_NAME}.service"
