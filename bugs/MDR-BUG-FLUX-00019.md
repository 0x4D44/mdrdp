# MDR-BUG-FLUX-00019 — RDP sessions get no audio channel from any test host: mdrdp requests rdpsnd but the server never opens it

- **State:** Fixed
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
- **State history:** Open (2026-08-19T21:20:19Z, raised via `deltic bugs new` model=claude-opus-5@high); Fixed (2026-08-20T10:50:51Z, manual land on task/20260820-TSK-HUM-fix-rdp-audio-channel-negotiation, model=claude-opus-5@high)

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

## Root cause

**The premise was wrong, and the report is what made it look right.** mdrdp's RDP audio
path works. Measured on quench 2026-08-20 with a build of this branch:

```
  audio: 215 packets at 44100 Hz/2ch, 0 dropped to overrun, 10 underruns
```

Two facts, both measured, settle it:

1. **The server does open the audio channel — on every session.** With
   `MDRDP_LOG=ironrdp_dvc=debug`, quench sends `DYNVC_CREATE_REQ` for
   `AUDIO_PLAYBACK_DVC` during connect, mdrdp accepts it (`CreationStatus(0)`), the
   server closes it after desktop setup and immediately opens it again. Both opens are
   accepted. It also offers `AUDIO_PLAYBACK_LOSSY_DVC`, which we decline — as FreeRDP
   does.
2. **Windows starts the RDPSND handshake only once something in the session is actually
   rendering audio.** With an idle remote desktop the channel sits open and completely
   silent: no Server Audio Formats PDU, so no client reply, so no format list and no
   waves. Play a `.wav` inside the session and the handshake and the audio arrive within
   a second. `audio-probe.exe` run *inside* the RDP session confirms the session-scoped
   endpoint exists throughout: `1 render endpoint(s) … 1 ACTIVE … mix format 44100 Hz,
   2 ch` — 44100/2 being exactly the format the waves then arrived in.

**The client defect this exposed** is in the epilogue, `src/main.rs`. It matched on
`(current_format, negotiated_formats)` only, and printed `no audio channel was opened by
the server this session` whenever the format exchange had not happened. Channel opening
was never measured, so the line asserted a server-side refusal that never occurred. That
is the same class of error the code immediately above it already warns about — "calling
that 'negotiated no format' blamed a stage that had not been measured" — one level up,
and it is precisely requirement 4 (no visibility). It cost two sessions of investigation
and this ticket.

**The FreeRDP comparison in the original diagnosis does not say what it was read to say.**
`[dynamic] Loaded mac backend for rdpsnd` is emitted from `rdpsnd_on_open`, i.e. when the
DVC is created — not when audio formats arrive. Run side by side against quench minutes
apart, FreeRDP 3.27.1 with `/sound` behaves identically to mdrdp: it opens
`AUDIO_PLAYBACK_DVC`, declines the lossy variant, and then receives nothing on an idle
desktop. There was no client-side difference to find.

Ruled out along the way, each by inspection or measurement: the `INFO_NOAUDIOPLAYBACK`
polarity (`vendor/ironrdp-connector/src/connection.rs:926` sets the flag only when
`enable_audio_playback` is false, and `src/connect.rs` sets that from `rdpsnd.is_some()`);
the static-channel registration (`channels: rdpdr, cliprdr, drdynvc, rdpsnd` on every
run); host policy (`fDisableAudioCapture=0`, `AudioEnumeratorDll=rdpendp.dll`, no
Terminal Services policy disabling redirection); and a silent decode failure of the format
PDU (that path returns `Err` from `ActiveStage::process` and ends the session `Failed` —
every session ended `Graceful`). The lead in the original report — `src/connect.rs:219` —
is a display-only label list (`KNOWN_CHANNELS`), not the channel registration.

## Fix

Measure the channel open, and report only measured stages.

