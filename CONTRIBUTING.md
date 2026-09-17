# House rules

The system is small. The code should be too.

* **`docs/DESIGN.md` is the contract.** A new rule needs a named accident it prevents and
  a named consumer that needs it. If it has neither, it is not built. Read the non-goals
  before adding a guard.
* **Comments.** One doc line per item, saying what the code cannot. No header essays, no
  history, no rejected alternatives, no restating `DESIGN.md`; cite a section instead. A
  comment names the accident, never an attacker.
* **Refuse, don't handle.** For input nothing of ours could have produced, return an
  error. No second mechanism to cope with it.
* **A rule is tested once**, in the language that implements it. The Perl suite checks
  that values cross the boundary; the JS suite checks the editor. Neither re-proves the
  core. Prefer a table of cases to a function per case.
* **One home per fact.** If two places must agree by hand, one of them calls the other.
* **Decisions** (`docs/decisions/`) record a choice between designs someone might revisit,
  in under 25 lines. A superseded record is deleted; git remembers.
* **Deleting is a feature.** A change that removes a concept removes its code, tests and
  docs together.

Checks: `make test`, `make check`, `make check-perl`, and `make -C crates/pve-meta-perl
check` on Linux.
