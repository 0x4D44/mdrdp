# `rhydra-server` — mdrdp's native capture/encode/send server, Windows

The server half of the mdrdp latency spike. It captures one display with DXGI
Desktop Duplication, converts BGRA→NV12 on the GPU, encodes H.264 with a Media
Foundation MFT in low-latency mode, and ships length-prefixed Annex B over loopback
TCP. A second loopback socket takes keystrokes and injects them with `SendInput`.

The point is **decomposition**, not throughput. Every stage stamps
`QueryPerformanceCounter` on entry and exit, so the output is a per-stage latency
budget rather than one opaque number. A frame that took 40 ms tells you nothing; a
frame that spent 2 ms in capture, 1 ms in convert, 31 ms in encode and 6 ms on the
socket tells you where to look.

**Not shipped code.** Like everything under `tools/`, this crate carries its own
empty `[workspace]` table, so `cargo build` at the repository root still builds
`mdrdp` and nothing else.

---

## Building

Native (macOS or Windows) — the pure-logic modules and their tests:

```
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

Cross-compile the Windows executable from macOS:

```
./build.sh                      # → target/x86_64-pc-windows-msvc/release/rhydra-server.exe
XWIN_DIR=/some/other/splat ./build.sh
```

`build.sh` needs the same one-time `xwin` provisioning that
`../../../scripts/check-windows.sh` documents, plus the import libraries — it
**links**, where that script only type-checks. The three `/libpath` arguments in
`build.sh` point at `crt/lib/x86_64`, `sdk/lib/um/x86_64` and `sdk/lib/ucrt/x86_64`.

Verified 2026-08-17 on this Mac: `dxgi.lib`, `d3d11.lib`, `dxguid.lib`, `Mf.lib`,
`Mfplat.lib`, `mfuuid.lib`, `User32.Lib`, `Ole32.Lib` and `WS2_32.Lib` are all
present in the xwin splat, and the produced binary is
`PE32+ executable (console) x86-64, for MS Windows` importing `dxgi.dll`,
`d3d11.dll`, `mfplat.dll`, `ole32.dll`, `oleaut32.dll`, `combase.dll`, `user32.dll`
and `WS2_32.dll`. (`mfuuid.lib` contributes GUID constants only, so it correctly
leaves no DLL import behind.)

On a non-Windows host the binary prints `rhydra-server: windows only` and exits 2.

---

## Running it on the host

Both listeners bind **127.0.0.1 only**. That is a security requirement, not a
default: an unauthenticated screen feed plus a keystroke injector on the LAN is a
remote-control channel for anyone on it. Reach them through an SSH tunnel:

```
ssh -L 9500:127.0.0.1:9500 -L 9501:127.0.0.1:9501 user@quench
```

Then, **in the interactive console session** (see the caveat below):

```
rhydra-server.exe --list-outputs
rhydra-server.exe --output 1 --out C:\Users\ano\spike.jsonl
```

```
rhydra-server --output N [--video-port 9500] [--input-port 9501]
             [--bitrate-kbps 20000] [--gop 120] [--out FILE.jsonl]
             [--no-rects] [--source dxgi|idd]
