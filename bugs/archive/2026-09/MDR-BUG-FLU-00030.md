# MDR-BUG-FLU-00030 — Native session shows two cursors and the remote cursor trails by seconds

- **State:** Closed
- **Priority:** Must
- **Severity:** High
- **Area:** native/input
- **Raised:** 2026-08-21T22:15:35Z
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
- **State history:** Open (2026-08-21T22:15:35Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-21T23:08:11Z, deltic:auto role=fix run=fix-20260821T225003Z-94169e2d branch=task/bug-MDR-BUG-FLU-00030-run-fix-20260821T225003Z-94169e2d code=40ae062 gate=manual) -> Closed (2026-09-13T05:26:31Z, independent verifier: cursor state reached the local platform path and wire framing round-tripped; the hidden-state mutant failed its assertion; model=codex@max)

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

Commit `40ae06287520ab71e53d1b88e2fd3b5395032bac` gives the IDD hardware cursor the
remote ownership path and mirrors host cursor visibility to the native viewer. The
post-fix native session suite passed 64/64, and the server cursor framing regression
passed. The native test confirmed cursor messages reach the platform callback without
painting the desktop.

The independent red check changed the hidden cursor dispatch to `Default`. The selected
test failed its own assertion with `[Default, Default]` instead of `[Hidden, Default]`.
The original duplicate/trailing cursor observation was reviewed against the ownership
path; this pass did not start a new Windows IDD session.

## Notes

Do not treat hiding the local pointer alone as a fix: that would leave the
seconds-late captured pointer as the only feedback.
