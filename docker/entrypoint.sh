#!/bin/sh
# RedDB container entrypoint — *_FILE secret expansion shim.
#
# Why this exists:
#   RedDB persists auth state in the encrypted vault region of the main
#   .rdb file. The encryption key is derived from REDDB_CERTIFICATE
#   (primary) or REDDB_VAULT_KEY (passphrase fallback). NEITHER value
#   should be baked into the image, the layer cache, or the env of an
#   audited running process. The recommended pattern is to mount the
#   secret as a file (Docker/Swarm secret, k8s Secret volume, AWS
#   Secrets Manager via efs-csi, etc.) and pass its PATH via *_FILE.
#
# What this does:
#   For each known secret env var:
#     - If `${VAR}_FILE` is set and points to a readable file, load the
#       trimmed file contents into VAR and unset VAR_FILE.
#     - Otherwise, if a file at the conventional /run/secrets/<lowercased>
#       path exists, fall back to that.
#   Then exec `red` with the original CMD/ARG list.
#
# Belt-and-suspenders note:
#   The `red` binary itself also resolves *_FILE env vars (Agent #2).
#   This shell shim is a defensive duplicate so that older binaries,
#   debugging shells, and exec-from-orchestrator workflows still benefit
#   from the same convention. It is intentionally a no-op when the
#   binary already resolved everything.

set -eu

# POSIX-portable trim helper (no bash-specific features).
_load_secret() {
    var_name=$1
    file_var="${var_name}_FILE"

    # Use eval to dereference the dynamically-named variable.
    eval "file_path=\${${file_var}:-}"
    eval "current_value=\${${var_name}:-}"

    # If the env var is already populated, leave it alone — explicit wins.
    if [ -n "$current_value" ]; then
        return 0
    fi

    # Explicit *_FILE path takes precedence.
    if [ -n "$file_path" ]; then
        if [ -r "$file_path" ]; then
            value=$(cat "$file_path" | tr -d '\r' | sed -e 's/[[:space:]]*$//')
            export "$var_name=$value"
            unset "$file_var" 2>/dev/null || true
            return 0
        else
            echo "entrypoint: ${file_var}=$file_path is set but not readable" >&2
            return 0
        fi
    fi

    # Conventional /run/secrets/<lowercased> fallback.
    lowered=$(echo "$var_name" | tr '[:upper:]' '[:lower:]')
    default_path="/run/secrets/$lowered"
    if [ -r "$default_path" ]; then
        value=$(cat "$default_path" | tr -d '\r' | sed -e 's/[[:space:]]*$//')
        export "$var_name=$value"
        return 0
    fi
}

# Order matters: certificate first (primary path), then passphrase
# fallback, then admin bootstrap creds, then root token.
_load_secret REDDB_CERTIFICATE
_load_secret REDDB_VAULT_KEY
_load_secret REDDB_USERNAME
_load_secret REDDB_PASSWORD
_load_secret REDDB_ROOT_TOKEN

# Drop privileges defensively if started as root (e.g. someone overrode
# USER in a `docker run`). The image's reddb user is uid/gid 10001.
if [ "$(id -u)" = "0" ] && command -v su-exec >/dev/null 2>&1; then
    exec su-exec 10001:10001 /usr/local/bin/red "$@"
fi

exec /usr/local/bin/red "$@"
