# Requirements — `reqs/` directory ledger

**Purpose.** A requirement is a **living spec**: a durable, attributed statement
of required behaviour plus a re-runnable **oracle** that proves it. One markdown
file per requirement under `reqs/`, the filename == the requirement ID; deltic
assembles the ledger in memory by globbing `reqs/*.md` and surfaces it as the F3
Repos→Reqs pill. There is **no master table** by design, and **no `REQS.md` —
never create one.** This mirrors the `bugs/` ledger (see `bugs/README.md`); the
difference is that a requirement never auto-closes — once `Satisfied` it can flip
to `Violated` and back, so the ledger answers *"what must this system always do,
and is it still doing it?"*.

**ID grammar.** `MDR-REQ-HOST-NNNNN`.
- **MDR** — this repo's acronym.
- **REQ** — the type token (the bugs ledger uses `BUG`; `REQ` is reserved for
  this ledger). deltic never inspects the token — both ledgers share one ID
  parser.
- **HOST** — this machine's canonical token: the **first DNS label** of its
  hostname (`<name>.local`, `<name>.home.arpa`, and a bare `<name>` all collapse
  to `<NAME>`), uppercased and reduced to `[A-Z0-9]`. It is derived **in code**
  by `deltic reqs new` — not by a hand-run `hostname`, which would bake the DNS
  domain into the token and split one machine into two. A short 3-letter typing
  code per host lives in deltic's host registry.
- **NNNNN** — a per-host sequence, zero-padded to at least 5 digits, **unified
  across the sibling ledgers**: the next number is `max(NNNNN)+1` over `reqs/`
  **and** `bugs/` **and** `issues/` for this host, so a bare host+number
  (`<HOST>-00042`) maps to exactly one item whatever its type.
- Legacy `MDR-NNNNN` ids (pre-conversion) remain valid, verbatim, forever.

**Per-host allocation.** Raise a requirement with
**`deltic reqs new --title "<summary>"`** (`--discovery-source`/`--priority`/`--flow`/
`--area`/`--body` optional; `--json` for tooling). It derives this machine's HOST token,
mints the next number across **all** sibling ledgers for that host, and writes
the Draft record from this binary's own template. A clone-wide OS file
reservation serializes the scan and publish; reqs and bugs never share a number.

**Merge safety (R-SAMEHOST).** The filename **stem is the identity**. Two actors
sharing a HOST token (worktrees on one box, a Win+WSL pair with the same
hostname) used to mint the same id routinely, because the high-water was read
from the working tree alone. `deltic reqs new` now allocates from a **host-wide**
high-water — this working tree, every sibling worktree, and every id ever added
on any ref — so a number held by a peer worktree or an unintegrated branch is
never re-issued. Should two ids still meet in the **same** ledger, that is the
same path, so it surfaces **loudly** as an add/add conflict at integration and
the resolver renumbers.

Across **different** sibling ledgers the same number is a different path, so git
merges it silently and nothing flags it — the allocator is the only thing
preventing that class. Always raise through `deltic reqs new`; never hand-glob
`reqs/` for `max+1`, which collides with the sibling ledgers by construction. A
structural oracle (`crates/deltic-repos/src/mint.rs`) fails the gate on a new
occurrence.

If a file's H1 id disagrees with its filename, deltic keys the record by the
**filename** and carries a "differs from filename" warning in the ledger note.

**States & transitions.**

```
Draft ──(triage)──► Accepted ◄──(unblock)── Blocked
                       │  └───(block)──────────┘
                       ├─(land --code <sha>)─► Integrated ──(accept)─► Satisfied ⇄ Violated
                       │                          │
                       │                          └─(human REJECT: git revert + verb)─► Accepted
                       └─ Implemented (LEGACY — parsed, accepted-from, never minted) ─► Satisfied / Violated
any non-Retired ──(retire)──► Retired (terminal)
```

| State | Meaning |
|---|---|
| **Draft** | Proposed, under discussion. |
| **Accepted** | Binding, not yet met (outstanding). |
| **Blocked** | Triaged and attempted, parked on an open question (reachable only from `Accepted`; `unblock` returns it there). Not `Draft` — that buries "attempted" in the untriaged pile; not `Accepted` — that would re-enter the ready queue. |
| **Implemented** | LEGACY — code present but landed outside the pipeline, oracle unverified at landing. Still parsed and accepted-from; no verb mints it (a manual build now lands `Integrated` via `deltic reqs land`). |
| **Integrated** | Code **and** oracle on `origin/main` — landed by the light auto-land or by a verb-fenced manual `deltic reqs land` — **awaiting batch acceptance**. |
| **Satisfied** | Code **and** a complete oracle, **accepted per the repo's `reqs_accept` policy** (default: a human; a second pair of eyes read the source and re-ran the oracle). |
| **Violated** | Built then breached — the alarm. Reachable from `Satisfied` *or* directly from `Implemented`. |
| **Retired** | Withdrawn or superseded (terminal). |

