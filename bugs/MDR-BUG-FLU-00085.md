# MDR-BUG-FLU-00085 — Deploy's legacy Rhydra uninstaller does not stop the current Windows service

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** deploy/rhydra
- **Raised:** 2026-08-23T22:19:13Z
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
- **State history:** Open (2026-08-23T22:19:13Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

On Quench during a same-version v0.1.163 force deploy, the pre-copy uninstaller selected the legacy flat C:\mdrdp\rhydra-agent.exe. It acknowledged worker shutdown but did not know about the installed RhydraAgent service, which immediately relaunched the worker; deploy then failed its quiescence check before copying. Expected: deploy stops the installed service through the current agent or SCM before replacing artifacts, regardless of which legacy binary path is present.

## Fix

<unfixed — raised only>

## Notes
