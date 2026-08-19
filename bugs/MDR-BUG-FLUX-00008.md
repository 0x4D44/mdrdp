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

## Fix

<unfixed — raised only>

## Notes
