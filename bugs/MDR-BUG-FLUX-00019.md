# MDR-BUG-FLUX-00019 — RDP sessions get no audio channel from any test host: mdrdp requests rdpsnd but the server never opens it

- **State:** Open
- **Priority:** Should
- **Severity:** Medium
- **Area:** audio/rdp
- **Raised:** 2026-08-19T21:20:19Z
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
- **State history:** Open (2026-08-19T21:20:19Z, raised via `deltic bugs new` model=claude-opus-5@high)

## Observation

mdrdp advertises rdpsnd among its static channels (src/connect.rs:219) and sets wants_audio from it (src/connect.rs:432), but every measured RDP session ends with 'audio: no audio channel was opened by the server this session'.

Measured 2026-08-19 with mdrdp v0.1.93, --rdp --foreground:
  - quench.lan.example (user ano), 32 s session, 42 frames: no audio channel.
  - temper.lan.example (user user@example.com), 20 s session, 456 frames: no audio channel.

Both sessions were otherwise healthy -- AVC444v2 video throughout, graceful shutdown, zero decode errors. Arthur reports that RDP audio demonstrably works on BOTH hosts with Microsoft's Windows App (a YouTube video), which is what makes this a defect rather than a host limitation: the reference client gets audio from the same two boxes that give mdrdp none.

Audio output (RDPSND) is explicitly in scope per CLAUDE.md, so a session that silently opens no audio channel is a shipped feature that does not work.

WHAT IS ESTABLISHED: mdrdp gets no audio channel from either host, on repeated measurement.

WHAT IS NOT YET SEPARATED, and should be the first step of any fix:
  1. Whether the reference client and mdrdp were compared against the same host in the same state. Arthur's observation and mine were not simultaneous.
  2. Whether the servers declined because they had nothing to redirect. quench's console session has NO active audio render endpoint (see 'wrk_docs/2026.08.19 - PARK - tranche 6 audio needs a render endpoint on the host.md'), and both mdrdp sessions RECONNECTED an already-logged-on session rather than creating a fresh one -- a reconnected session may keep its original endpoint-less audio state, whereas a fresh logon gets a per-session virtual Remote Audio endpoint.
  3. Whether mdrdp requests audio redirection in the way the server honours -- advertising the static channel may not be sufficient on its own.

The cheap discriminator is to connect with Windows App 11.3.8 and with mdrdp against the same host minutes apart, and compare whether an audio channel opens. That needs a hands-off Mac and has not been run.

This is deliberately raised as a record rather than investigated further: establishing the cause needs the reference-client comparison, and the drain owns the fix.

## Fix

<unfixed — raised only>

## Notes
