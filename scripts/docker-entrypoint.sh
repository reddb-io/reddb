#!/bin/sh
# RedDB Docker entrypoint
# Raises the stack limit for deep recursive initialization and normalizes
# docker commands to the unified `red` binary.
set -eu

ulimit -s 16384 2>/dev/null || true

DEFAULT_DATA_PATH="${REDDB_DATA_PATH:-/data/data.rdb}"
DEFAULT_GRPC_BIND="${REDDB_GRPC_BIND_ADDR:-${REDDB_BIND_ADDR:-0.0.0.0:50051}}"
DEFAULT_HTTP_BIND="${REDDB_HTTP_BIND_ADDR:-0.0.0.0:8080}"

set_default_server_args() {
    set -- /usr/local/bin/red server \
        --path "$DEFAULT_DATA_PATH" \
        --grpc-bind "$DEFAULT_GRPC_BIND" \
        --http-bind "$DEFAULT_HTTP_BIND"
}

if [ "$#" -eq 0 ]; then
    set_default_server_args
elif [ "${1#-}" != "$1" ]; then
    set -- /usr/local/bin/red server "$@"
else
    case "$1" in
        red)
            cmd="$1"
            shift
            set -- "/usr/local/bin/$cmd" "$@"
            ;;
        server)
            if [ "$#" -eq 1 ]; then
                set_default_server_args
            else
                set -- /usr/local/bin/red "$@"
            fi
            ;;
        replica|health|status|query|insert|get|delete|mcp|auth|connect|service|help|version)
            set -- /usr/local/bin/red "$@"
            ;;
    esac
fi

exec "$@"
