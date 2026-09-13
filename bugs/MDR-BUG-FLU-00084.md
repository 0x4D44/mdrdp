# MDR-BUG-FLU-00084 — Deploy retry without a running Rhydra agent loses the retained 5K/200% policy

- **State:** Fixed
- **Priority:** Must
- **Severity:** High
- **Area:** deploy/display
- **Raised:** 2026-08-23T22:19:13Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** verify
- **Owner run:** verify-20260913T133254Z-e33dc7c9
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00084-run-verify-20260913T133254Z-e33dc7c9
- **Owner base:** b7ae0ad188d0efe6fbc70dae74ef212244f4d1a7
- **Owner fingerprint:** sha256:5f667216174cc138e193f935c5d00d4a94ca621494f162931d2234770ef0bbd6
- **Owner since:** 2026-09-13T13:32:54Z
- **Owner until:** 2026-09-13T15:32:54Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:19:13Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh) -> Fixed (2026-08-24T23:45:30Z, deltic:auto role=fix run=fix-20260824T233341Z-1ba54fdd branch=task/bug-MDR-BUG-FLU-00084-run-fix-20260824T233341Z-1ba54fdd code=db1b0ae8bd7527393db73cc19b63243d31ef73ab gate=manual)

## Observation

After the failed same-version deploy left RhydraAgent stopped on Quench, retrying the v0.1.163 force deploy had no live agent status from which to recover the desired display. It silently installed defaults of 2560x1440 at 100% although the host was already configured for 5120x2880 at 200%, then failed its display check. Expected: a deploy retry preserves the last installed display policy even when the agent is stopped, rather than depending only on a live status probe. This is a distinct no-agent recurrence beyond fixed MDR-BUG-FLU-00032.

## Fix

<unfixed — raised only>

## Notes
