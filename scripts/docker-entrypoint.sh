#!/bin/sh
# RedDB Docker entrypoint
# Raises the stack limit for deep recursive initialization and normalizes
# docker commands to the unified `red` binary.
set -eu

ulimit -s 16384 2>/dev/null || true

if [ "$#" -eq 0 ]; then
    set -- /usr/local/bin/red server --grpc --path /data/data.rdb --bind 0.0.0.0:50051
elif [ "${1#-}" != "$1" ]; then
    set -- /usr/local/bin/red server --grpc "$@"
else
    case "$1" in
        red)
            cmd="$1"
            shift
            set -- "/usr/local/bin/$cmd" "$@"
            ;;
        server|replica|health|status|query|insert|get|delete|mcp|auth|connect|help|version)
            set -- /usr/local/bin/red "$@"
            ;;
    esac
fi

exec "$@"
