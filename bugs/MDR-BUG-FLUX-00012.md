# MDR-BUG-FLUX-00012 — Native sessions publish no stats samples: latency, decode, present and frame_gap are all empty in the metrics report

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** native-transport
- **Raised:** 2026-08-19T11:18:36Z
- **Discovery source:** Agent
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
- **State history:** Open (2026-08-19T11:18:36Z, raised via `deltic bugs new` model=claude-fable-5@high)

## Observation

A native (rhydra) session writes a --metrics-json report whose latency, decode, present and frame_gap blocks all read sample_count 0 with every percentile null, and whose session block reads frames 0 / bytes_in 0, even when the session demonstrably decoded frames (the run epilogue's own 'native: frames N bytes in M' line disagrees with the report in the same run). The report is RDP-shaped: only the RDP path feeds those collectors, and the native path keeps its counters separately for the epilogue.

Consequence: none of mdrdp's visibility surfaces cover the native transport. That is squarely product requirement 4 (no visibility - no cache stats, no codec mix, no way to tell why it feels slow), and it is what blocked tranche 3's AC8 latency-sanity glass run: there is no instrumented typing round-trip on the native path to quote, so no figure can be compared against the spike viewer's 48.6 ms p50 baseline.

Related gap in the same area: the connect stage feed stops at 'handshake'. There is no first-decoded-frame stage event on the native path, so connect-to-first-pixel cannot be timed directly and had to be bounded indirectly by varying --duration. A first-frame stage would serve the tranche 4 health ladder as well as this.

## Fix

<unfixed — raised only>

## Notes
