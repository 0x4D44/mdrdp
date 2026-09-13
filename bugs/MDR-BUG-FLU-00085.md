# MDR-BUG-FLU-00085 — Deploy's legacy Rhydra uninstaller does not stop the current Windows service

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** deploy/rhydra
- **Raised:** 2026-08-23T22:19:13Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T133307Z-8ca9d852
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00085-run-verify-20260913T133307Z-8ca9d852
- **Owner base:** 10a5285aacd5e07a1bc3a371dcb61cc79a9d1ed5
- **Owner fingerprint:** sha256:af6a0dd4500add529bd7e65c1f1e6206c98a6dc4fd58ef1e755105205396b8e0
- **Owner since:** 2026-09-13T13:33:07Z
- **Owner until:** 2026-09-13T15:33:07Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:19:13Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T23:54:48Z, deltic:auto role=fix run=fix-20260824T234549Z-b5e9422b branch=task/bug-MDR-BUG-FLU-00085-run-fix-20260824T234549Z-b5e9422b code=7ed0437ec2c5c513c70ca2b9a3a7638bfe7093c1 gate=manual)

## Observation

On Quench during a same-version v0.1.163 force deploy, the pre-copy uninstaller selected the legacy flat C:\mdrdp\rhydra-agent.exe. It acknowledged worker shutdown but did not know about the installed RhydraAgent service, which immediately relaunched the worker; deploy then failed its quiescence check before copying. Expected: deploy stops the installed service through the current agent or SCM before replacing artifacts, regardless of which legacy binary path is present.

## Fix

<unfixed — raised only>

## Notes