- `src/audio.rs`: `AudioStats` gains `dvc_opens`, incremented in
  `DynamicRdpsndListener::create` — the moment the DYNVC create is accepted, before any
  audio PDU has to arrive.
- `src/audio.rs`: new `session_summary(&AudioStats) -> String` owns the epilogue wording,
  with a branch for "channel open, handshake never started" that names the real reason
  (nothing was playing) instead of blaming the server. The honest "no audio channel was
  opened" line survives for the case where `dvc_opens == 0`.
- `src/main.rs`: the epilogue calls it instead of matching inline.

No change to negotiation, channel registration, or the wire — there was nothing wrong
with them.

**Live validation, quench, this branch's release build:**

- idle desktop — `audio: channel opened 2 time(s), but the server never started the
  format exchange — nothing was playing on the remote desktop`
- `.wav` playing in the session — `audio: 215 packets at 44100 Hz/2ch, 0 dropped to
  overrun, 10 underruns`

## Notes

- **Do not measure RDP audio against an idle desktop.** Anything that checks audio
  negotiation must make the remote session render sound first, or it measures nothing.
  A scheduled task registered `/ru <user> /it` runs inside the interactive session and is
  a reliable way to do it over ssh; typed input via `--input-script` did not land here.
- The `wrk_docs/2026.08.19 - PARK` doc's console-session finding still stands and is
  unrelated: the console session has no render endpoint, an RDP session gets one.
- 10-12 underrun *episodes* across ~45 s of continuous playback is a real quality
  question, but a different one — it is the ring/jitter policy, not negotiation. Not
  raised here; worth a look if audio quality is ever assessed.

## Discriminator run, 2026-08-20 — SUPERSEDED, see Root cause above

**Kept for the record, but its conclusion is wrong.** Re-run side by side the same
day: FreeRDP receives no audio formats from an idle quench either. The
`[dynamic] Loaded mac backend for rdpsnd` line it hangs on is emitted from
`rdpsnd_on_open`, when the DVC is created — not when audio formats arrive — so it is
not evidence that FreeRDP negotiated anything. mdrdp accepts the same DVC on the same
sessions; it just did not say so. Read the Root cause section, not this one.

The ticket asked for a comparison against another client on the same host. Run
with **FreeRDP** rather than Windows App, because it is scriptable and needs no
GUI, which is what had blocked this for two days.

`sdl-freerdp /v:quench.lan.example /u:ano /sound /cert:ignore /from-stdin:force`,
against quench, minutes after an mdrdp run against the same host:

```
[dvcman_load_addin]: Loading Dynamic Virtual Channel rdpsnd
[rdpsnd_load_device_plugin]: [dynamic] Loaded mac backend for rdpsnd   <- twice, post-connect
```

mdrdp, same host, same window:

```
audio: no audio channel was opened by the server this session
```

`query session` confirmed FreeRDP established a real session
(`rdp-tcp#0  ano  2  Active`), so this is not a client that failed to connect —
which was the failure mode of the first attempt at this test and is why it is
called out here.

**Conclusion: the asymmetry is client-side. This is an mdrdp defect, not a host
limitation.** Two clients, one host, minutes apart, opposite outcomes. That
retires the ticket's open question 1 (whether the comparison was ever against
the same host in the same state) and question 2 (whether the servers simply had
nothing to redirect — FreeRDP shows they will engage rdpsnd).

Remaining, and now the actual fix question: mdrdp advertises `rdpsnd` among its
static channels (`src/connect.rs:219`) and sets `wants_audio` from it
(`src/connect.rs:432`), yet the server does not open it. FreeRDP loads rdpsnd as
a **dynamic** virtual channel through `drdynvc`, which is the difference worth
looking at first.

**Not established**, and stated so it is not assumed later: whether audio bytes
would actually flow. quench's console session has no audio render endpoint at
all (see the tranche-6 PARK document), so a negotiated channel is not the same
as audible sound. The channel negotiation is the defect; the endpoint is a
separate, already-recorded problem.

