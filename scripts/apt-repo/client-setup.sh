#!/usr/bin/env bash
# scripts/apt-repo/client-setup.sh
#
# Run this as root on a PVE host to add the pve-meta apt repository.
#
# Usage:
#   client-setup.sh [--url https://apt.example.com] [--private] \
#                    [--private-user USER] [--private-pass PASS] [--no-update]
#
# See docs/DISTRIBUTION.md for what "--private" gates and how credentials
# for it are issued.

set -euo pipefail

BASE_URL="${PVE_META_APT_URL:-https://apt.example.com}"
PRIVATE=0
PRIVATE_USER=""
PRIVATE_PASS=""
DO_UPDATE=1
SUITE="trixie"
COMPONENT="main"

usage() {
    cat <<EOF
Usage: $(basename "$0") [options]

  --url URL          Base URL of the public apt repo (default: $BASE_URL,
                      or \$PVE_META_APT_URL). The private prefix is assumed
                      to live at URL/private.
  --private           Also configure the private prefix: adds a second
                      stanza to the .sources file and writes
                      /etc/apt/auth.conf.d/pve-meta.conf.
  --private-user U    Basic-auth username for the private prefix (a
                      placeholder is written if omitted -- edit the file
                      afterwards).
  --private-pass P    Basic-auth password for the private prefix (same
                      placeholder behavior as --private-user).
  --no-update         Skip running 'apt update' at the end.
  -h, --help          This help.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --url) BASE_URL="$2"; shift 2 ;;
        --private) PRIVATE=1; shift ;;
        --private-user) PRIVATE_USER="$2"; shift 2 ;;
        --private-pass) PRIVATE_PASS="$2"; shift 2 ;;
        --no-update) DO_UPDATE=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 1 ;;
    esac
done

BASE_URL="${BASE_URL%/}"

if [ "$(id -u)" -ne 0 ]; then
    echo "this script must be run as root (writes /etc/apt/...)" >&2
    exit 1
fi

echo "==> installing keyring from $BASE_URL/pve-meta.asc"
install -d -m 0755 /etc/apt/keyrings
curl -fsSL "$BASE_URL/pve-meta.asc" -o /etc/apt/keyrings/pve-meta.asc
chmod 0644 /etc/apt/keyrings/pve-meta.asc

echo "==> writing /etc/apt/sources.list.d/pve-meta.sources"
{
    echo "Types: deb"
    echo "URIs: $BASE_URL"
    echo "Suites: $SUITE"
    echo "Components: $COMPONENT"
    echo "Signed-By: /etc/apt/keyrings/pve-meta.asc"
} > /etc/apt/sources.list.d/pve-meta.sources

if [ "$PRIVATE" -eq 1 ]; then
    echo "==> appending the private-prefix stanza"
    {
        echo ""
        echo "Types: deb"
        echo "URIs: $BASE_URL/private"
        echo "Suites: $SUITE"
        echo "Components: $COMPONENT"
        echo "Signed-By: /etc/apt/keyrings/pve-meta.asc"
    } >> /etc/apt/sources.list.d/pve-meta.sources

    echo "==> writing /etc/apt/auth.conf.d/pve-meta.conf"
    install -d -m 0755 /etc/apt/auth.conf.d
    host_and_path="$(printf '%s' "$BASE_URL/private" | sed -E 's#^[a-zA-Z]+://##')"
    {
        echo "# Basic-auth credentials for the pve-meta private apt prefix,"
        echo "# checked by a Cloudflare Worker in front of the R2 bucket."
        echo "# See docs/DISTRIBUTION.md (\"Private prefix\") for how"
        echo "# credentials are issued. Replace the placeholders below."
        echo "machine ${host_and_path}"
        echo "login ${PRIVATE_USER:-CHANGEME}"
        echo "password ${PRIVATE_PASS:-CHANGEME}"
    } > /etc/apt/auth.conf.d/pve-meta.conf
    chmod 0600 /etc/apt/auth.conf.d/pve-meta.conf
fi

if [ "$DO_UPDATE" -eq 1 ]; then
    echo "==> apt update"
    apt update
fi

echo "==> done"
