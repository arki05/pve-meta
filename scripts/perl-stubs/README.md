# Perl stubs for `perl -c`

Empty stand-ins for the Proxmox VE modules this project's Perl files `use`, so
`perl -I scripts/perl-stubs -c <file>` can run on a machine without PVE
installed -- a laptop, or the plain Debian container CI builds in. Each stub is
a package name plus the symbols the real module exports; nothing is
implemented. `perl -c` compiles a file without running it, so `use` and
`use base` are all it needs satisfied.

On a PVE host, `perl -c` without `-I` uses the real modules and is the better
check; `make check-perl` uses the stubs so the same command works everywhere.
