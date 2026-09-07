#!/usr/bin/env bash
# scripts/apt-repo/lib.sh
#
# Shared helpers for the pve-meta apt-repo tooling (build-repo.sh,
# publish-r2.sh). Not meant to be executed directly -- source it.
#
# shellcheck shell=bash

log()  { echo "[apt-repo] $*" >&2; }
die()  { echo "[apt-repo] ERROR: $*" >&2; exit 1; }

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "required command '$1' not found in PATH"
}

# True (exit 0) if all four R2 credential env vars are set and non-empty.
have_r2_env() {
    [ -n "${R2_ACCOUNT_ID:-}" ] && [ -n "${R2_ACCESS_KEY_ID:-}" ] \
        && [ -n "${R2_SECRET_ACCESS_KEY:-}" ] && [ -n "${R2_BUCKET:-}" ]
}

# Writes a throwaway rclone config file pointing a remote named "r2" at the
# Cloudflare R2 S3-compatible endpoint, from the R2_* env vars, and exports
# RCLONE_CONFIG so every subsequent `rclone` call in this process (and
# children) picks it up automatically -- callers never pass --config.
write_rclone_config() {
    have_r2_env || die "R2_ACCOUNT_ID / R2_ACCESS_KEY_ID / R2_SECRET_ACCESS_KEY / R2_BUCKET must all be set"
    require_cmd rclone

    local cfg
    cfg="$(mktemp -t pve-meta-rclone.XXXXXX)"
    cat > "$cfg" <<EOF
[r2]
type = s3
provider = Cloudflare
env_auth = false
access_key_id = ${R2_ACCESS_KEY_ID}
secret_access_key = ${R2_SECRET_ACCESS_KEY}
endpoint = https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com
acl = private
no_check_bucket = true
EOF
    chmod 600 "$cfg"
    export RCLONE_CONFIG="$cfg"
    log "wrote rclone config for remote 'r2' -> bucket '${R2_BUCKET}' ($cfg)"
}

# Imports a GPG private key from APT_GPG_PRIVATE_KEY (armored, possibly
# multi-line) into the invoking user's keyring, if a key with id
# APT_GPG_KEY_ID isn't already present. No-op if the env var is unset --
# local/dev signing is expected to use an already-imported key.
import_gpg_key_from_env() {
    [ -n "${APT_GPG_PRIVATE_KEY:-}" ] || return 0
    require_cmd gpg

    if [ -n "${APT_GPG_KEY_ID:-}" ] && gpg --list-secret-keys "${APT_GPG_KEY_ID}" >/dev/null 2>&1; then
        log "GPG key ${APT_GPG_KEY_ID} already present in keyring, skipping import"
        return 0
    fi

    log "importing APT_GPG_PRIVATE_KEY into the GPG keyring"
    printf '%s\n' "${APT_GPG_PRIVATE_KEY}" | gpg --batch --yes --import -

    if [ -n "${APT_GPG_KEY_ID:-}" ]; then
        # Trust our own freshly-imported signing key so gpg never prompts.
        printf '5\ny\n' | gpg --batch --yes --command-fd 0 --edit-key "${APT_GPG_KEY_ID}" trust >/dev/null 2>&1 || true
    fi
}

# Emits the extra gpg flags needed for unattended signing, one per line (read
# into an array with `mapfile -t args < <(gpg_sign_args)`). Unattended CI
# keys are normally passphrase-less; APT_GPG_PASSPHRASE is the escape hatch
# for keys that aren't.
gpg_sign_args() {
    echo "--batch"
    echo "--yes"
    echo "--pinentry-mode"
    echo "loopback"
    if [ -n "${APT_GPG_PASSPHRASE:-}" ]; then
        echo "--passphrase"
        echo "${APT_GPG_PASSPHRASE}"
    fi
}
