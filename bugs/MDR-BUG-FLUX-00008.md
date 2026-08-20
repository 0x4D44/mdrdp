# MDR-BUG-FLUX-00008 — Reveal after Suppress Output paints black: the resumed AVC444 stream fails to decode in a burst

- **State:** Open
- **Priority:** Must
- **Severity:** High
- **Area:** gfx
- **Raised:** 2026-08-19T07:15:59Z
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
- **State history:** Open (2026-08-19T07:15:59Z, raised via `deltic bugs new` model=claude-opus-5@high)

## Observation

Reported by Arthur 2026-08-19: a live mdrdp 0.1.68 session to kiln that had been
connected and working showed a permanently black window, while `mdrdp -S` reported the
connection healthy with 1 decode error. Arthur reports the same session survived a night
intact **before** the Suppress Output change (MDR-BUG-FLUX-00005's fix) landed, and that
he has seen this only on kiln, which is the one host of the three on WiFi. n=1 night, so
the regression pointer is suggestive rather than settled.

Distinct from MDR-BUG-FLUX-00006. That fix (send Refresh Rect on reveal) is present and
firing — every reveal logs `resume and repaint`. The repaint now arrives; it does not
decode.

Measured live on the wedged session, 2026-08-19 (pid 5837, up 8h59m, 2560x1440,
Avc444v2, kiln on WiFi):

- While hidden the session was completely inert: frames 231, bytes_in 18,077,416,
  decode_errors 1 — unchanged across 10 minutes of 1 Hz sampling. Server sending nothing,
  which is Suppress Output working as intended.
- The session thread was healthy throughout, parked in `poll` inside
  `mdrdp::wake::wait_readable` — waiting for data, not stalled.
- Window pixels captured directly from the window server at a reveal (`screencapture
  -l<windowid>`, 5120x2880) were **uniformly black**, no content at all.
- A reveal at 08:01:04 sent the allow + Refresh Rect and produced **zero bytes** over the
  following 14 s, on a desktop that carries a seconds-ticking tray clock and therefore
  always has something to send.
- A reveal at 08:02:33 did produce the repaint burst: in under 3 s, frames 231 -> 368
  (143 paints) and bytes_in 18.08 MB -> 26.00 MB, and **decode_errors 1 -> 148**. More
  failures than frames because an AVC444 update carries two sub-streams and both fail.
- It then healed by itself: frames reached 568 with decode_errors static at 148.

`frames` counts picture changes, not frames received (`session.rs`, `notify_if_painted`
increments `s.frames` only when the paint generation moves), so the burst did paint —
but the window was black for the whole period Arthur was looking at it.

Ruled out on direct evidence: process/thread death (thread parked in `poll`); a protocol
or DVC error (any `Err` out of `stage.process` returns `SessionEnd::Failed`,
`session.rs:375` — the session stayed up, and `ironrdp-dvc-0.8.0` propagates per-channel
errors rather than swallowing them); a silent Deactivation-Reactivation (`drive_reactivation`
logs `resolution: session reactivated at WxH`; absent); a host-side wedge (`probe stages
Kiln` clean — tcp 6.6 ms, X.224 HYBRID_EX 10.3 ms, TLS1.3 9.7 ms, credential-free).

**Not root-caused.** The decode failure *reasons* are the missing datum and are only
emitted on exit (`main.rs:1236`); the session was closed via the Quit menu, which skips
that summary entirely — see MDR-BUG-FLUX-00009, which blocked this diagnosis and should
be fixed first so the next occurrence is readable.

Two hypotheses remain open, and they need different fixes:

1. The server resumes mid-GOP with P-frames whose reference frames were never delivered
   (we asked it to stop). No decoder can reconstruct those. If so, the reveal must force
   the server back to a keyframe rather than assume the stream is still decodable — EGFX
   codec state is keyed to the surface, so the surface is the available lever.
2. Something in our own reassembly or decoder state is at fault on the resumed stream.

Either way this is a client defect: we asked for the suppression, so we own the recovery.
And 147 silent decode failures that reach the log not at all, and `mdrdp -S` only as a
number, is a visibility failure against requirement 4 on exactly the case that requirement
exists for.

Arthur's direction (2026-08-19): fix Suppress Output rather than remove it — without it
the server encodes and ships updates that a hidden window consumes GPU to decode for no
purpose.

## Investigation 2026-08-19 (reproduction attempts)

Still not root-caused, but the field has narrowed and one earlier hypothesis is dead.

**Resolution is implicated, and the failure reaches the host.** Driving suppress/resume
cycles against kiln with a fixed build:

- **1280x720 windowed, 8 cycles, 30 s hidden each**: completely clean. 3149 frames, **0
  decode errors**, 0 surface errors, surfaces +2 -1. Every reveal restored the stream.
- **2560x1440 windowed, 4 min hidden**: after the fourth suppress the **server terminated
  the session** — `[Protocol independent error] The display driver in the remote session
  was unable to complete all the tasks required for startup`. 276 frames, 0 decode errors
  up to that point.

Arthur's failing sessions run at 2560x1440. A remote display driver that has failed but
not taken the session down produces exactly the reported symptom: session alive, input
round-tripping, every client-side measurement healthy, nothing rendering, so nothing sent,
and a black window.

**quench does not reproduce it.** 6 cycles at 1280x720 and a short run at 2560x1440, both
0 decode errors, no driver fault. quench is wired; kiln is the WiFi host.

**A false lead, recorded so it is not re-run.** An apparent wedge at cycle 8 of the
1280x720 run — frames frozen across a full hide/reveal — was the window being restored
but still *covered* by other windows. macOS reports that as occluded and the client
correctly holds the suppression; raising the window recovered the stream immediately
(3122 -> 3147 frames). Designed behaviour, manufactured by the harness, not the defect.

**Ruled out by reading, not guessing:** the visibility command cannot sit unserviced — it
travels on a `WakingSender` that rings the session doorbell, and `IDLE_WAIT` is 250 ms
(`session.rs`).

**Unexplained, do not theorise on it yet:** in one 2560x1440 cycle 2.2 MB arrived during a
4-minute suppression, while the next cycle's suppression held to 1.4 KB.

**Observed live, not by this investigation:** a kiln session on 0.1.70 at 2560x1440 that
this agent did not start accumulated 72 decode errors in ~2 minutes. Worth noting that
0.1.70 carries another agent's AVC444 paint change (MDR-BUG-FLUX-00010, commit 0345af8)
and that every 0.1.69 run above showed 0 decode errors — a correlation only, confounded by
differing conditions, and the original 148-error session predates it on 0.1.68.

**Next, both needing host access:** read kiln's Windows event log for the display driver
fault (source, timestamp, and whether it also fired during the overnight black screens),
and drive the same cycling from FreeRDP or the Microsoft client to establish whether we
are provoking a Windows fault or sending something wrong at that resolution.

## Host-side evidence 2026-08-19 (via fleet SSH)

The likely trigger is on the host, and the one configuration difference across the fleet
lines up with the one host that shows the defect.

**Display power-off timeout (`powercfg SUB_VIDEO VIDEOIDLE`), measured on each host:**

| host | AC | battery | shows the defect |
|---|---|---|---|
| **kiln** | **600 s** | **600 s** | yes |
| quench | never | never | no |
| temper | never | 180 s | no |
| crucible | 900 s | 600 s | no |

crucible carries a timeout but is never idle — fleet agents drive it continuously, so the
idle timer never fires. kiln sits idle and reaches 10 minutes routinely.

**The timing on Arthur's black connect is exact.** kiln's System log, `Win32k`:
`Power Manager has requested suppression of all input (INPUT_SUPPRESS_REQUEST=1)` at
**15:22:41**, released at **15:22:53** — bracketing the connect that came up black
(session pid 49030, 47 frames, 1 decode error). `lessons_learnt.md` already records the
same failure shape from the capture side: a Windows monitor power-off leaves frames
reporting success while the pixels go black.

**Not proven.** kiln exposes both a `Microsoft Remote Display Adapter` and `Intel(R)
Graphics`; it is not established that a console display power-off reaches the RDP
session's virtual adapter. It is plausible if the RDP logon takes over the console session
(same account), and the Win32k events are machine-wide Power Manager activity, but that is
inference. **Test armed 2026-08-19:** kiln's `VIDEOIDLE` set to 0 on both AC and DC
(was `0x258`/`0x258` — restore those to revert). If the black screens stop, this is it.

**kiln also has an unrelated hardware fault worth tracking.** Bursts of WHEA corrected
PCIe errors (dozens at 14:46–14:47, more at 14:14 and 15:14) against
`PCI\VEN_8086&DEV_272B` — the Intel Wi-Fi adapter — with `Netwaw18` driver warnings
alongside, and a measured session RTT p50 of 422 ms against 15.8 ms to anvil. Corrected
errors do not corrupt data, but the link is unstable. Arthur flashed the BIOS and updated
the Intel video driver the same day.

**Why the official client does not show this (hypothesis, untested).** mdrdp sends
Suppress Output on *occlusion*, which for a fullscreen session on its own macOS Space
fires every time the user switches Space — many times an hour. The Microsoft client
suppresses on *minimise*, which rarely happens. mdrdp therefore spends a large fraction of
the day with the host not encoding; the official client spends almost none. That matches
Arthur's report that sessions survived overnight before the Suppress Output change landed.
Clean A/B: restore kiln's 600 s timeout, leave the official client connected and idle past
it, and see whether it also goes black.

**What is ours regardless of trigger.** The client can see that it asked for a repaint and
received nothing decodable, and still shows a black window with no log line, no on-screen
indication, and nothing but a counter in `mdrdp -S`. That silence is a client defect and is
being fixed separately from the trigger.

## Fix

**Root cause: the client destroyed its own H.264 decoder on every ResetGraphics.**

`H264Decoder::reset` is not a flush — it drops the VideoToolbox decompression session
(`h264.rs`, `self.session = None`). A session rebuilt mid-GOP holds no reference frames,
so every P-frame after it fails until the server's next IDR, and the server has no way to
know the decoder was thrown away. `h264.rs` already carried that measurement — "a rebuild
mid-GOP fails every P-frame until the next IDR (measured -12909), so rebuilds must never
be routine" — but `handle_reset_graphics` was calling `reset()` unconditionally.

ResetGraphics is routine on any host with a screen attached: idle display power-off,
backlight, lid, dock all produce one, and so does a window resize (which renegotiates the
resolution). That is why resizing a few times appeared to "fix" it — the resize forced a
fresh keyframe.

The reset was unnecessary as well as harmful. `decode_yuv420` rebuilds the session itself
whenever parameter sets first arrive or change, so a genuinely new stream heals at its
first IDR; a restarted stream reusing the same SPS/PPS decodes correctly on the live
session, because an IDR resets reference state inside the codec.

**Decisive evidence**, Arthur's session on 0.1.78, 2026-08-19 — the epilogue that
MDR-BUG-FLUX-00009 made reachable:

```
frames 168  decode errors 105  undecoded regions 0  surface errors 0
surfaces +4 -3  reset Some((2560, 1440))  unhandled pdus 0
codecs {"Avc444v2": 163}
decode failures by reason:
     67  Avc444v2: avc444 luma decode failed
     38  Avc444v2: avc444 chroma decode failed
```

105 failures across 163 AVC444 updates, with a ResetGraphics in the same session. Healthy
sessions on the same client show `surfaces +2 -1` and zero failures. The stall diagnostic
also fired in the log, catching a reveal that produced no frames for 5 s.

**Why only kiln:** it is the only machine with a real screen attached. Every fleet test
host is headless and therefore cannot produce a display-mode transition at all — raised
separately as MDR-BUG-FLUX-00015, because no test written on this fleet could have caught
this.

Two earlier hypotheses recorded above are **superseded** and should not be re-run: the
display power-off timeout correlation (real but incidental — it is one of several ways to
reach a ResetGraphics), and the Intel driver version difference (kiln's 32.0.101.8974 was
installed 2026-08-19 at 14:01, after the failures began, so it cannot be the cause).

Fixed on the branch that carries this note, with a regression test proven red-then-green
by restoring the reset. `on_decode_failure` was also widened from `&'static str` to `&str`
so the reason tally carries the decoder's own error — including the VideoToolbox
OSStatus — rather than a bare label that says a decode failed and nothing about why.

**The mechanism is now measured, not inferred.** With the widened reason string, a live
kiln session reports:

```
7  Avc444v2: avc444 luma decode failed: VideoToolbox decode callback failed (status -12909)
3  Avc444v2: avc444 chroma decode failed: VideoToolbox decode callback failed (status -12909)
1  Avc444v2: avc444 luma decode failed: oscillating SPS/PPS (divergent AVC444 sub-streams); refusing to rebuild
```

`-12909` is exactly the status `h264.rs:235` predicted for a decoder decoding without its
reference frames.

## REFUTED 2026-08-20: it is not the suppress/resume — it is surface replacement

**The hypothesis below is wrong.** It was mine, and it was built on three unmatched runs
in which two variables moved at once. Kept, struck through in substance, because the
evidence that killed it is worth as much as the evidence that raised it.

**What refuted it.** A kiln session on 0.1.79 ran 14h26m and logged **35 suppress / 34
resume cycles with zero resolution changes**. If suppress/resume broke reference
continuity, 34 reveals would have shown it. They did not — the failures arrived in a
single burst, not per reveal, and the repaint-stall detector never fired at all (frames
kept painting throughout).

**What the epilogue then showed, and it is decisive:**

```
frames 2996  decode errors 109  undecoded regions 0  surface errors 0
surfaces +3 -2   reset Some((2560, 1440))
     71  avc444 luma decode failed:   VideoToolbox decode callback failed (status -12909)
     35  avc444 chroma decode failed: VideoToolbox decode callback failed (status -12909)
      2  avc444 chroma decode failed: oscillating SPS/PPS ...; refusing to rebuild
      1  avc444 luma decode failed:   oscillating SPS/PPS ...; refusing to rebuild
```

`surfaces +3 -2` — one replacement beyond the +2 -1 baseline — **with no resolution change
anywhere in the log**. The server rebuilt its output surface of its own accord. 106 of the
109 failures are `-12909`: decoding without reference frames.

Re-reading the three runs that misled me, the error run was also the only one with extra
surface churn:

| run | reveal | surfaces | errors |
|---|---|---|---|
| `--size`, 40 s | no | +2 -1 | 0 |
| pre-fix `--fullscreen` | no | +2 -1 | 0 |
| fixed `--fullscreen` | yes | **+3 -2** | **11** |

Both variables moved; I attributed it to the one I was already thinking about.

**Consequence:** suppress-on-occlusion is not implicated, so Arthur's decision to keep it
carries no hidden cost, and the minimal-allow-rect and forced-resync ideas explored on the
way here are all unnecessary.

## Fix 2026-08-20: reset the decoder when the surface it decodes for changes

A surface's size is fixed at `CreateSurface`, so any rebuild of the output — a resolution
change, or the server's own decision — **replaces** it, and the replacement is a new H.264
sequence. One decoder serves the whole channel (the August HLD's decision, unchanged), so
it carried the previous surface's reference frames into a video they do not belong to.

`retarget_decoder` in `vendor/ironrdp-egfx/src/client.rs` now resets the decoder when the
surface it is decoding for changes, from both the AVC444 and AVC420 paths. FreeRDP gets
the same effect structurally by freeing `surface->h264` in `gdi_DeleteSurface`; this repo's
own lesson already said it — *codec state dies with the SURFACE, not with ResetGraphics*.

**Keyed on the first frame FOR a surface, not on DeleteSurface**, because the ordering is
the server's to choose: it may create and start painting the replacement before deleting
the old one, and a reset fired on that delete would destroy the references of the surface
now on screen.

Note this is **not** the design rejected as
`wrk_docs/2026.08.19 - HLD - one H264 decode session per AVC444 view.md`. That proposed
splitting decoders per AVC444 sub-stream and its premise was refuted. This is the surface
lifecycle, which is a different axis and was independently justified.

**Known limitation, recorded rather than designed around:** if a server ever kept two
surfaces live and alternated updates between them, this would reset on every switch and
thrash. It cannot happen today — multi-monitor is out of scope and every measured session
shows exactly one live surface — and the fix for it, should it ever arise, is a decoder
per surface as FreeRDP has.

## Superseded hypothesis (kept for the record): reference continuity across suppress/resume

**This bug is NOT fully fixed.** Removing the ResetGraphics teardown turns a permanent
black desktop into a short burst that recovers, but decode failures remain, and three live
runs on kiln 2026-08-19 isolate a second cause:

| run | reveal in the session? | decode errors |
|---|---|---|
| fixed build, `--size 2560x1440`, 40 s | no | 0 |
| pre-fix build, `--fullscreen`, 45 s | no | 0 |
| fixed build, `--fullscreen`, 45 s | **yes** | **11, all -12909** |

Errors appeared only in the run that contained a reveal. The likely mechanism: while
output is suppressed the server keeps encoding and discarding, so its reference frames
advance without ever reaching us; on resume its P-frames reference frames we never
received, and the Refresh Rect we send asks for a repaint but does not oblige the server
to emit an IDR.

If that holds it is the mechanism behind the ORIGINAL overnight reports, which involved no
resize at all — and it means Suppress Output is not safe as designed against a
differential codec. That is an architectural question (does the client stop suppressing
under AVC, force a keyframe some other way, or accept a transient?), explicitly for Arthur
rather than a judgement to make inside a fix.

**Do not close this bug on the ResetGraphics fix alone.** Three runs is a thin sample and
the two paths were not otherwise matched; the correlation is suggestive, not settled.

### DECIDED 2026-08-19: suppress-on-minimise-only is permanently parked

Arthur's decision, recorded so it is not re-proposed. mdrdp keeps sending Suppress Output
on **occlusion**, not only on minimise, despite that being the thing no other client does.

The comparison that prompted the question: FreeRDP suppresses only on `UnmapNotify` and
the minimized `PropertyNotify` — never on occlusion — so established clients avoid this
whole failure class by rarely entering the suppressed state rather than by handling resume
better. FreeRDP has the same bug on file anyway
([#4371](https://github.com/FreeRDP/FreeRDP/issues/4371), "black screen with gfx and
suppress output", reproduced by switching workspaces away and back). Notably FreeRDP sends
no Refresh Rect on resume at all, so mdrdp's FLUX-00006 fix already does more than
upstream — the repaint request was never the weak link.

Rationale for keeping it: a hidden window otherwise makes the server encode and ship
updates that cost bandwidth on the wire and GPU to decode, for a window nobody can see,
and Space-switched-away is the common case rather than minimised. The saving is judged
worth the exposure.

**Consequence to carry:** the suppress/resume reference-continuity mechanism above stays
live and unmitigated. Whatever eventually addresses it must work *with* occlusion-driven
suppression, not by removing it.

## Notes
