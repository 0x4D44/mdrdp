# MDR-BUG-FLU-00084 — Deploy retry without a running Rhydra agent loses the retained 5K/200% policy

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** deploy/display
- **Raised:** 2026-08-23T22:19:13Z
- **Discovery source:** Agent
- **Owner:** deltic:manual
- **Owner role:** fix
- **Owner run:** fix-20260824T233341Z-1ba54fdd
- **Owner host:** flux
- **Owner branch:** task/bug-MDR-BUG-FLU-00084-run-fix-20260824T233341Z-1ba54fdd
- **Owner base:** da3ff62ee6c864588f1a6ce0c7cc8d09c86f5993
- **Owner fingerprint:** -
- **Owner since:** 2026-08-24T23:33:41Z
- **Owner until:** 2026-08-25T01:33:41Z
- **Verify retry after:** -
- **Held branch:** -
- **Legacy fixed run:** -
- **Attempts:** fix=0, doubt=0, indeterminate=0
- **State history:** Open (2026-08-23T22:19:13Z, raised via `deltic bugs new` model=gpt-5.6-sol@xhigh)

## Observation

After the failed same-version deploy left RhydraAgent stopped on Quench, retrying the v0.1.163 force deploy had no live agent status from which to recover the desired display. It silently installed defaults of 2560x1440 at 100% although the host was already configured for 5120x2880 at 200%, then failed its display check. Expected: a deploy retry preserves the last installed display policy even when the agent is stopped, rather than depending only on a live status probe. This is a distinct no-agent recurrence beyond fixed MDR-BUG-FLU-00032.

## Fix

<unfixed — raised only>

## Notes
