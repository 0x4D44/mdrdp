# MDR-BUG-FLU-00075 — RDP clipboard work can block graphics decode and input on the sole session thread

- **State:** Fixed
- **Priority:** Should
- **Severity:** High
- **Area:** rdp/clipboard-latency
- **Raised:** 2026-08-23T20:34:25Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T121351Z-af272b56
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00075-run-verify-20260913T121351Z-af272b56
- **Owner base:** 85fd2e05c7be502bc1228cad7b7a1ae7ef2c3b14
- **Owner fingerprint:** sha256:277a73ccb31d1266386cea6a3dd0502c4be1c0e5ad17d0417151670ba1b8b884
- **Owner since:** 2026-09-13T12:13:51Z
- **Owner until:** 2026-09-13T14:13:51Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T20:34:25Z, raised via `deltic bugs new` model=gpt-5.6-sol@high) -> Fixed (2026-08-24T07:57:10Z, deltic:auto role=fix run=fix-20260824T065233Z-095a1cce branch=task/bug-MDR-BUG-FLU-00075-run-fix-20260824T065233Z-095a1cce code=3e5b56f1191abdfedce1b11c0f8f185e859b8766 gate=manual)

## Observation

The RDP session services an unbounded clipboard action drain before reading graphics, performs OS clipboard and image conversion synchronously, and writes clipboard responses through a blocking socket with no write timeout. A slow pasteboard, large image, or peer that stops reading can delay graphics decode and all input indefinitely. Bound clipboard work per turn and isolate or bound its blocking I/O without breaking static-channel ordering.

## Fix

<unfixed — raised only>

## Notes