rhydra-server --list-outputs
```

`--list-outputs` prints every DXGI adapter/output with its resolution, whether it is
attached, and its rotation. The left-hand index is what `--output` takes. This is how
you find the virtual display's output index once the IDD driver is installed.

### `--source` — where frames come from

`dxgi` (the default) is DXGI Desktop Duplication: it works against any output on any
host, and it costs a measured ~7.5 ms between DWM's present and `AcquireNextFrame`
returning.

`idd` reads the `mdrdp-idd` driver's shared texture pool directly — the driver copies
each committed swapchain buffer into one of three named shared textures and publishes
what changed through the `Global\mdrdp-idd` section — which removes that gap. It needs
the driver installed and started, it takes **no `--output`** (the pool is found by
name, not by an output index), and it waits up to 30 s at startup for the driver to
publish a pool, so a server started before the driver still comes up.

The active source is recorded in the stats header as `source`. Quote it with any
figure: the two paths differ by that ~7.5 ms, so a number that does not name its
source cannot be compared with one that does.

One video client at a time. On disconnect the server goes back to accepting, and the
first frame of the next connection is forced to a keyframe. **While no client is
connected the server does not capture at all** — holding the compositor and running
the GPU encoder for an audience of nobody would distort the very measurement it
exists to take.

### `SendInput` needs the interactive session

`SendInput` posts to the input queue of the session the *process* runs in. Started
over SSH into a session-0 or service context, the calls succeed and nothing reaches
the desktop. Run the server from a console session on the host (or from a session
started with `psexec -i`). A `SendInput` that injects fewer events than asked is
reported on stderr rather than swallowed, so this failure is visible instead of
looking like "the keystrokes do nothing".

---

## Wire protocol

### Video channel (default port 9500) — server → viewer

```
[u32 le length][u8 type][payload …]
```

`length` counts the bytes that follow it: the type byte plus the payload. So the
smallest legal message is 1.

| type | payload |
| --- | --- |
| 1 | one H.264 access unit, Annex B, exactly one AU per message |
| 2 | one server stats line (JSON, no trailing newline) |

`TCP_NODELAY` is on. The header line is sent as a type-2 message immediately on
connect, so an archived capture is self-describing without fetching the server's own
`--out` file.

Both encoding and reassembly live in `src/framing.rs`, which is portable and
unit-tested on macOS — the viewer uses the same module rather than a second
implementation of the same format.

### Input channel (default port 9501) — viewer → server

Fixed 8-byte records, no framing of their own:

```
[u8 kind][u8 reserved][u16 le vk][u32 le seq]
```

`kind` is 1 (key down) or 2 (key up); `reserved` must be 0; `vk` is a Win32
virtual-key code in `0x01..=0xFE`; `seq` is the viewer's own counter, echoed back in
the stats line so a round trip can be matched end to end. Anything malformed, and any
short read, closes the connection: a fixed-width protocol that has lost phase cannot
be resynchronised.

---

## Stage timestamps

`--out FILE.jsonl` writes JSONL; the same lines also go out as type-2 messages. One
header line, then one line per frame and one per injected keystroke.

Every `*_us` field is an **absolute QPC stamp in microseconds on the server's clock**.
Differences between adjacent fields are the stage costs; the whole line is one row of
the budget. `qpc_frequency` is in the header, so ticks can be re-derived.

| field | taken when |
| --- | --- |
| `present_qpc_us` | `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime` — when the **compositor** presented the frame |
| `acquire_qpc_us` | `AcquireNextFrame` returned to us |
| `convert_start_us` | just before `VideoProcessorBlt` |
| `convert_end_us` | just after `VideoProcessorBlt` **returned** |
| `encode_submit_us` | `ProcessInput` was called for this frame |
| `encode_out_us` | `ProcessOutput` handed the bytes back |
| `send_done_us` | the socket write returned |

Three caveats a reader must know, because each one will otherwise be misread:

* `acquire_qpc_us - present_qpc_us` is the **duplication pipeline's own** latency,
  not ours. It is time that had already passed before we were told a frame existed.
* `convert_end_us` is when the blit was **submitted**, not when the GPU finished.
  `VideoProcessorBlt` does not wait. The real conversion cost surfaces as
  back-pressure inside the encode stage, so do not read `convert_end - convert_start`
  as "the cost of colour conversion".
* `encode_submit_us` on the async path is stamped when the encoder granted a
  `METransformNeedInput` credit and we handed the frame over — so
  `encode_submit_us - convert_end_us` includes any wait for the encoder to be ready.
  That wait is real latency and belongs in the budget.

Other per-frame fields: `au_bytes`, `keyframe`, `param_sets_prepended` (below), and
`dropped_frames` — a cumulative count of frames discarded because the send queue was
full. The queue is bounded at two frames and **lossy on purpose**: a queued frame is
a stale frame, and dropping the newest keeps the latency number honest. Every drop is
counted, so the loss is never silent.

The header line carries the config, the adapter and output description, `source`
(`dxgi` or `idd` — which capture path produced the run), the selected
MFT's friendly name, its kind (`async-hardware` / `sync-software`), the QPC
frequency, and — the field to check first when something looks odd —
`codec_api_applied` / `codec_api_refused`. An encoder that silently refuses
`AVLowLatencyMode` is the single most likely explanation for a surprising encode
stage, and this makes that visible without a debugger.

Stats lines are flushed per line. The operator kills this process with Ctrl-C, and a
buffered tail lost at that moment is a measurement lost.

---

## Encoder configuration, and the in-band SPS/PPS route

Hardware **async** MFTs are enumerated first
(`MFT_ENUM_FLAG_HARDWARE | ASYNCMFT | SORTANDFILTER`) and driven by the documented
`IMFMediaEventGenerator` contract — `METransformNeedInput` / `METransformHaveOutput`,
never a blind `ProcessInput`/`ProcessOutput` poll. The Microsoft software encoder is
a **sync** MFT and is the fallback; it drives the plain
`ProcessInput` → `ProcessOutput`-until-`NEED_MORE_INPUT` loop. Both sit behind one
`Encoder` trait, so the pipeline above them is identical, and which one was selected
is recorded in the stats header. `src/win/encode.rs` opens with the full contract in
the order the code follows it.

Settings: `MFVideoFormat_NV12` in, `MFVideoFormat_H264` out;
`CODECAPI_AVLowLatencyMode = TRUE`; `AVEncCommonRateControlMode = CBR` with the
`--bitrate-kbps` value; `AVEncMPVDefaultBPictureCount = 0`; `AVEncMPVGOPSize` from
`--gop`; Main profile. Zero B-frames is load-bearing beyond compression: it makes
output order equal submission order, which is what makes the submit-stamp FIFO that
pairs `encode_submit_us` to the right frame correct.

### Which parameter-set route was taken

**Both, belt and braces — in-band preferred, out-of-band as the fallback.**

The receiver forces the issue: mdrdp's decoder at `src/h264.rs` (`decode_yuv420`)
hard-errors with *"no SPS/PPS seen yet; cannot decode this access unit"* for anything
that arrives before a parameter-set pair. MF encoders normally emit SPS/PPS in band
ahead of each IDR, but that is not contractual — the sequence header is *also*
published out of band on the output media type as `MF_MT_MPEG_SEQUENCE_HEADER`.

So the server reads and stores `MF_MT_MPEG_SEQUENCE_HEADER` at configuration time
(and again after any `MF_E_TRANSFORM_STREAM_CHANGE` renegotiation), and for every
**keyframe** checks whether the access unit already carries both an SPS and a PPS.
If it does, the AU goes out untouched and no copy is taken. If it does not, the
stored pair is prepended as `00 00 00 01 SPS 00 00 00 01 PPS`. Non-keyframes are
never padded. Which route fired is recorded per frame as `param_sets_prepended`, and
whether the fallback was even available is `sequence_header_available` in the header.

This lives in `src/annexb.rs` and is unit-tested natively, because it is the one
place where a wrong answer looks like "the viewer shows nothing" rather than like an
error.

---

## Known-unvalidated areas

Everything below compiles, links, and has been reasoned against the documented
contract. **None of it has executed.** The first live run happens on the host.

* **The async MFT event pump is the biggest unknown.** The HLD flagged it and it is
  still the least-exercised code here. Specifics to watch on the first run: whether
  `METransformNeedInput` credits arrive before the first frame is offered; whether
  the non-blocking drain after each submit actually keeps up, or output lags a frame;
  and whether the shutdown drain ever sees `METransformDrainComplete` (the wait is
  bounded at ~256 polls so a driver that never sends it cannot wedge the exit).
* **`MFT_MESSAGE_SET_D3D_MANAGER` and zero-copy NV12.** If the hardware encoder
  rejects the DXGI surface buffer, configuration fails and the server falls back to
  the software encoder — visible in the header as `sync-software`. That is a working
  outcome, not a broken one, but it is not the outcome we want.
* **Colour.** `src/colorspace.rs` packs the
  `D3D11_VIDEO_PROCESSOR_COLOR_SPACE` bitfield by hand (windows-rs exposes it as a
  bare `_bitfield: u32`). The packing is unit-tested; whether BT.709 studio-range NV12
  is what this encoder and mdrdp's decoder agree on is an *end-to-end* question. A
  washed-out or contrast-shifted picture points here first.
* **`DXGI_ERROR_ACCESS_LOST` recovery** is implemented (release the duplication, then
  re-`DuplicateOutput` with a bounded retry on the transient
  `DXGI_ERROR_UNAVAILABLE`) but has never been provoked. It is expected to fire on
  session switches and whenever the IDD driver reconfigures.
* **A per-frame input view.** `CreateVideoProcessorInputView` runs once per frame,
  because the captured texture is a new object each frame and the view cannot be
  cached the way the output views are. That is a known allocation on the hot path; it
  is in the budget's convert stage, so the first run measures it rather than
  guessing.
* **`SendInput` under the interactive-session constraint** (above) — untested from
  the tunnelled setup.
* **No Ctrl-C handler.** Stats are flushed per line so nothing is lost, but the MF
  drain and `MFShutdown` do not run on a kill. Harmless for a spike; worth knowing
  before anyone reads a driver log.

---

## `rhydra-agent` — the session agent

The second binary in this crate: the logon-task supervisor that keeps the whole
capture stack up (IDD creator → device → 1920x1080@240 → `rhydra-server`), with a
loopback JSON control port on 9502 (`{"cmd":"status"}`, `{"cmd":"restart-server"}`,
`{"cmd":"shutdown"}` — one object per line, same shape back). `rhydra-agent install`
registers the onlogon scheduled task and starts it; `rhydra-agent uninstall` shuts
the running agent down over the control port (killing its children) and deletes the
task. Stopping it any other way orphans the children; the sweep at the next agent
start repairs that. Design and rationale:
`wrk_docs/2026.08.18 - HLD - rhydra tranche 1 - session agent and rename.md`.