State is a **field inside the file** (`- **State:** …`) — never rename a file to
change its state. Transitions are append-only and dated in the `State history:`
line. deltic's *reader* only *classifies* the declared state (a case-insensitive
contains-scan, `retired → violated → blocked → integrated → satisfied →
implemented → accepted → draft`); it never derives or enforces a transition. The
`deltic reqs` **verbs** are the sanctioned writers (below); they always write
bare state labels. An unrecognised state word classifies as `Other` so it
surfaces in the counts rather than vanishing.

**Verbs.** Every mechanical transition is a `deltic reqs` verb — a fenced,
atomic, docs-lane edit at `origin/<trunk>`, mirroring `deltic bugs`: `triage`
(Draft→Accepted; sets Priority/Flow/oracle/design; `--from <decisions.toml>`
batches many decisions into ONE commit), `block` / `unblock`
(Accepted ⇄ Blocked, the build's return path for an open question), `claim` /
`release` / `land --code <sha>` (the manual build lane — Accepted→Integrated,
under a 2h `deltic:manual` owner lease), `accept` (Integrated/Violated→Satisfied;
policy-gated, batchable), `violate --bug <BUG-ID>` (Satisfied/Implemented→
Violated), and `retire`. The human spends the decision; the verb does the
markdown.

**Field set.** Modeled fields are the `- **Field:**` bullets **above the first
`## ` heading**; everything from the first level-2 heading on is free prose (it
may quote a `- **State:** …` line without poisoning the record).

| Label | Meaning |
|---|---|
| `State` | One of the seven states above. |
| `Priority` | MoSCoW: `Must` / `Should` / `Could` (an unknown/blank cell → `—`). |
| `Area` | Free text (the module/feature the requirement governs). |
| `Raised` | When first raised (else the first `State history` timestamp). |
| `Discovery source` | Who asked for it: `Customer` / `Human` / `Agent` / `Automation` / `Unknown` (absent, `—`, or unrecognised → *not recorded*). Record-only; see "Discovery source" below. |
| `Implemented-by` | req→code refs — see traceability. |
| `Satisfied-by` | req→oracle refs — see traceability. |
| `Violated-by` | req→bug refs (ledger IDs) — see traceability. |
| `Depends-on` | req→req prerequisite ids — this req is not drain-ready until each has been **integrated** onto trunk. See "Dependency gating". |
| `Design` | req→design-doc ref (`wrk_docs/….md`), else `—`. A **gate input** like `Satisfied-by`: for a `Flow: heavy` req, the build `claim` refuses until the referenced doc's **trunk** copy carries a `**Status:** … SIGNED-OFF` line (a branch-local doc never satisfies the gate). |
| `Surfaces` | Coarse area tags this req is expected to touch (comma-separated; `—`/blank → none). A **soft** contention signal — see "Contention signals". |
| `Hotspots` | Specific conflict-prone file paths this req is expected to edit (comma-separated; `—`/blank → none). A **strong-avoid** contention signal — see "Contention signals". |
| `Flow` | `light` / `heavy` — which build flow the `/loop` runner uses (blank/unknown → `—`). |
| `Claimed-by` | The loop's in-flight marker while a runner holds it, else `—`. A `box:<host> run:<run>` value is a **deltic-written machine claim** (cleared automatically with the owner block); human claims stay free text. |
| `Owner`, `Owner role`, `Owner run/host/branch/base/since/until` | deltic's **cross-site lease** (box exclusion): principal, claimed role (`build` claims an Accepted req, `accept` an Integrated or Violated one, else `-`), run id, host, build branch, trunk base SHA, and the lease window — renewed on a ~45-min lease while building (2h for a manual claim). All `-` = unowned; a partially-filled block reads as malformed (`Owner role` is advisory and exempt — a legacy file has no such line). deltic-written; humans never edit these. |
| `Auto attempts` | Durable failed-auto-build counter, deltic-written; the auto-land pick parks a req at 3. An absent legacy field reads as 0; a present malformed value fails closed as `u32::MAX`, adds an automation-blocking note and parks the req. |
| `State history` | Timestamped, append-only transitions. |

**Discovery source.** Who *asked for* the requirement — the same field, and the
same five values, as the bug ledger's `Discovery source`. It is not the same as
who typed it: an agent running `deltic reqs new` on your behalf is still
`Human`.
Choose `Customer` only for a requirement traceable to customer intake, `Human`
for one a person asked for, `Agent` for one a model proposed from its own
judgement, and `Automation` for one a deterministic process emitted. Use
`Unknown` when you genuinely cannot tell — that is a recorded answer, and it is
distinguishable from having said nothing.

The field is **record-only and never diagnosed**. Unlike the bug ledger's copy,
there is no adoption-date rule and no note for a bad value,
because in this ledger a parser note makes a record ineligible for automation —
so diagnosing provenance would silently pull requirements out of the drain. An
unrecognised value therefore degrades quietly to *not recorded*. The one
exception is structural, not value-based: a **duplicated** `- **Discovery source:**` line
is a malformed header and is rejected like any other duplicated schema field.

`--discovery-source` is optional, so a stale `reqs-capture` skill still mints
successfully — it just records nothing.

**Timestamps.** Every timestamp deltic writes — `Raised` and each `State history`
transition — is **RFC 3339 UTC to whole seconds**: `YYYY-MM-DDTHH:MM:SSZ`. Readers
also accept a bare `YYYY-MM-DD` — the legacy shape, which the worked examples below
still use — and those records keep their day and simply have no
recorded time. Ordering is by **day**, so same-day transitions are read in the order
they appear on the line. For an
automated transition the instant is when the *run* began, not the moment the file
was written — precise to the second, but not a measure of write latency.

**Traceability (three labelled lines).** Multiple refs are comma-separated on the
one line; empty items are dropped; an empty value or `—` / `-` means "none yet".

- `Implemented-by:` — **req→code**. Free-text path/symbol refs (displayed, not
  resolved).
- `Satisfied-by:` — **req→oracle**. Free-text test path / command refs. Its
  **non-emptiness is the honesty check's signal**. When automation is genuinely
  impractical, name a documented re-runnable manual check prefixed `manual:`.
- `Violated-by:` — **req→bug**. Ledger IDs. deltic shape-checks each, then
  resolves it against the sibling `bugs/` directory (dangling / stale-alarm).

**Dependency gating.** `Depends-on:` names other requirement ids (comma-separated;
`—`/blank → none) that must land **before** this one. deltic resolves each against
the ledger and treats the req as **not ready** — it drops out of `deltic reqs
--ready` and the auto-drain — while any dependency is unmet. A dependency is **met**
only when its id resolves to **exactly one** record that has been **integrated onto
trunk**: state `Integrated` (landed via the pipeline, awaiting acceptance) or
`Satisfied` (accepted), and that record is honesty-clean and unambiguous.
`Implemented` does **not** clear a dependency — the manual/heavy path is "built" but
not yet "integrated", so a dependent could overtake it in the integration queue.
`Draft`/`Accepted`/`Violated`/`Retired`, a missing/typo'd id, or a duplicate id all
leave the dependency unmet. It gates **readiness only** — it never changes a req's
state — and checks only **direct** dependencies (a chain clears as each link
integrates). An **`Accepted`** req blocked this way is named in the ledger note
(a `Draft` or terminal req is not — the `unmet_deps` fact still shows per-req in
`--json`). Ids may be bare or link/emphasis-wrapped (`[ID](#…)`, `**ID**`), like
`Violated-by`.

**Contention signals (advisory).** `Surfaces:` and `Hotspots:` declare, in advance,
what a requirement is expected to touch, so the auto-land pick can avoid handing two
concurrently-building reqs the same files. Both are comma-separated, author-owned,
and `—`/blank means "none declared".

- `Surfaces:` — coarse area tags (`review-loop`, `versioning`). A **soft** signal.
- `Hotspots:` — specific conflict-prone file paths (`crates/foo/src/bar.rs`). A
  **strong-avoid** signal.

The pick compares a candidate against the frontier already held by in-flight
claims (any req carrying `Claimed-by` or a live owner lease, on **any** box) and
orders candidates by contention tier: sharing a held **hotspot** sorts last,
sharing only a held **surface** next, disjoint first — the oldest-first tie-break
running unchanged inside each tier. Spelling variations are normalised before
comparison.

Both are **advisory and starvation-safe**: contention only *deprioritises*, so a
colliding req is still picked when it is the only claimable work. Neither gates
readiness, and neither is ever validated — an omitted or wrong value costs a
merge conflict, never a stalled queue.

**Honesty rule + two-eyes process.** deltic **mechanically flags** a requirement
marked `Satisfied` with an **empty `Satisfied-by`** (no oracle). It does not run
the oracle, so the human gate on truth is the **two-eyes rule**: a requirement
moves `Implemented → Satisfied` only after a **second pair of eyes** confirms the
named oracle genuinely exists and passes.

**File format.** Each `reqs/<ID>.md`:

```markdown
# MDR-REQ-HOST-00001 — Short imperative requirement title

- **State:** Satisfied
- **Priority:** Must
- **Area:** <module/feature>
- **Raised:** 2026-01-01
- **Discovery source:** Human
- **Implemented-by:** src/foo.rs
- **Satisfied-by:** tests/foo.rs::proves_it
- **Violated-by:** —
- **Depends-on:** —
- **Design:** —
- **Surfaces:** —
- **Hotspots:** —
- **Flow:** light
- **Claimed-by:** —
- **Owner:** -
- **Owner role:** -
- **Owner run:** -
- **Owner host:** -
- **Owner branch:** -
- **Owner base:** -
- **Owner since:** -
- **Owner until:** -
- **Auto attempts:** 0
- **State history:** Draft (2026-01-01) → Accepted (2026-01-01) → Implemented (2026-01-01, `sha`) → Satisfied (2026-01-02, verified)

## Statement
The system must <durable behavioural spec — not a task>.

## Notes
<design links, the oracle command, history of breaches — free prose; NOT parsed>
```

**The `/loop` runner & the auto-land flow.** The reqs ledger doubles as the
**work queue** for the `/loop` runner. The human gates *batches*, not every diff:

- **Gate 1 (human, batch triage).** Review `Draft` reqs; set `Priority`, `Flow`,
  and — the acceptance oracle — a `Satisfied-by` the build will be proven against,
  then move to `Accepted` (or `Retired` for one-shot tasks). This is the cull.
- **The loop drains *ready* reqs** — `deltic reqs --ready` = `Accepted`, a
  routable `Flow` (not `—`), and **unclaimed**:
  - **`Flow: light`** → the runner builds it **oracle-first** (a cross-family
    agent authors the failing `tests/<oracle>` blind to the impl; a build agent
    makes it green editing `src/` only) and **deltic composes the commit, gates
    it, and fast-forward-pushes the green pair to `origin/main`**, moving the req
    `Accepted → Integrated`. The agents never push — deltic owns git.
  - **`Flow: heavy`** → the runner reports it to the human and skips.
- **Gate 2 (batch acceptance).** The verifier reads the landed source + oracle
  and re-runs it against clean `origin/<trunk>`. On confirmation, run
  **`deltic reqs accept <ID>...`** — one docs-lane commit for the whole batch.
  Who may accept is the repo's **`reqs_accept`** policy in `.deltic-models.toml`,
  read at trunk: `"human"` (the default when absent — the verb then requires
  `--human <name>` as the recorded attestation), `"light-agent"` (a
  different-actor agent may accept `Flow: light` reqs under a live
  `claim --role accept`; heavy stays human), or `"agent"`. The verb refuses an
  accept whose claim run or host matches the landing's (no self-acceptance), and
  refuses any accept with an empty `Satisfied-by` — the honesty rule, mechanical
  at last.
  To reject, `git revert` every commit carrying
  the req's `Req-Id` trailer (`git log --grep='Req-Id: <ID>'` is the audit path),
  then return the req to `Accepted` with an explaining note.

**The `Accepted → Integrated` ledger edit — deltic-authored.** The build branch
is **forbidden to touch `reqs/`**: the landing fence rejects any branch whose
diff includes the ledger, so an agent can never rewrite the file that carries
the lease. After the code lands (pre-landing rebase + guarded ff-push), **deltic
itself authors the terminal flip** as a deltic-owned commit at origin:

- set `- **State:** Integrated`;
- append `→ Integrated (<YYYY-MM-DDTHH:MM:SSZ>)` to the existing `- **State history:**`
  line (the dated-transition shape this ledger parses);
- fill `- **Implemented-by:**` from the build report;
- clear the `Owner*` block and the box `Claimed-by` marker.

deltic likewise writes the lease edits at origin: **claim** (before the build —
the `Owner*` block plus the box `Claimed-by` marker), **renew** (`Owner until`
bumps on the ~45-min lease while building), and **release** (on failure or skip;
a genuine build failure also increments `Auto attempts`, and the pick parks a
req at 3 attempts).

A `Claimed-by:` value of the form `box:<host> run:<run>` is that deltic-written
machine claim — cleared automatically together with the owner block. Human
free-text `Claimed-by` values are never touched by automation.

The write split, in short: **deltic owns the machine fields** — the `Owner*`
block, `Auto attempts`, the box `Claimed-by`, and the terminal
`Accepted → Integrated` flip. **Humans own the decisions** — triage to
`Accepted`, batch acceptance to `Satisfied`, `Retired`.

> This ledger was bootstrapped by `deltic reqs init`. Edit it freely — it is your
> repo's own copy, not a fleet-managed cache.
