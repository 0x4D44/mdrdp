# MDR-BUG-FLU-00036 — 5K full-motion playback starves Rhydra mouse and keyboard handling

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** rhydra/latency
- **Raised:** 2026-08-22T18:08:59Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T051556Z-4993dab9
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00036-run-verify-20260913T051556Z-4993dab9
- **Owner base:** 1b2f42476f88b0103cdd420ffd018fff6c8579b4
- **Owner fingerprint:** sha256:9162ce83996621347ed93a7f621790548197d6874d1a5dec19df4fb77f5ab1f8
- **Owner since:** 2026-09-13T05:15:56Z
- **Owner until:** 2026-09-13T07:15:56Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-22T18:08:59Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-22T18:52:50Z, deltic:auto role=fix run=fix-20260822T183358Z-2efabf47 branch=task/bug-MDR-BUG-FLU-00036-run-fix-20260822T183358Z-2efabf47 code=91ee578 gate=manual)

## Observation

On Quench at 5120x2880 and 200% scale, interactive mouse and keyboard response becomes seconds late while YouTube plays full-motion video. The native viewer currently performs full-frame CPU conversion and presentation work on the macOS event-loop thread, which also receives user input; the server path must also be checked for unbounded transport backlog. Input must remain responsive under sustained full-motion 5K content.

## Fix

<unfixed — raised only>

## Notes
