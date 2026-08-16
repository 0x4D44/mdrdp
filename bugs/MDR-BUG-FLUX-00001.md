# MDR-BUG-FLUX-00001 — Audio ring drops burst waves during playback (overruns) leaving gaps in sound

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** audio
- **Raised:** 2026-08-16T10:16:35Z
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
- **State history:** Open (2026-08-16T10:16:35Z, raised via `deltic bugs new` model=claude-fable-5@high) -> Fixed (2026-08-16T17:35:01Z, deltic:auto role=fix run=fix-20260816T172955Z-p90591-n740395000-c1 branch=task/bug-MDR-BUG-FLUX-00001-run-fix-20260816T172955Z-p90591-n740395000-c1 code=2dd6b6a gate=manual)

## Observation

Playing a YouTube video on quench through the dynamic RDPSND channel, src/audio.rs AudioRing recorded 1880 overruns alongside 7577 underruns in one 120 s session (metrics audio.overruns / audio.underruns). An overrun is incoming wave data dropped because the ring is full, so every one is an audible gap; the server sends audio in bursts faster than real time and the ring (RING_BUFFER_MS) cannot absorb them. Reproduce with mdrdp quench --user ano --password-stdin --input-script <script that opens a YouTube video in Edge> --metrics-json out.json and read audio.overruns; it should be zero. Likely fix territory: size the ring to the negotiated format's burst depth or apply backpressure instead of dropping.

## Fix

<unfixed — raised only>

## Notes
