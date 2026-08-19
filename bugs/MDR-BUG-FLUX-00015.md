# MDR-BUG-FLUX-00015 — Every fleet test host is headless, so display-transition faults are structurally unreachable in testing

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** testing
- **Raised:** 2026-08-19T16:10:39Z
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
- **State history:** Open (2026-08-19T16:10:39Z, raised via `deltic bugs new` model=claude-opus-5@high)

## Observation

All four fleet test hosts are headless — no monitor attached. kiln, the machine that
exposed MDR-BUG-FLUX-00008, is the only one with a real screen, and it is not a test
host; it is a laptop Arthur uses.

A machine with an attached panel produces a class of display-mode transition that a
headless box physically cannot: idle display power-off, backlight changes, lid open and
close, dock and undock. Each of those makes the RDP server send ResetGraphics
mid-stream, which was the trigger for FLUX-00008 — a fault that made the entire remote
desktop undecodable and went undiagnosed for a full day.

The consequence is not that we lacked a test for that bug. It is that no test we could
have written on this fleet would have caught it. Eight suppress/resume cycles driven
against kiln at 1280x720, and a matched run against quench, both came back completely
clean (0 decode errors) while a real machine was black, because a headless host never
generates the trigger.

`powercfg SUB_VIDEO VIDEOIDLE` values measured across the fleet 2026-08-19 — kiln 600 s,
quench never, temper never, crucible 900 s — looked like a promising correlation and
were partly a red herring for exactly this reason: crucible carries a timeout but has no
panel for it to act on.

Expected: the fleet can exercise display-mode transitions on at least one host, so this
class of fault is reachable before a user finds it.

Fix directions, in rough order of cost:
1. Attach a display (or an HDMI/DP dummy plug, which presents as a real monitor) to one
   fleet host and give it a short display-idle timeout.
2. Failing that, drive mode changes programmatically on a host that has an indirect
   display driver — the repo already ships one (`mdrdp-idd`, staged on quench) — and
   check whether adding/removing/resizing that display reproduces the ResetGraphics
   sequence a physical panel produces.
3. At minimum, record in TESTING-GUIDE.md that the fleet cannot cover this, so the next
   person does not read a green sweep as coverage it does not have.

## Fix

<unfixed — raised only>

## Notes
