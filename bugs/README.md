# Bugs — `bugs/` directory ledger

**Purpose.** Active records are one markdown file per bug directly under `bugs/`,
with filename == bug ID. Closed records from sealed months may instead live under
`bugs/archive/YYYY-MM/<ID>.md`; their modeled headers remain visible through the
top-level `bugs/archive-YYYY-MM.md` manifest. There is **no `BUGS.md` — never
create one.**

**Monthly archive.** `deltic bugs archive` moves only well-formed Closed records
whose final Closed transition predates the current month. It publishes the full
original and a digest-backed manifest row before deleting the hot copy, so an
interrupted run is safe to repeat. `deltic bugs archive --check` validates the
manifest/original bijection without changing files. Do not hand-edit archive
manifests or move records yourself. Normal summaries, filters, ID minting, lookup,
and detail prose include archived records. A later recurrence after closure is a
new regression record; Closed records are not reopened in place.

**ID grammar.** `PREFIX-TYPE-HOST-NNNNN`.
- **PREFIX** — this repo's acronym: `MDR`.
- **TYPE** — `BUG` in this ledger; the sibling `reqs/` ledger mints `REQ`
  (see `reqs/README.md`). Both ledgers share one ID parser, so deltic never
  inspects the token.
- **HOST** — this machine's canonical token: the **first DNS label** of its
  hostname (`<name>.local`, `<name>.home.arpa`, and a bare `<name>` all collapse
  to `<NAME>`), uppercased and reduced to `[A-Z0-9]`. It is derived **in code**
  by `deltic bugs new` — not by a hand-run `hostname`, which would bake the DNS
  domain into the token and split one machine into two. A short 3-letter typing
  code per host lives in deltic's host registry.
- **NNNNN** — a per-host sequence, zero-padded to at least 5 digits, **unified
  across the sibling ledgers**: the next number is `max(NNNNN)+1` over `bugs/`
  **and** `reqs/` **and** `issues/` for this host, so a bare host+number
  (`<HOST>-00042`) maps to exactly one item whatever its type.
- Legacy `PREFIX-NNNNN` ids (pre-conversion) remain valid, verbatim, forever.

**Per-host allocation.** Raise a bug with
**`deltic bugs new --title "<summary>" --discovery-source <SOURCE>`** (`--severity`/`--priority`/`--area`/
`--body` optional; `--json` for tooling). It derives this machine's HOST token,
mints the next number across **all** sibling ledgers for that host, and writes
the record from this binary's own template. A clone-wide OS file reservation
serializes the scan and publish; bugs and reqs never share a number.

**Always mint with the command — never by hand.** A hand-run `max(NNNNN)+1` over
one ledger's glob collides with the sibling ledgers by construction, and a
hand-run `hostname` bakes the DNS domain into the token, splitting one machine
into two (MDK-BUG-KILN-00152). The command derives HOST in code and reads the
whole number space.

**How concurrent minting is made safe (R-SAMEHOST).** The unit of concurrency on
one box is the **worktree**, not the machine — a host runs many task worktrees at
once, each a fork of trunk with its own `bugs/` directory. The number space is per
HOST and every worktree for a host lives on that host's filesystem, so the
allocator reads a **host-wide** high-water: this working tree, every sibling
worktree (`git worktree list`), and every id ever added on any ref. That covers a
peer agent's not-yet-committed id and an id sitting on an unintegrated branch. An
error reading any view aborts the mint rather than silently narrowing it — under-
counting mints a duplicate, while over-counting only skips a number
(MDK-BUG-KILN-00150). The allocator holds an exclusive OS file lock under the
process temp directory, keyed by the clone's canonical common Git directory,
across the complete scan and publish. This keeps the lock shared by every
worktree without requiring write access to Git metadata. Process exit releases
the lock; there is no stale lease to recover (MDK-BUG-KILN-00176,
MDK-BUG-ANVIL-00334).

Two further backstops sit behind the reservation:
- A same-directory race is caught by the write itself: publication is atomic and
  no-clobbering, so the loser gets `AlreadyExists` and retries at the next number
  rather than overwriting the winner. This is a no-replace rename, **not**
  `O_EXCL`, and it compares filenames within **one directory** — it cannot see a
  peer worktree, and it cannot see a bug and a req holding the same number.
- Same number in the same ledger is the same path, so git raises an **add/add
  conflict** at integration — loud, and never a silent overwrite. The resolver
  renumbers.

Same number in **different** sibling ledgers is *not* the same path, so git
merges it silently. The reserved host-wide high-water prevents that class
(MDK-BUG-KILN-00151 and MDK-BUG-KILN-00176); a structural oracle in
`crates/deltic-repos/src/mint.rs` also fails the gate on a new occurrence.

**Timestamps.** Every timestamp deltic writes — `Raised` and each `State
history` transition — is **RFC 3339 UTC to whole seconds**: `YYYY-MM-DDTHH:MM:SSZ`.
Readers also accept a bare `YYYY-MM-DD` — the legacy shape — and those records
keep their day and simply have no recorded time. Ordering is by **day**, so
same-day transitions are read in the order they appear on the line. For an
automated transition the instant is when the *run* began, not the moment the file
was written — precise to the second, but not a measure of write latency.

**States & transitions.** `Open → Blocked → Fixed → Closed`, each dated and
attributed in the append-only `State history:` line. State is a **field inside
the file** (`- **State:** …`) — never rename a file to change its state.

**Two-eyes rule.** A bug moves to `Closed` only after a second pair of eyes
verifies the fix (regression test green, root cause understood).

