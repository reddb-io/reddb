#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET="${1:-replica}"
MODE="${2:-all}"

case "${TARGET}" in
    *.yml)
        case "$(basename "${TARGET}")" in
            docker-compose.replica.yml|docker-compose.yml)
                TARGET="replica"
                ;;
            docker-compose.full.yml)
                TARGET="full"
                ;;
            docker-compose.remote.yml|dev.docker-compose.yml)
                TARGET="remote"
                ;;
        esac
        ;;
esac

exec "${SCRIPT_DIR}/test-environment.sh" "${TARGET}" "${MODE}"
