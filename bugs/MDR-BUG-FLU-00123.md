# MDR-BUG-FLU-00123 — AVC444 animation flickers between 4:2:0 and chroma-refined output

- **State:** Fixed
- **Priority:** Must
- **Severity:** Medium
- **Area:** graphics/avc444
- **Raised:** 2026-08-26T07:01:57Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T060703Z-9ed3a291
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00123-run-verify-20260913T060703Z-9ed3a291
- **Owner base:** 33bebafdace7764bfa69dcd56be9bb719c59834c
- **Owner fingerprint:** sha256:a255fe2683dac245147daf5d6910e11d1c7383aa5f4e784ae52cca6ed1d49c9a
- **Owner since:** 2026-09-13T06:07:03Z
- **Owner until:** 2026-09-13T08:07:03Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-26T07:01:57Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-26T07:26:26Z, deltic:auto role=fix run=fix-20260826T070341Z-002f10ac branch=task/bug-MDR-BUG-FLU-00123-run-fix-20260826T070341Z-002f10ac code=f96fe83 gate=manual)

## Observation

Arthur reports that the earlier persistent chroma degradation is fixed, but animated regions still show a repeatable chroma flicker. The visible shape matches an LC=1 main-view update being presented as required 4:2:0 output, followed by a separately presented LC=2 chroma refinement; a subsequent luma frame then returns the region to 4:2:0. Chroma-only refinement needs presentation debouncing so it settles after motion without delaying fresh luma or retaining stale auxiliary samples.

## Fix

<unfixed — raised only>

## Notes
