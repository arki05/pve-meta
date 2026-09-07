#!/usr/bin/env bash
# scripts/watch-pve/check.sh
#
# Scheduled ceiling watcher (see .github/workflows/watch-pve.yml). Compares
# the Proxmox no-subscription repo's current versions of the four packages
# our patches touch against ceilings.toml's [tested] table at the repo
# root. For each package with a newer upstream version:
#
#   1. downloads the .deb and extracts it with `dpkg-deb -x`;
#   2. for pve-manager: runs `pve-manager-patch/pve-meta-patch verify` against
#      the extracted pvemanagerlib.js / index.html.tpl;
#   3. for every package: `patch --dry-run -p1` of every
#      pve-manager-patches/lifecycle/<pkg>_*.diff registered for it.
#
# A package with a clean dry-run on every applicable check gets its ceiling
# bumped and rolled into one PR. A package with any failure gets rolled into
# one issue (label: pve-upgrade) carrying the patch/verify output, with a
# TODO block marking the hand-off to the (not yet built) LLM-assisted fix
# flow described in docs/VISION.typ ("Upgrade gating").
#
# Local dry run (no GitHub side effects, just prints what would happen):
#   PVE_META_DRY_RUN=1 scripts/watch-pve/check.sh
#
# See docs/DISTRIBUTION.md ("Ceiling watcher") for the full picture and the
# manual escape hatch.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CEILINGS="$REPO_ROOT/ceilings.toml"
LIFECYCLE_DIR="$REPO_ROOT/pve-manager-patches/lifecycle"
UI_PATCH_TOOL="$REPO_ROOT/pve-manager-patch/pve-meta-patch"

PVE_REPO_BASE="http://download.proxmox.com/debian/pve"
PVE_SUITE="trixie"
PVE_COMPONENT="pve-no-subscription"
PVE_ARCH="amd64"
PACKAGES_URL="$PVE_REPO_BASE/dists/$PVE_SUITE/$PVE_COMPONENT/binary-$PVE_ARCH/Packages"

TRACKED_PACKAGES=(pve-manager pve-container qemu-server libpve-guest-common-perl)

DRY_RUN="${PVE_META_DRY_RUN:-0}"

WORKDIR="$(mktemp -d -t pve-meta-watch.XXXXXX)"
trap 'rm -rf "$WORKDIR"' EXIT

log()  { echo "[watch-pve] $*" >&2; }
die()  { echo "[watch-pve] ERROR: $*" >&2; exit 1; }

require_cmd() { command -v "$1" >/dev/null 2>&1 || die "required command '$1' not found"; }

require_cmd curl
require_cmd dpkg-deb
require_cmd dpkg
require_cmd patch
require_cmd awk
require_cmd git

[ -f "$CEILINGS" ] || die "ceilings.toml not found at $CEILINGS"

# --- fetch & parse the upstream Packages index ------------------------------

PACKAGES_FILE="$WORKDIR/Packages"
log "fetching $PACKAGES_URL"
curl -fsSL "$PACKAGES_URL" -o "$PACKAGES_FILE"

get_field() {
    # get_field <packages-file> <package-name> <field-name>
    awk -v pkg="$2" -v field="$3" '
        BEGIN { RS = ""; FS = "\n" }
        {
            hit = 0; value = ""
            for (i = 1; i <= NF; i++) {
                if ($i == "Package: " pkg) hit = 1
                if (index($i, field ": ") == 1) value = substr($i, length(field) + 3)
            }
            if (hit && value != "") { print value; exit }
        }
    ' "$1"
}

get_ceiling() {
    # get_ceiling <package-name>
    awk -F' = ' -v k="$1" '
        $1 == k { v = $2; gsub(/^"|"$/, "", v); print v; exit }
    ' "$CEILINGS"
}

set_ceiling() {
    # set_ceiling <package-name> <new-version>
    local pkg="$1" ver="$2" esc_ver
    esc_ver="$(printf '%s' "$ver" | sed 's/[&/\]/\\&/g')"
    sed -i.bak -E "s/^(${pkg}) = \".*\"\$/\1 = \"${esc_ver}\"/" "$CEILINGS"
    rm -f "$CEILINGS.bak"
}

version_gt() { dpkg --compare-versions "$1" gt "$2"; }

declare -a NEWER_PKGS=()
declare -A NEW_VERSION=()
declare -A NEW_FILENAME=()

