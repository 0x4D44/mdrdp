# MDR-BUG-FLU-00030 — Native session shows two cursors and the remote cursor trails by seconds

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** native/input
- **Raised:** 2026-08-21T22:15:35Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T051530Z-ddb1b3a4
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00030-run-verify-20260913T051530Z-ddb1b3a4
- **Owner base:** 42f92e14383e7b972a4c56d64c3b80fb58d8f622
- **Owner fingerprint:** sha256:e1d03e77c3906f855dd6cf5f23d19d47c92ed8f1ad2c68571f6825a5fba88bc5
- **Owner since:** 2026-09-13T05:15:30Z
- **Owner until:** 2026-09-13T07:15:30Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-21T22:15:35Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-21T23:08:11Z, deltic:auto role=fix run=fix-20260821T225003Z-94169e2d branch=task/bug-MDR-BUG-FLU-00030-run-fix-20260821T225003Z-94169e2d code=40ae062 gate=manual)

## Observation

On 2026-08-21 Arthur connected from Flux to Quench with mdrdp v0.1.114 in
native fullscreen mode at 5120x2880. Both the local macOS pointer and a pointer
inside the captured Windows desktop were visible. The captured pointer trailed
the local pointer by several seconds. A native session should show one
responsive pointer whose hotspot matches the coordinates sent to Windows.

The Rhydra native transport carries no pointer-shape channel. The local window
therefore keeps its default pointer while the IDD capture currently includes
Windows pointer motion in desktop frames. Diagnosis must establish which end
owns the cursor before hiding either copy.

## Fix

<unfixed — raised only>

## Notes

Do not treat hiding the local pointer alone as a fix: that would leave the
seconds-late captured pointer as the only feedback.
