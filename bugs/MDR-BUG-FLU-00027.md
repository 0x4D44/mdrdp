# MDR-BUG-FLU-00027 — 5K tiled H.264 decodes its first frame then rejects nearly every post-renegotiation access unit

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native-transport/h264
- **Raised:** 2026-08-20T19:02:37Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T051503Z-bad3aa28
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00027-run-verify-20260913T051503Z-bad3aa28
- **Owner base:** 154370746c612475688d6e434ccd77b68c64ce32
- **Owner fingerprint:** sha256:3d7430518b7f4275c0f08ced67d4a4ae1143621f24dea31539b134ec9f94dda7
- **Owner since:** 2026-09-13T05:15:03Z
- **Owner until:** 2026-09-13T07:15:03Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-20T19:02:37Z, raised via `deltic bugs new`) -> Fixed (2026-08-20T19:44:44Z, deltic:auto role=fix run=fix-20260820T190256Z-28ceb536 branch=task/bug-MDR-BUG-FLU-00027-run-fix-20260820T190256Z-28ceb536 code=1f246e5 gate=manual)

## Observation

After installing the 5K IDD copy-deadline fix on quench, an integrated mdrdp v0.1.111 30-second 5120x2880@240 run received 1,668,530 video bytes but painted only 4 frames and counted 284 tile decode errors. The server emitted 146 complete two-tile sequences with zero queue drops and zero timestamp mismatches; frame 1 was an IDR for each tile at config epoch 0, then both encoders changed to config epoch 1 at frame 2 and emitted only non-IDR access units. Expected: both 2560x2880 H.264 tile streams continue decoding after encoder output-type renegotiation, with zero decode errors.

## Fix

<unfixed — raised only>

## Notes