for pkg in "${TRACKED_PACKAGES[@]}"; do
    ceiling="$(get_ceiling "$pkg")"
    [ -n "$ceiling" ] || die "no [tested] ceiling recorded for '$pkg' in ceilings.toml"

    upstream_ver="$(get_field "$PACKAGES_FILE" "$pkg" "Version")"
    if [ -z "$upstream_ver" ]; then
        log "warning: '$pkg' not found in the upstream Packages index, skipping"
        continue
    fi

    if version_gt "$upstream_ver" "$ceiling"; then
        log "$pkg: upstream $upstream_ver > tested ceiling $ceiling"
        filename="$(get_field "$PACKAGES_FILE" "$pkg" "Filename")"
        [ -n "$filename" ] || die "$pkg: has a Version but no Filename in the Packages index"
        NEWER_PKGS+=("$pkg")
        NEW_VERSION["$pkg"]="$upstream_ver"
        NEW_FILENAME["$pkg"]="$filename"
    else
        log "$pkg: upstream $upstream_ver <= tested ceiling $ceiling, nothing to do"
    fi
done

if [ ${#NEWER_PKGS[@]} -eq 0 ]; then
    log "all tracked packages are at or below their tested ceiling, nothing to do"
    exit 0
fi

# --- per-package dry run -----------------------------------------------------

check_pve_manager() {
    # UI patch tool's own `verify` already takes explicit file paths, so no
    # --root/wiring changes to pve-manager-patch are needed here; see
    # docs/DISTRIBUTION.md ("Ceiling watcher") for the --root convenience
    # flag proposed for that tool anyway.
    local extracted="$1"
    local lib_js="$extracted/usr/share/pve-manager/js/pvemanagerlib.js"
    local tpl="$extracted/usr/share/pve-manager/index.html.tpl"
    bash "$UI_PATCH_TOOL" verify "$lib_js" "$tpl"
}

check_lifecycle_diffs() {
    # check_lifecycle_diffs <pkg> <extracted>
    local pkg="$1" extracted="$2" perl_root status=0
    perl_root="$extracted/usr/share/perl5"

    shopt -s nullglob
    local diffs=("$LIFECYCLE_DIR/${pkg}_"*.diff)
    shopt -u nullglob

    if [ ${#diffs[@]} -eq 0 ]; then
        echo "no lifecycle diffs registered for $pkg, nothing to dry-run"
        return 0
    fi

    local diff
    for diff in "${diffs[@]}"; do
        echo "--- patch --dry-run -p1 < $(basename "$diff") ---"
        if ! patch --dry-run -p1 -d "$perl_root" < "$diff"; then
            status=1
        fi
    done
    return "$status"
}

declare -A RESULT=()
declare -A OUTPUT=()

for pkg in "${NEWER_PKGS[@]}"; do
    ver="${NEW_VERSION[$pkg]}"
    filename="${NEW_FILENAME[$pkg]}"
    deb_url="$PVE_REPO_BASE/$filename"
    deb_path="$WORKDIR/$pkg.deb"
    extracted="$WORKDIR/extracted/$pkg"
    mkdir -p "$extracted"

    log "downloading $pkg $ver from $deb_url"
    if ! curl -fsSL "$deb_url" -o "$deb_path"; then
        RESULT["$pkg"]="fail"
        OUTPUT["$pkg"]="failed to download $deb_url"
        continue
    fi

    if ! dpkg-deb -x "$deb_path" "$extracted" >"$WORKDIR/$pkg.extract.log" 2>&1; then
        RESULT["$pkg"]="fail"
        OUTPUT["$pkg"]="dpkg-deb -x failed:
$(cat "$WORKDIR/$pkg.extract.log")"
        continue
    fi

    ok=1
    combined=""

    if [ "$pkg" = "pve-manager" ]; then
        if ui_out="$(check_pve_manager "$extracted" 2>&1)"; then
            combined+="$ui_out"$'\n'
        else
            ok=0
            combined+="$ui_out"$'\n'
        fi
    fi

    if lc_out="$(check_lifecycle_diffs "$pkg" "$extracted" 2>&1)"; then
        combined+="$lc_out"$'\n'
    else
        ok=0
        combined+="$lc_out"$'\n'
    fi

    if [ "$ok" -eq 1 ]; then
        RESULT["$pkg"]="pass"
    else
        RESULT["$pkg"]="fail"
    fi
    OUTPUT["$pkg"]="$combined"
    log "$pkg: dry-run ${RESULT[$pkg]}"
done

PASS_PKGS=()
FAIL_PKGS=()
for pkg in "${NEWER_PKGS[@]}"; do
    if [ "${RESULT[$pkg]:-fail}" = "pass" ]; then
        PASS_PKGS+=("$pkg")
    else
        FAIL_PKGS+=("$pkg")
    fi
done

# --- GitHub side effects -----------------------------------------------------

gh_open_exists_with_title() {
    # gh_open_exists_with_title <issue|pr> <exact-title>
    local kind="$1" title="$2" n
    n="$(gh "$kind" list --state open --json title \
        --jq "[.[] | select(.title == \"${title//\"/\\\"}\")] | length" 2>/dev/null || echo 0)"
    [ "${n:-0}" -gt 0 ]
}

ensure_label() {
    gh label create pve-upgrade --color BFD4F2 \
        --description "Upstream Proxmox package ceiling watcher" >/dev/null 2>&1 || true
}

open_ceiling_pr() {
    local pkgs=("$@")
    [ ${#pkgs[@]} -gt 0 ] || return 0

    local title
    title="ceilings: bump $(IFS=,; echo "${pkgs[*]}")"

    if [ "$DRY_RUN" = "1" ] || ! command -v gh >/dev/null 2>&1; then
        log "[dry-run] would bump ceilings and open PR '$title'"
        return 0
    fi

    if gh_open_exists_with_title pr "$title"; then
        log "PR '$title' is already open, skipping"
        return 0
    fi

    local body pkg old new
    body="Automated bump after a clean \`patch --dry-run\`/\`pve-meta-patch verify\` against the newly released package(s) (see \`scripts/watch-pve/check.sh\`)."$'\n\n'
    for pkg in "${pkgs[@]}"; do
        old="$(get_ceiling "$pkg")"
        new="${NEW_VERSION[$pkg]}"
        set_ceiling "$pkg" "$new"
        body+="- **$pkg**: $old -> $new"$'\n'
    done

    if git -C "$REPO_ROOT" diff --quiet -- ceilings.toml; then
        log "no ceiling changes to commit"
        return 0
    fi

    local branch
    branch="ceiling-bump/$(date +%Y%m%d%H%M%S)"
    git -C "$REPO_ROOT" config user.name "pve-meta-watch-bot"
    git -C "$REPO_ROOT" config user.email "actions@users.noreply.github.com"
    git -C "$REPO_ROOT" checkout -b "$branch"
    git -C "$REPO_ROOT" add ceilings.toml
    git -C "$REPO_ROOT" commit -m "$title"
    git -C "$REPO_ROOT" push origin "$branch"

    ensure_label
    printf '%s\n' "$body" | gh pr create --title "$title" --body-file - \
        --label pve-upgrade --base main --head "$branch"
}

open_failure_issue() {
    local pkgs=("$@")
    [ ${#pkgs[@]} -gt 0 ] || return 0

    local title
    title="pve-upgrade: patch dry-run failed for $(IFS=,; echo "${pkgs[*]}")"

    if [ "$DRY_RUN" = "1" ] || ! command -v gh >/dev/null 2>&1; then
        log "[dry-run] would open issue '$title'"
        local pkg
        for pkg in "${pkgs[@]}"; do
            echo "--- $pkg ---"
            echo "${OUTPUT[$pkg]}"
        done
        return 0
    fi

    if gh_open_exists_with_title issue "$title"; then
        log "issue '$title' is already open, skipping"
        return 0
    fi

    ensure_label
    local pkg
    {
        echo "The scheduled Proxmox ceiling watcher found upstream release(s) our"
        echo "patches no longer dry-run cleanly against."
        echo
        for pkg in "${pkgs[@]}"; do
            echo "## $pkg: $(get_ceiling "$pkg") -> ${NEW_VERSION[$pkg]}"
            echo
            echo '```'
            echo "${OUTPUT[$pkg]}"
            echo '```'
            echo
        done
        echo "---"
        echo "<!-- TODO(llm-fix): hand-off point for the planned LLM-assisted"
        echo "     fix flow (docs/VISION.typ, \"Upgrade gating\": a"
        echo "     claude-code-action run that regenerates the failing hunks"
        echo "     against the new upstream source and opens a draft PR for"
        echo "     human review). Not implemented yet -- for now, a human"
        echo "     fixes the patch(es) under pve-manager-patches/lifecycle/"
        echo "     (or pve-manager-patch/ for the UI anchors), re-runs"
        echo "     'PVE_META_DRY_RUN=1 scripts/watch-pve/check.sh' locally to"
        echo "     confirm, and bumps ceilings.toml by hand (the escape"
        echo "     hatch -- see docs/DISTRIBUTION.md). -->"
    } | gh issue create --title "$title" --label pve-upgrade --body-file -
}

open_ceiling_pr "${PASS_PKGS[@]}"
open_failure_issue "${FAIL_PKGS[@]}"

log "done. passed: ${PASS_PKGS[*]:-none}; failed: ${FAIL_PKGS[*]:-none}"

if [ ${#FAIL_PKGS[@]} -gt 0 ]; then
    exit 1
fi
