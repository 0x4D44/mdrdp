# MDR-BUG-FLU-00130 — AVC444 luma and chroma presentations still alternate on coloured terminal content

- **State:** Fixed
- **Priority:** Must
- **Severity:** Medium
- **Area:** graphics/avc444
- **Raised:** 2026-09-14T20:41:13Z
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
- **Attempts:** fix=0, doubt=0, indeterminate=1
- **State history:** Open (2026-09-14T20:41:13Z, raised via `deltic bugs new --land`) -> Fixed (2026-09-14T21:13:36Z, deltic:auto role=fix run=fix-20260914T204351Z-8b2550a0 branch=task/bug-MDR-BUG-FLU-00130-run-fix-20260914T204351Z-8b2550a0 code=1f8674a gate=manual) -> Open (2026-09-14T23:25:45Z, Codex, Arthur reports continued flicker on 0.1.247; successful submissions racing incoming frames leave the luma wait bypassed) -> Fixed (2026-09-14T23:53:12Z, deltic:auto role=fix run=fix-20260914T232621Z-8551b907 branch=task/bug-MDR-BUG-FLU-00130-run-fix-20260914T232621Z-8551b907 code=c3ac64e gate=manual)

## Observation

Arthur reports continuing flicker on bright red terminal text and patterned usage bars in Temper with mdrdp 0.1.245. Metadata traces show ordered luma and chroma updates for the same rectangles; the LC2-only debounce also misses chroma carried by LC0 and permits refinements through unrelated immediate writes. Successor recurrence of closed MDR-BUG-FLU-00123. Implement a bounded luma presentation grace period, release when pending regions receive chroma, and preserve ordered decoding and a hard latency bound.

Evidence fingerprint: `manual:v1:avc444-luma-and-chroma-presentations-still-alte-b649e6ff695aa9a7`


## Fix

### Snapshot acknowledgement correction — 0.1.248

Integrated code `c3ac64e`, version landing `4c61c94`. Ready batch tokens are
captured with snapshots and acknowledged independently of producer activity.
Newer luma retains its own bounded pending coverage/deadline. Failed submissions
remain retryable, timer-only expiry releases work, and lifecycle changes do not
reuse an old acknowledgement token. This supersedes the current-stamp rule below.

Validation: 73 surface, 71 window and 65 graphics tests passed independently;
regressions were observed failing before the fix or under restored mutations.
Clippy, formatting and both Windows type-checks passed, with existing Windows
icon/unused-parameter warnings only. See the 2026.09.15 snapshot acknowledgement
HLD and journal for the exact mutation evidence and limitations.

The delay remains fixed at 50 ms, not adaptive. Ready retries may still include
newer unrefined luma, and chroma arriving after the cap may still be drawn
separately. No live visual improvement is claimed yet. Leave Fixed for an
independent affected-session check; do not treat the policy tests as visual closure.

### Earlier bounded wait — 0.1.247

Integrated in `1f8674a`, version `0.1.247`: hold pending luma for at most 50 ms
from the first update, releasing early on accepted chroma coverage. Preserve
wire-order decoding and bracket unframed bitmap callbacks. Bound region tracking
and prevent deadline rearming before a successful current-stamp presentation.

Focused validation: 73 surface, 65 graphics and 69 window tests passed; all
vendored suites passed. Regression mutations were restored after observed
failures. Clippy (`--all-targets -D warnings`) and Windows cross-check passed.
Existing Windows warnings and standalone vendored formatting drift remain.

Design and full evidence are in the 2026.09.14 bounded AVC444 luma-wait HLD and
journal. This is Fixed, not independently visually verified: a live affected
terminal session still needs to confirm the reported flicker has improved.

## Notes

The reopening date was normalized from the local calendar date to the UTC
timestamp of its ledger commit (`1076555`), preserving transition order.

2026-09-15: Arthur reports: "I'm running .247 and connected with it to temper.
But I still see the chroma flickering." The local startup log confirms that
version, host, 2560x1440 and IOSurface presentation. Metadata at 23:07:49 UTC
shows a deadline release, successful submissions rejected by the policy as
`active_update_or_frame`, then fresh luma published with `wait_us=0`.
`SurfaceStore::acknowledge_presentation` requires the latest producer stamp and
an idle decoder; the ready latch suppresses new region tracking until that
condition holds. This establishes a wait-policy defect, not proof that it is
the only source of visible flicker. Genuine 50 ms expiry also occurs.

## Verification

2026-09-15: Independent run `verify-20260915T064309Z-d1919aa1` checked the
integrated 0.1.249 tree (`38a06b6`) after the adaptive wait landing. Surface,
window, and graphics tests passed 76/76, 71/71, and 65/65; the full root suite
passed 932 tests. Formatting, Clippy, vendored suites, and the Windows
cross-check passed, with only the documented icon and `src/present.rs`
unused-parameter warnings. The adaptive regression
`adaptive_wait_learns_chroma_arriving_after_timeout_and_ack` passed; replacing
the learned delay with the old fixed 50 ms delay failed its late-chroma
assertion, then restoration was clean.

The 0.1.249 release binary attempted the original Temper observation but the
connection timed out before session startup. No screenshot, metrics, or live
visual evidence is claimed. The claim was released; this remains Fixed and
indeterminate pending a reachable affected Temper session.
