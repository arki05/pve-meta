# 017 — `data` on a read is a native structure, in PVE's spelling

**Status:** accepted (2026-09-11). Closes the question left open in 006 and 007.

## Context

A write's `data` parameter is a JSON string, decoded once in Rust, because that is what
a REST parameter is. A read's `data` is a native structure, because that is what a PVE
API response is. On the way out a native structure loses exactly one thing: booleans,
which Perl has none of and PVE's encoder renders as `1`/`0`. (Key order is also lost,
but it is not a value, 007.) The store tolerates that spelling in the three places it
arrives: a selector's `all: 1`, an access answer's `read: 1`, and a stored boolean
written as `1` against a `type: boolean` schema.

Returning `data` as a JSON string instead would carry booleans exactly and let those
three tolerances go.

## Decision

`data` stays a native structure. `1`/`0` is how every PVE API spells a boolean
(`onboot: 1`), so an operator that reads pve-meta reads it the way it reads `qm config`;
an escaped JSON document inside a `pvesh` response would be the one place in PVE that
does not. `format=yaml` is the exact-typed read for a client that needs `true` and
`false`, and it is what the editor uses. The three tolerances stay, as the cost of
speaking the platform's dialect.

## Consequences

A schema's `type: boolean` accepts `1`/`0` and `true`/`false` alike, in the editor and
under `enforce`. A client that wants exact types asks for YAML.
