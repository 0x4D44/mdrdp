# MDR-BUG-FLU-00027 — 5K tiled H.264 decodes its first frame then rejects nearly every post-renegotiation access unit

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native-transport/h264
- **Raised:** 2026-08-20T19:02:37Z
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
- **State history:** Open (2026-08-20T19:02:37Z, raised via `deltic bugs new`) -> Fixed (2026-08-20T19:44:44Z, deltic:auto role=fix run=fix-20260820T190256Z-28ceb536 branch=task/bug-MDR-BUG-FLU-00027-run-fix-20260820T190256Z-28ceb536 code=1f246e5 gate=manual) -> Closed (2026-09-13T05:26:31Z, independent verifier: tiled reference-chain regression passed and the early-return mutant failed its decoder-call assertion; model=codex@max)

## Observation

After installing the 5K IDD copy-deadline fix on quench, an integrated mdrdp v0.1.111 30-second 5120x2880@240 run received 1,668,530 video bytes but painted only 4 frames and counted 284 tile decode errors. The server emitted 146 complete two-tile sequences with zero queue drops and zero timestamp mismatches; frame 1 was an IDR for each tile at config epoch 0, then both encoders changed to config epoch 1 at frame 2 and emitted only non-IDR access units. Expected: both 2560x2880 H.264 tile streams continue decoding after encoder output-type renegotiation, with zero decode errors.

## Fix

Commit `1f246e57e46410c32b889f9f0c7e398020b2fc1c` decodes every tiled access unit before
deciding whether its pixels are redundant, preserving both H.264 decoder reference
chains through rect-covered sequences. The post-fix native session suite passed 64/64,
including `rect_completed_tile_aus_still_advance_both_decoder_reference_chains`.

The independent red check restored the old early return for suppressed tiles. That test
then selected one test and failed its decoder-call assertion (`left: 1`, `right: 2`).
The original 5120x2880 renegotiation observation was reviewed against this exact staged
tile path; this pass did not start a new live Quench session.

## Notes
