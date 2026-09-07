#!/usr/bin/env bash
# scripts/apt-repo/build-repo.sh <deb-dir> <repo-dir>
#
# Builds (incrementally -- never from scratch) a static, GPG-signed apt
# repository tree suitable for handing straight to publish-r2.sh. Produces:
#
#   <repo-dir>/pool/main/<letter>/<pkg>/<pkg>_<ver>_<arch>.deb
#   <repo-dir>/dists/trixie/main/binary-amd64/{Packages,Packages.gz}
#   <repo-dir>/dists/trixie/{Release,Release.gpg,InRelease}
#   <repo-dir>/pve-meta.asc                        (armored public key)
#
# ...and, if there are any private packages, an identically-shaped tree
# rooted at <repo-dir>/private/ for the private component/suite prefix (see
# docs/DISTRIBUTION.md, "Private prefix").
#
# See docs/DISTRIBUTION.md for the full picture (required env, how the GPG
# key is created, R2 bucket setup, and a local dry-run recipe).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/apt-repo/lib.sh
source "$SCRIPT_DIR/lib.sh"

SUITE="trixie"
ARCH="amd64"
COMPONENT="main"

usage() {
    cat <<EOF
Usage: $(basename "$0") <deb-dir> <repo-dir>

  <deb-dir>   Directory of freshly-built .deb files to add to the repo.
              Public packages go directly in <deb-dir>/*.deb; packages for
              the private prefix go in <deb-dir>/private/*.deb.
  <repo-dir>  Output directory for the apt tree (pool/, dists/, and the
              private/ mirror of both, if there are private packages).
              Reused across runs -- existing pool/ contents are kept, never
              wiped.

Required env:
  APT_GPG_KEY_ID        GPG key id (or fingerprint) used to sign Release.

Optional env:
  APT_GPG_PRIVATE_KEY   Armored private key to import before signing (CI).
  APT_GPG_PASSPHRASE    Passphrase for that key, if it has one.
  R2_ACCOUNT_ID, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_BUCKET
                        If all four are set, the existing pool/ (public and
                        private) is pulled down from the bucket first via
                        rclone, so old package versions already published
                        are never lost -- this script only ever appends.
EOF
}

[ $# -eq 2 ] || { usage >&2; exit 1; }
DEB_DIR="$1"
REPO_DIR="$2"

[ -d "$DEB_DIR" ] || die "deb-dir '$DEB_DIR' does not exist"
[ -n "${APT_GPG_KEY_ID:-}" ] || die "APT_GPG_KEY_ID must be set"

require_cmd gpg
require_cmd gpgv
require_cmd dpkg-deb

mkdir -p "$REPO_DIR"

import_gpg_key_from_env

# --- 1. pull the existing pool down first, so old versions are kept -------

pull_existing_pool() {
    local prefix="$1" # "" or "private"
    local remote_path="${R2_BUCKET}"
    [ -n "$prefix" ] && remote_path="${R2_BUCKET}/${prefix}"
    local local_dir="$REPO_DIR"
    [ -n "$prefix" ] && local_dir="$REPO_DIR/$prefix"

    mkdir -p "$local_dir/pool"
    if rclone lsf "r2:${remote_path}/pool" >/dev/null 2>&1; then
        log "pulling existing ${prefix:+$prefix/}pool/ from r2:${remote_path}/pool"
        rclone copy "r2:${remote_path}/pool" "$local_dir/pool"
    else
        log "no existing ${prefix:+$prefix/}pool/ found on the bucket (or bucket unreachable), starting fresh"
    fi
}

if have_r2_env; then
    write_rclone_config
    pull_existing_pool ""
    pull_existing_pool "private"
else
    log "R2 credentials not set -- skipping pull of the existing pool (local/dry-run mode)"
fi

# --- 2. pool subdirectory for a package name, Debian-style -----------------
# pool/<component>/<letter-or-lib-prefix>/<source-pkg>/<file>.deb

pool_letter() {
    local pkg="$1"
    case "$pkg" in
        lib?*) echo "${pkg:0:4}" ;;
        *)     echo "${pkg:0:1}" ;;
    esac
}

# --- 3. copy new .deb files into the pool -----------------------------------
# Append-only: an existing file at the destination path is left alone if
# byte-identical, and is a hard error (version collision, different bytes at
# the same name) rather than a silent overwrite.

add_debs_to_pool() {
    local src_dir="$1" prefix="$2"
    local pool_root="$REPO_DIR"
    [ -n "$prefix" ] && pool_root="$REPO_DIR/$prefix"
    pool_root="$pool_root/pool/$COMPONENT"

    shopt -s nullglob
    local f
    for f in "$src_dir"/*.deb; do
        local pkg letter dest_dir dest
        pkg="$(dpkg-deb -f "$f" Package)"
        letter="$(pool_letter "$pkg")"
        dest_dir="$pool_root/$letter/$pkg"
        mkdir -p "$dest_dir"
        dest="$dest_dir/$(basename "$f")"
        if [ -e "$dest" ]; then
            if cmp -s "$f" "$dest"; then
                log "already in pool, unchanged: $(basename "$f")"
            else
                die "refusing to overwrite $dest with different content from $f (version collision?)"
            fi
        else
            log "adding to pool: $dest"
            install -m 0644 "$f" "$dest"
        fi
    done
    shopt -u nullglob
}

add_debs_to_pool "$DEB_DIR" ""
[ -d "$DEB_DIR/private" ] && add_debs_to_pool "$DEB_DIR/private" "private"

HAVE_PRIVATE=0
if [ -d "$REPO_DIR/private/pool" ] && find "$REPO_DIR/private/pool" -name '*.deb' -print -quit 2>/dev/null | grep -q .; then
    HAVE_PRIVATE=1
fi

# --- 4. export the public key alongside the tree ----------------------------

gpg --batch --yes --armor --export "$APT_GPG_KEY_ID" > "$REPO_DIR/pve-meta.asc"
if [ "$HAVE_PRIVATE" -eq 1 ]; then
    mkdir -p "$REPO_DIR/private"
    cp "$REPO_DIR/pve-meta.asc" "$REPO_DIR/private/pve-meta.asc"
fi

# --- 5. Packages / Packages.gz ---------------------------------------------

generate_packages() {
    local prefix="$1"
    local root="$REPO_DIR"
    [ -n "$prefix" ] && root="$REPO_DIR/$prefix"
    local dists_dir="$root/dists/$SUITE/$COMPONENT/binary-$ARCH"
    mkdir -p "$dists_dir"
    mkdir -p "$root/pool/$COMPONENT"

    (
        cd "$root"
        if command -v apt-ftparchive >/dev/null 2>&1; then
            apt-ftparchive packages "pool/$COMPONENT" > "dists/$SUITE/$COMPONENT/binary-$ARCH/Packages"
        else
            log "apt-ftparchive not found, falling back to dpkg-scanpackages"
            require_cmd dpkg-scanpackages
            dpkg-scanpackages "pool/$COMPONENT" /dev/null > "dists/$SUITE/$COMPONENT/binary-$ARCH/Packages" 2>/dev/null
        fi
    )
    gzip -9 -k -f "$dists_dir/Packages"
}

generate_packages ""
[ "$HAVE_PRIVATE" -eq 1 ] && generate_packages "private"

# --- 6. Release --------------------------------------------------------------

sha_line() {
    # sha_line <file> <relative-path-for-Release> <algo>
    local file="$1" relpath="$2" algo="$3"
    local sum size
    sum="$("${algo}sum" "$file" | awk '{print $1}')"
    size="$(wc -c < "$file" | tr -d ' ')"
    printf ' %s %s %s\n' "$sum" "$size" "$relpath"
}

generate_release() {
    local prefix="$1"
    local root="$REPO_DIR"
    local label="pve-meta"
    if [ -n "$prefix" ]; then
        root="$REPO_DIR/$prefix"
        label="pve-meta (private)"
    fi
    local dists_dir="$root/dists/$SUITE"
    local bindir="$COMPONENT/binary-$ARCH"
    local rel="$dists_dir/Release"

    {
        echo "Origin: pve-meta"
        echo "Label: $label"
        echo "Suite: $SUITE"
        echo "Codename: $SUITE"
        echo "Version: $SUITE"
        echo "Architectures: $ARCH"
        echo "Components: $COMPONENT"
        echo "Description: pve-meta ${prefix:+private }apt repository"
        echo "Date: $(date -Ru)"
        echo "MD5Sum:"
        sha_line "$dists_dir/$bindir/Packages" "$bindir/Packages" md5
        sha_line "$dists_dir/$bindir/Packages.gz" "$bindir/Packages.gz" md5
        echo "SHA1:"
        sha_line "$dists_dir/$bindir/Packages" "$bindir/Packages" sha1
        sha_line "$dists_dir/$bindir/Packages.gz" "$bindir/Packages.gz" sha1
        echo "SHA256:"
        sha_line "$dists_dir/$bindir/Packages" "$bindir/Packages" sha256
        sha_line "$dists_dir/$bindir/Packages.gz" "$bindir/Packages.gz" sha256
    } > "$rel"
}

generate_release ""
[ "$HAVE_PRIVATE" -eq 1 ] && generate_release "private"

# --- 7. sign: Release.gpg (detached) + InRelease (clearsigned) -------------

sign_release() {
    local prefix="$1"
    local root="$REPO_DIR"
    [ -n "$prefix" ] && root="$REPO_DIR/$prefix"
    local dists_dir="$root/dists/$SUITE"
    local rel="$dists_dir/Release"

    local gpg_args=()
    while IFS= read -r arg; do
        gpg_args+=("$arg")
    done < <(gpg_sign_args)

    gpg "${gpg_args[@]}" --local-user "$APT_GPG_KEY_ID" --digest-algo SHA256 \
        --clearsign -o "$dists_dir/InRelease.tmp" "$rel"
    mv "$dists_dir/InRelease.tmp" "$dists_dir/InRelease"

    gpg "${gpg_args[@]}" --local-user "$APT_GPG_KEY_ID" --digest-algo SHA256 \
        -abs -o "$rel.gpg.tmp" "$rel"
    mv "$rel.gpg.tmp" "$rel.gpg"
}

sign_release ""
[ "$HAVE_PRIVATE" -eq 1 ] && sign_release "private"

# --- 8. sanity-check what we just signed, with gpgv (no network, no CI creds
#        needed) -- catches a broken signature before anything is published.

verify_signature() {
    local prefix="$1"
    local root="$REPO_DIR"
    [ -n "$prefix" ] && root="$REPO_DIR/$prefix"
    local dists_dir="$root/dists/$SUITE"
    local pubkeyring="$root/.verify-keyring.gpg"

    gpg --dearmor < "$REPO_DIR/pve-meta.asc" > "$pubkeyring"
    if gpgv --keyring "$pubkeyring" "$dists_dir/Release.gpg" "$dists_dir/Release"; then
        log "${prefix:+$prefix/}dists/$SUITE/Release.gpg: signature OK"
    else
        rm -f "$pubkeyring"
        die "${prefix:+$prefix/}dists/$SUITE/Release.gpg failed gpgv verification"
    fi
    rm -f "$pubkeyring"
}

verify_signature ""
[ "$HAVE_PRIVATE" -eq 1 ] && verify_signature "private"

log "done. repo tree ready at $REPO_DIR$([ "$HAVE_PRIVATE" -eq 1 ] && echo " (with a private/ prefix)")."
