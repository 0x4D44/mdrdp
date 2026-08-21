# MDR-BUG-FLU-00030 — Native session shows two cursors and the remote cursor trails by seconds

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** native/input
- **Raised:** 2026-08-21T22:15:35Z
- **Discovery source:** Human
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260821T225003Z-94169e2d
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00030-run-fix-20260821T225003Z-94169e2d
- **Owner base:** efc8049188fe8e65b170e77b3b63aabecd3f1de6
- **Owner fingerprint:** -
- **Owner since:** 2026-08-21T22:50:03Z
- **Owner until:** 2026-08-22T00:50:03Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-21T22:15:35Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

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
