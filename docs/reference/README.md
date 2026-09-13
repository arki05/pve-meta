# Reference notes on upstream Proxmox

Notes taken from reading upstream source while pve-meta was being designed. They
are input, not design: what Proxmox does, cited by file and line at the revisions
that were current when they were written (September 2026). Nothing keeps them in
step with upstream, and nothing in the build reads them.

* `PROXMOX-CONVENTIONS.md` — how Proxmox writes Rust, Perl, JavaScript and Debian
  packaging, so pve-meta could adopt the house style rather than invent one. The
  packaging sections (§7, §8) are what `debian/rules` and both Makefiles cite.
* `PDM-DESIGN-LANGUAGE.md` — how Proxmox Datacenter Manager composes pages in its
  Rust/Yew UI. Written for a rebuild of the editor on that stack, which decision 012
  then rejected in favour of plain ExtJS; kept as the record of what was studied.

What pve-meta itself decided is `../DESIGN.md` and `../decisions/`.