**Priority vs severity.** `Priority` is fix urgency (`Must` / `Should` /
`Could`). `Severity` is user impact (`Critical` / `High` / `Medium` / `Low`).
Malformed or missing priority is automation-ineligible until corrected.
`Severity` does **not** order the queue: it is an input to choosing the
priority, so ordering by it as well would weight user impact twice. It stays on
the record to inform whoever sets the priority, and for the dashboard and
release notes.

**Pick order.** Automation picks Open bugs by `Priority`, then `Discovery
source`, then oldest `Raised` first. So a customer- or human-reported bug leads
its priority band — responsiveness to the people who reported a problem does not
queue behind agent-generated backlog — but source is a tie-break *within* a
band, never above it. `Priority` is the raiser's own declaration of urgency, so
a human-raised `Could` (an explicit "this can wait") does not displace an
agent-raised `Must`. The verify-and-close queue leads with the attempt counts
instead (fewest `doubt`, then fewest `indeterminate`) so uncontested closures
clear first, and applies the same source order beneath them.

**Discovery source.** `Discovery source` records the earliest evidenced route
into engineering: `Customer`, `Human`, `Agent`, `Automation`, or `Unknown`.
Choose `Customer` only for customer issue intake and record the originating
issue backlink; a later issue backlink alone does not prove customer origin.
Use `Human` for non-customer manual/exploratory reports, `Agent` for model
judgment, and `Automation` for deterministic assertions. Use `Unknown` when the
available evidence does not establish the route. Historical records may omit
the field; records raised from 2026-08-02 must carry it. It ranks the queue in
that same listed order, with `Unknown` and omitted records last (a malformed value
ranks there too). In the Open queue that cannot bury the pre-field backlog, because
`Priority` is the outer key there: a legacy `Must` still outranks every `Should`.
That argument does not carry to the verify-and-close queue, which has no priority
key — there the source rank only orders bugs already equal on attempt counts.
`Agent` ranks above `Automation`: agent-raised defects come from model judgment
during review or testing, while automation-raised ones are deterministic
assertions that arrive with a failing test and stay cheap to pick up whenever
they are reached.

**Ownership.** The current owner lives in the bug file. `Owner role: human`
parks automation indefinitely for a named human owner. `Owner role: fix` and
`Owner role: verify` are leased automation claims; every automation owner must
carry the matching `Owner run`, `Owner host`, `Owner branch`, `Owner base`,
timestamps, and, for verify owners, `Owner fingerprint`. The unowned marker is
the ASCII hyphen `-` in every owner field. Partial, blank, or inconsistent owner
data is treated as malformed and skipped by automation.

**Attempts and parking.** `Attempts: fix=N, doubt=N, indeterminate=N` records
durable unattended retry history. `Held branch` preserves useful fixer work that
needs human follow-up: release it with `deltic bugs resume <ID> --reason <why>`,
which clears the hold and claims the bug in one step, then prints the
`deltic start-task --bug <ID> --base <branch>` that resumes the preserved work.
Nothing deletes a held branch. `Blocked` is left with
`deltic bugs reopen <ID> --reason <why>`. `Verify retry after` temporarily parks indeterminate
verification without blocking future fix attempts; after three indeterminate
passes, automation leaves the bug parked as `Fixed` for human follow-up.
`Legacy fixed run` is set on pre-schema `Fixed` bugs so the verify loop has
explicit provenance.

**Requirement backlink.** `Req` names the sibling `reqs/` requirement this defect
breaches, or `-` when there is none. It is **detail-only**: read for the Repos→Bugs
detail row and nothing else — never resolved, never validated, never a flag, and
never an input to the queue. The checked direction is the reqs side's
`Violated-by:` (req→bug, shape-checked and resolved against `bugs/`); this is the
convenience reverse, so a stale or typo'd value here costs nothing.

**File format.** Each `bugs/<ID>.md`:

```markdown
# MDR-BUG-HOST-00001 — Short title

- **State:** Open
- **Priority:** Should
- **Severity:** High
- **Area:** ui
- **Raised:** YYYY-MM-DDTHH:MM:SSZ
- **Discovery source:** Human
- **Owner:** -
- **Owner role:** -
- **Owner run:** -
- **Owner host:** -
- **Owner branch:** -
- **Owner base:** -
- **Owner fingerprint:** -
- **Owner since:** -
- **Owner until:** -
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **Req:** -
- **State history:** Open (YYYY-MM-DDTHH:MM:SSZ, raised by …) → Fixed (YYYY-MM-DDTHH:MM:SSZ, `sha`)

## Observation
<symptom, repro, expected vs actual>

## Fix
<accepted fix summary and verification notes>

## Notes
<other notes, links, failed attempts>
```

> This ledger was bootstrapped by `deltic bugs init`. Edit it freely — it is your
> repo's own copy, not a fleet-managed cache.

Deltic appends successful `### Fix summary (...)` and `### Verification summary (...)`
sections under `## Fix`. Failed autonomous attempts still append `### Fix attempt
summary (...)` under `## Notes`. Older ledgers may have `### Fix summary (...)`
under `## Notes`; the Repos browser treats that legacy content as Fix prose. The
modeled header fields and `State history` remain the machine-readable ledger truth.

A transition may carry a `model=<id>@<effort>` token in its detail, recording who did
that step: an autonomous fix/verify records the agent family (with the effort deltic
pins for Codex — `model=codex@xhigh`; Claude records `model=claude`), and a raise via
`deltic bugs new --model <id> --effort <tier>` records the precise raising model.
