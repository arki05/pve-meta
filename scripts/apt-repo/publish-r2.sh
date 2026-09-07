#!/usr/bin/env bash
# scripts/apt-repo/publish-r2.sh <repo-dir>
#
# Syncs a repo tree built by build-repo.sh to the R2 bucket with rclone.
# pool/ is uploaded first, dists/ (which contains the signed Release) last --
# both for the public tree and, if present, the private/ mirror -- so an apt
# client can never observe a Release file that points at pool files that
# haven't landed yet. Content-Type is set explicitly per file kind, since
# R2's default guess (by extension) gets the extension-less apt index files
# wrong.
#
# See docs/DISTRIBUTION.md for required secrets and the R2 bucket setup.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/apt-repo/lib.sh
source "$SCRIPT_DIR/lib.sh"

usage() {
    cat <<EOF
Usage: $(basename "$0") <repo-dir>

Required env: R2_ACCOUNT_ID, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_BUCKET
EOF
}

[ $# -eq 1 ] || { usage >&2; exit 1; }
REPO_DIR="$1"
[ -d "$REPO_DIR" ] || die "repo-dir '$REPO_DIR' does not exist -- run build-repo.sh first"

require_cmd rclone
have_r2_env || die "R2_ACCOUNT_ID / R2_ACCESS_KEY_ID / R2_SECRET_ACCESS_KEY / R2_BUCKET must all be set"

write_rclone_config

RCLONE_FLAGS=(--checksum --no-update-modtime)

# sync_with_content_type <local-dir> <remote-dir> <content-type> <include-pattern...>
# Only files matching one of the include patterns (matched by basename, per
# rclone filter semantics) are copied; everything else in <local-dir> is
# left for a different call with a different content type.
sync_with_content_type() {
    local src="$1" dst="$2" content_type="$3"
    shift 3
    [ -d "$src" ] || return 0

    local includes=()
    local pat
    for pat in "$@"; do
        includes+=(--include "$pat")
    done

    rclone copy "$src" "$dst" "${RCLONE_FLAGS[@]}" "${includes[@]}" \
        --header-upload "Content-Type: ${content_type}"
}

publish_pool() {
    local prefix="$1"
    local local_root="$REPO_DIR"
    [ -n "$prefix" ] && local_root="$REPO_DIR/$prefix"
    local remote="r2:${R2_BUCKET}"
    [ -n "$prefix" ] && remote="r2:${R2_BUCKET}/${prefix}"
    [ -d "$local_root/pool" ] || return 0

    log "publishing ${prefix:+$prefix/}pool/ ..."
    sync_with_content_type "$local_root/pool" "$remote/pool" \
        "application/vnd.debian.binary-package" "*.deb"
}

publish_keyring() {
    local prefix="$1"
    local local_root="$REPO_DIR"
    [ -n "$prefix" ] && local_root="$REPO_DIR/$prefix"
    local remote="r2:${R2_BUCKET}"
    [ -n "$prefix" ] && remote="r2:${R2_BUCKET}/${prefix}"
    [ -f "$local_root/pve-meta.asc" ] || return 0

    sync_with_content_type "$local_root" "$remote" "application/pgp-keys" "pve-meta.asc"
}

publish_dists() {
    local prefix="$1"
    local local_root="$REPO_DIR"
    [ -n "$prefix" ] && local_root="$REPO_DIR/$prefix"
    local remote="r2:${R2_BUCKET}"
    [ -n "$prefix" ] && remote="r2:${R2_BUCKET}/${prefix}"
    [ -d "$local_root/dists" ] || return 0

    log "publishing ${prefix:+$prefix/}dists/ ..."
    # Package indexes first...
    sync_with_content_type "$local_root/dists" "$remote/dists" \
        "text/plain; charset=utf-8" "Packages"
    sync_with_content_type "$local_root/dists" "$remote/dists" \
        "application/gzip" "*.gz"
    # ...then the signed manifest, last: this is the file(s) an apt client
    # trusts to say what else exists, so it must be the last thing to change.
    sync_with_content_type "$local_root/dists" "$remote/dists" \
        "text/plain; charset=utf-8" "Release" "InRelease"
    sync_with_content_type "$local_root/dists" "$remote/dists" \
        "application/pgp-signature" "Release.gpg"
}

publish_pool ""
publish_pool "private"
publish_keyring ""
publish_keyring "private"
publish_dists ""
publish_dists "private"

log "done."
