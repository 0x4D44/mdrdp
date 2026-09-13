# MDR-BUG-FLUX-00001 — Audio ring drops burst waves during playback (overruns) leaving gaps in sound

- **State:** Closed
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
- **State history:** Open (2026-08-16T10:16:35Z, raised via `deltic bugs new` model=claude-fable-5@high) -> Fixed (2026-08-16T17:35:01Z, deltic:auto role=fix run=fix-20260816T172955Z-p90591-n740395000-c1 branch=task/bug-MDR-BUG-FLUX-00001-run-fix-20260816T172955Z-p90591-n740395000-c1 code=2dd6b6a gate=manual) -> Closed (2026-09-13T06:27:20Z, 0x4D44/Codex verify run=verify-20260913T061845Z-b933a768)

## Observation

Playing a YouTube video on quench through the dynamic RDPSND channel, src/audio.rs AudioRing recorded 1880 overruns alongside 7577 underruns in one 120 s session (metrics audio.overruns / audio.underruns). An overrun is incoming wave data dropped because the ring is full, so every one is an audible gap; the server sends audio in bursts faster than real time and the ring (RING_BUFFER_MS) cannot absorb them. Reproduce with mdrdp quench --user ano --password-stdin --input-script <script that opens a YouTube video in Edge> --metrics-json out.json and read audio.overruns; it should be zero. Likely fix territory: size the ring to the negotiated format's burst depth or apply backpressure instead of dropping.

## Fix

Commit `2dd6b6a27bb4e795355f7e09eeb4b8a665048370` increases the shared audio ring capacity from 200 ms to 400 ms and counts one underrun per dry episode. The direct regression `audio::tests::an_underrun_episode_counts_once_however_long_the_dry_spell_lasts` and the capacity regression `audio::tests::native_ring_policy_is_enabled_only_by_native_constructor` each passed after restoration; the latter confirms that 201 samples fit the 400 ms ring at the fixture rate. As a behavioral red check, changing `RING_BUFFER_MS` back to 200 made that capacity regression fail its own assertion (`left: 200`, `right: 201`); the source was restored and its diff is empty. `cargo build --locked`, `cargo test --locked` (908 library, 17 binary, 8 integration tests), `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, `./scripts/check-windows.sh --locked`, and `./scripts/test-vendored.sh` (372 vendored tests) all passed. No fresh Quench playback session was available to remeasure the original 120-second overrun report.

## Notes
