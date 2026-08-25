# MDR-BUG-FLUX-00017 — rhydra health ladder: a disconnected agent session reports as a missing IDD device, pointing the operator at the wrong remedy

- **State:** Fixed
- **Priority:** Should
- **Severity:** Medium
- **Area:** rhydra/agent
- **Raised:** 2026-08-19T16:57:07Z
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
- **State history:** Open (2026-08-19T16:57:07Z, raised via `deltic bugs new` model=claude-fable-5@high) -> Fixed (2026-08-25T00:09:07Z, deltic:auto role=fix run=fix-20260824T235840Z-9b6d5b75 branch=task/bug-MDR-BUG-FLUX-00017-run-fix-20260824T235840Z-9b6d5b75 code=4794a4e gate=manual)

## Observation

The `device` rung reports Fail when the agent's own Windows session is merely
**disconnected**, which is indistinguishable in the report from a genuinely
absent IDD device. The two have completely different remedies, and the ladder
points at the one that cannot work.

### Observed, 2026-08-19 on quench

`mdrdp quench.lan.example --doctor` reported:

```
  device         FAIL
  pool           FAIL      the capture server is attached to generation 1, but the
                           driver now publishes 2: it is reading a section nothing writes to
  display-mode   FAIL
  server         FAIL      the process is up but nothing is listening on the video port yet
  input-desktop  unknown   OpenInputDesktop failed (Access is denied. (0x80070005)) — this may
                           mean the secure desktop is up, or simply that the agent is not in
                           the console session
```

The IDD driver was in fact **present and healthy** the whole time:

```
Status : OK
Class  : Display
FriendlyName : mdrdp latency-spike display
InstanceId   : SWD\MDRDP_IDD\MDRDP_IDD
```

The actual fault was session topology — `query session` showed the console as
session 4 with **no user**, while `ano`'s session 2 was `Disc`, and
`rhydra-agent` was running in session 2:

```
 SESSIONNAME    USERNAME    ID  STATE
>services                    0  Disc
                ano          2  Disc
 console                     4  Conn
```

A disconnected session has no attached display, so the agent's
`EnumDisplayDevices` walk found nothing and reported the device gone. Running
the rig's existing `mdrdp-tocon` task (which `tscon`s session 2 onto the
console) took every rung to `ok` **in one step**, with no device work at all.

### Why this matters

The remedies are opposites, and the ladder recommends the wrong one:

- Genuinely absent device → `cycle-device`, which tears down and rebuilds the
  display and drops every session on the host.
- Disconnected agent session → `tscon`, which is instant and disturbs nothing.

Acting on this report, I fired `cycle-device` on evidence that did not support
it. It happened to clear a real *second* fault (the stranded pool generation),
but it could never have fixed the device rung, and against a live session it
would have been destructive for no reason.

### The clue was present but ranked below the cause

`input-desktop` already says "or simply that the agent is not in the console
session" — the correct diagnosis, in the report, two rungs *below* the symptom.
The ladder is ordered by bring-up dependency, so the operator reads `device
FAIL` first and stops there.

### Suggested direction (not a decision)

The agent can tell these apart cheaply: it knows its own session id
(`ProcessIdToSessionId`) and can compare it with `WTSGetActiveConsoleSessionId`.
When they differ, the honest report for `device` is `Unknown` with "the agent's
session is not attached to the console; displays are invisible from here",
rather than `Fail` — the tranche-4 distinction between "I could not tell" and
"it is broken", applied to a case that was missed. A `session` rung *above*
`device` would be the natural home, since every rung below it is unreadable
when it is red.

## Fix

<unfixed — raised only>

## Notes
