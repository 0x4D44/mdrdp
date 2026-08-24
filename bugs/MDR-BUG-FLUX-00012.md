# MDR-BUG-FLUX-00012 — Native sessions publish no stats samples: latency, decode, present and frame_gap are all empty in the metrics report

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** native-transport
- **Raised:** 2026-08-19T11:18:36Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T083231Z-1af22f05
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLUX-00012-run-fix-20260824T083231Z-1af22f05
- **Owner base:** 5765c0a0780415ea49f37873d0fbaaf366e62faa
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T08:32:31Z
- **Owner until:** 2026-08-24T10:32:31Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-19T11:18:36Z, raised via `deltic bugs new` model=claude-fable-5@high)

## Observation

A native (rhydra) session writes a --metrics-json report whose latency, decode, present and frame_gap blocks all read sample_count 0 with every percentile null, and whose session block reads frames 0 / bytes_in 0, even when the session demonstrably decoded frames (the run epilogue's own 'native: frames N bytes in M' line disagrees with the report in the same run). The report is RDP-shaped: only the RDP path feeds those collectors, and the native path keeps its counters separately for the epilogue.

Consequence: none of mdrdp's visibility surfaces cover the native transport. That is squarely product requirement 4 (no visibility - no cache stats, no codec mix, no way to tell why it feels slow), and it is what blocked tranche 3's AC8 latency-sanity glass run: there is no instrumented typing round-trip on the native path to quote, so no figure can be compared against the spike viewer's 48.6 ms p50 baseline.

Related gap in the same area: the connect stage feed stops at 'handshake'. There is no first-decoded-frame stage event on the native path, so connect-to-first-pixel cannot be timed directly and had to be bounded indirectly by varying --duration. A first-frame stage would serve the tranche 4 health ladder as well as this.

## Fix

<unfixed — raised only>

## Notes

### 2026-08-20 — Root cause, and the same defect on a second surface

Arthur reported `mdrdp -S` showing a native session with FRAMES 1 and a blank P50
beside RX 7.8 MB. Same defect as this ticket, seen through `--sessions` instead of
`--metrics-json`: only the RDP path feeds the shared collectors.

Evidence, from the live presence file of a 5h42m native session
(`~/Library/Application Support/mdrdp/sessions/46248.json`, mdrdp 0.1.95):

```
"frames": 1, "bytes_in": 8295888, "latency_p50_us": null,
"frame_gap_p50_us": 60011134, "codecs": {}
```

**Two causes, not three.**

1. **FRAMES 1 — the rects path painted without counting.** `NativeSink::on_au` and
   `NativeSink::paint` each kept their own stats block, and `paint`'s omitted
   `s.frames += 1` (it did update `bytes_in`, `codec_painted` and `mark_painted`).
   A native session receives one AU keyframe and then rect deltas forever, so the
   count froze at 1 while 8.3 MB of rects arrived — ~346 updates at ~24 KB each.
   Neither path fed `s.codecs` either, so the HUD's codec line read
   "no surface updates yet" for the life of every native session.

2. **Blank P50 — nothing on the native path ever recorded `s.latency`.** The
   input→paint round-trip proxy existed only in the RDP loop (`src/session.rs:295`
   stamps on send, `:525` records on the next paint). The native session sends input
   on `native-input` and paints on `native-net`, so a thread-local stamp could not
   work and none was written.

**FPS 0 is not a defect.** `frame_gap_p50_us` was 60,011,134 µs — the host's idle
cadence really is one update per minute, so 1e6/gap rounds to 0. The arithmetic is
honest; the column grades an unfiltered median and so paints an idle session red.
Logged separately in `scratchpad.md` (2026-08-20) as a presentation question, not a
counter that is wrong.

The presence writer and the sink share one `StatsHandle` (`src/main.rs:1637`), and
the file refreshes normally — the earlier suspicion of a stale file or a second
handle is disproved by the recorded frame gaps, which only `mark_painted` can write.

### Fix, landed on this branch

`src/native/session.rs`:

- One `NativeSink::record_paint(bytes, generation, decode_us)` now folds a painted
  frame into the collectors — frame count, bytes, codec tally, codec bytes, decode
  sample (AU path only), the input round trip, and `mark_painted`. Both paint paths
  call it. The duplication that let the two blocks drift is gone.
- New `InputClock` (`Arc<Mutex<Option<Instant>>>`), stamped by `pump_input` after any
  lap that put records on the wire and taken by the next paint. Only the first
  unanswered input starts it, matching the RDP loop's `get_or_insert_with`.

Not affected, and verified working on the native path: `frame_gap` and `present`.
`mark_painted` already ran on both paint paths, and `mark_presented` is called from
the shared renderer (`src/window.rs:1887`), which is transport-agnostic.

Still open in this ticket after this change: the `decode` block carries roughly one
sample per native session, because only the AU path decodes and a session receives
one keyframe — a real number, but not a distribution; and the connect stage feed
still has no first-decoded-frame event, so connect-to-first-pixel cannot be timed
directly.

## Live validation, 2026-08-20

Confirmed against a real rhydra session on quench (mdrdp v0.1.99, release build,
45 s native session with scripted typing so both input and repaints flowed):

```
before:  FPS 0   FRAMES 1     RX 7.8 MB   P50 -
after:   FPS 8   FRAMES 116   RX 1.0 MB   P50 82.0ms
```

The row is now internally consistent — 116 frames against 1.0 MB is ~9 KB per
frame, where the old row claimed one frame for 7.8 MB, and that inconsistency was
what identified the defect in the first place. The session epilogue agrees
exactly: `frames 116  bytes in 1049864`.

This closes the caveat the fix was committed with (unit tests only, no live
host). Reported by Arthur against `mdrdp -S`.

