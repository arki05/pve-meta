#!/bin/bash
# Exercises a running pve-metad over HTTPS with curl.
#
# Required:
#   PVE_META_URL      e.g. https://10.10.10.154:8007
# Auth (one of):
#   PVE_META_TOKEN     Authorization: PVEAPIToken=user@realm!tokenid=secret
#   PVE_META_TICKET + PVE_META_CSRF   PVEAuthCookie ticket + matching CSRFPreventionToken
#
# Uses vmid 999999 (>= 999000, per the environment's test-document convention) and cleans it
# up on exit. Requires curl and jq. Bash (not POSIX sh) for its header-array handling.
#
# Usage: PVE_META_URL=https://host:8007 PVE_META_TICKET=... PVE_META_CSRF=... ./scripts/smoke.sh

set -eu

: "${PVE_META_URL:?set PVE_META_URL, e.g. https://10.10.10.154:8007}"
VMID=999999
BASE="$PVE_META_URL/api2/json/meta"
FAIL=0

AUTH_HEADERS=()
WRITE_HEADERS=()

if [ -n "${PVE_META_TOKEN:-}" ]; then
	AUTH_HEADERS=(-H "Authorization: PVEAPIToken=${PVE_META_TOKEN}")
elif [ -n "${PVE_META_TICKET:-}" ]; then
	AUTH_HEADERS=(-H "Cookie: PVEAuthCookie=${PVE_META_TICKET}")
else
	echo "set PVE_META_TOKEN or PVE_META_TICKET (+PVE_META_CSRF for writes)" >&2
	exit 1
fi
WRITE_HEADERS=("${AUTH_HEADERS[@]}")
if [ -n "${PVE_META_CSRF:-}" ]; then
	WRITE_HEADERS+=(-H "CSRFPreventionToken: ${PVE_META_CSRF}")
fi

# req METHOD PATH [extra curl args...] -- prints body, then a line with just the HTTP code.
req() {
	local method="$1" path="$2"
	shift 2
	curl -sk -w '\n%{http_code}' -X "$method" "$BASE$path" "$@"
}

step() { echo "== $1 =="; }

check_code() {
	local body="$1" code="$2" want="$3" label="$4"
	if [ "$code" != "$want" ]; then
		echo "FAIL ($label): expected HTTP $want, got $code: $body" >&2
		FAIL=1
		return 1
	fi
	return 0
}

cleanup() {
	set +e
	out=$(req DELETE "/guests/$VMID" "${WRITE_HEADERS[@]}")
	code=$(echo "$out" | tail -n1)
	if [ "$code" != "200" ] && [ "$code" != "404" ]; then
		echo "warning: cleanup DELETE returned $code" >&2
	fi
}
trap cleanup EXIT

step "version"
out=$(req GET "/version" "${AUTH_HEADERS[@]}")
code=$(echo "$out" | tail -n1); body=$(echo "$out" | sed '$d')
check_code "$body" "$code" 200 version
echo "$body" | jq -e '.data.token' >/dev/null
echo "$body"

step "health"
out=$(req GET "/health")
code=$(echo "$out" | tail -n1); body=$(echo "$out" | sed '$d')
check_code "$body" "$code" 200 health
echo "$body" | jq -e '.data.store.root' >/dev/null
echo "$body"

step "create (PUT patch, vmid=$VMID)"
patch_body=$(jq -n --arg stamp "$(date +%s)" '{patch:{smoketest:{stamp:$stamp}}}')
out=$(req PUT "/guests/$VMID" "${WRITE_HEADERS[@]}" -H 'Content-Type: application/json' -d "$patch_body")
code=$(echo "$out" | tail -n1); body=$(echo "$out" | sed '$d')
check_code "$body" "$code" 200 create
digest=$(echo "$body" | jq -r '.data.digest')
echo "created, digest=$digest"

step "patch (digest-checked update)"
patch2=$(jq -n --arg d "$digest" '{patch:{smoketest:{extra:"ok"}}, digest:$d}')
out=$(req PUT "/guests/$VMID" "${WRITE_HEADERS[@]}" -H 'Content-Type: application/json' -d "$patch2")
code=$(echo "$out" | tail -n1); body=$(echo "$out" | sed '$d')
check_code "$body" "$code" 200 patch
digest=$(echo "$body" | jq -r '.data.digest')

step "get"
out=$(req GET "/guests/$VMID" "${AUTH_HEADERS[@]}")
code=$(echo "$out" | tail -n1); body=$(echo "$out" | sed '$d')
check_code "$body" "$code" 200 get
echo "$body" | jq -e '.data.data.smoketest.extra == "ok"' >/dev/null
echo "$body"

step "raw"
out=$(req GET "/guests/$VMID?raw=1" "${AUTH_HEADERS[@]}")
code=$(echo "$out" | tail -n1); body=$(echo "$out" | sed '$d')
check_code "$body" "$code" 200 raw
echo "$body" | jq -e '.data.raw | length > 0' >/dev/null

step "convert"
conv=$(jq -n --arg d "$digest" '{format:"toml", digest:$d}')
out=$(req POST "/guests/$VMID/convert" "${WRITE_HEADERS[@]}" -H 'Content-Type: application/json' -d "$conv")
code=$(echo "$out" | tail -n1); body=$(echo "$out" | sed '$d')
check_code "$body" "$code" 200 convert
echo "$body" | jq -e '.data.format == "toml"' >/dev/null

step "delete"
out=$(req DELETE "/guests/$VMID" "${WRITE_HEADERS[@]}")
code=$(echo "$out" | tail -n1); body=$(echo "$out" | sed '$d')
check_code "$body" "$code" 200 delete
trap - EXIT

if [ "$FAIL" -eq 0 ]; then
	echo "ALL OK"
else
	echo "SMOKE TEST FAILED" >&2
	exit 1
fi
