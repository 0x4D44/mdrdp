# `spike-viewer` — the macOS measuring client for the latency spike

Connects to `rhydra-server` over an SSH tunnel, decodes the HEVC stream it sends,
presents it in a window, and forwards its own keystrokes back on the server's input
port. It writes one JSON line per frame so the client half of the pipeline is a
per-stage budget rather than one number.

**The presenter is mdrdp's own.** Decoding is `mdrdp::hevc::hardware_decoder()` called
through `VideoDecoder::decode`; presenting is `mdrdp::window::present_into` into a
`softbuffer` buffer behind a `winit` window, at the crate versions the root
`Cargo.toml` pins (winit 0.30.13, softbuffer 0.4.8). That is the requirement, not a
convenience: the spike exists to compare *server and transport* designs, so a
comparison is only valid if the client side is byte-for-byte the same code as the
client being compared against. Nothing here re-implements a decoder, a colour
conversion, or a scaler.

The wire format is not re-implemented either — `rhydra-server`'s own `framing`,
`annexb` and `input_proto` modules are imported as a path dependency.

**Not shipped code.** Like everything under `tools/`, this crate carries its own empty
`[workspace]` table, so `cargo build` at the repository root still builds `mdrdp` and
nothing else.

---

## Building

```
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

macOS on Apple Silicon. On any other platform the binary prints
`spike-viewer: macos only` and exits 2 — VideoToolbox and the CoreMedia host clock have
no portable equivalent, and `probe glass`, the instrument this viewer is measured with,
is macOS-only for the same reasons. The library still compiles and tests elsewhere.

The crate repeats mdrdp's `[patch.crates-io]` table with the paths rebased. Cargo
applies a patch table only from the workspace root, and this crate is its own root, so
without the repetition it would link *unpatched* IronRDP — a different client from the
one being measured. See `vendor/README.md` for what the six forks change.

---

## Running it

Open the tunnel to the host running `rhydra-server` (both its listeners bind 127.0.0.1
only — an unauthenticated screen feed plus a keystroke injector on the LAN is a
remote-control channel for anyone on it):

```
ssh -L 9500:127.0.0.1:9500 -L 9501:127.0.0.1:9501 user@quench
```

Then, on the Mac:

```
spike-viewer --connect 127.0.0.1:9500 --input 127.0.0.1:9501 --out client.jsonl
```

```
spike-viewer [--connect 127.0.0.1:9500] [--input 127.0.0.1:9501]
             [--out FILE.jsonl] [--title STR]
```

Both endpoints are `ADDRESS:PORT` literals; a host name is refused, because the address
is always the local end of the tunnel.

The window opens at 640x360 titled "waiting for stream" and resizes itself to the
stream's coded size the moment the first frame decodes. Close it, or press Ctrl-C, to
end the run: either path closes both sockets and flushes the stats file.

Without `--out` the viewer is just a picture — nothing is recorded.

### What the window does and does not do

The image is presented **1:1**, never scaled. A window larger than the stream centres
the image on black; a window smaller crops it. That is deliberate: fitting the stream to
the window would turn on nearest-neighbour resampling inside the stage being measured,
and its cost would vary with how far off the window was.

On a Retina display the window is sized in *physical* pixels, so a 1920x1080 stream
occupies a window that measures 960x540 points. Each stream pixel still lands on exactly
one device pixel, which is what a 1:1 present needs.

There is no stats overlay. Painting one would repaint pixels inside the interval
`probe glass` is watching.

### Measuring it with `probe glass`

The viewer forwards its own keyboard events, so the existing instrument works unchanged:

```
mdrdp probe glass --target-pid <spike-viewer pid> ...
```

`--target-pid` posts the synthetic keystroke straight to this process rather than to
whatever has focus, so nothing stealing focus mid-run can misroute the measurement.

---

## Keyboard coverage

Every winit **physical** key in the table below posts an 8-byte record to the input
port; anything else is dropped locally and never reaches the wire (the server closes the
connection on a virtual key outside `0x01..=0xFE`, so an unmapped key must not be sent).

| keys | virtual-key codes |
| --- | --- |
| A–Z | `0x41`–`0x5A` (ASCII uppercase) |
| 0–9 | `0x30`–`0x39` |
| Space | `0x20` |
| Enter, Numpad Enter | `0x0D` `VK_RETURN` |
| Backspace | `0x08` `VK_BACK` |
| Delete (forward) | `0x2E` `VK_DELETE` |
| Escape | `0x1B` |
| Tab | `0x09` |
| Left, Up, Right, Down | `0x25`, `0x26`, `0x27`, `0x28` |

The measurement workload is `x` and backspace — `VK_X` `0x58` and `VK_BACK` `0x08` — and
both are pinned by tests. On a Mac keyboard the key *labelled* delete is backspace;
sending `VK_DELETE` for it would erase the wrong character, so the two are separately
asserted.

Key **repeats are forwarded** as further key-downs. An OS repeat is a genuine extra
transition and `SendInput` on the far side reproduces exactly that.

Records are written on the window thread inside the key event, not handed to a worker:
a scheduling hop would land inside the interval being measured.

---

## The stats file

One JSON object per line. `type` names the record. Every `*_us` field is an absolute
stamp on **the CoreMedia host clock in microseconds** — the same clock
`mdrdp probe glass` stamps its keystrokes and its ScreenCaptureKit presentation
timestamps with, so a client file and a glass run from the same session overlay without
a conversion.

**Client stamps and server stamps are never comparable.** The two machines' clocks are
not synchronised — that is the HLD's design, not an omission — so a server `*_qpc_us`
minus a client `*_us` is meaningless. Differences *within* one file are the measurement.

### `client-header` — first line

`schema`, `clock` (which clock the stamps are on), `connect`, `input`, and `decoder`:
whether this build has a hardware HEVC decoder at all. `decoder: false` means every
access unit will be a `decode_error` and the window will stay black.

### `frame` — one per decoded frame

| field | taken when |
| --- | --- |
| `recv_done_us` | the `read` that completed this message returned |
| `decode_in_us` | immediately before `VideoDecoder::decode` |
| `decode_out_us` | immediately after it returned |
| `present_done_us` | immediately after `buffer.present()` returned |

Plus `frame` (a counter over all access units, decoded or not), `au_bytes`, `keyframe`,
the decoded `width`/`height`, `dropped`, and `partial` — whether the present that
closed this row went through the damage-only path (Increment 4) rather than a full
convert. The same `partial` field appears on painted `rects` rows.

Read the gaps: `decode_in - recv_done` is the handoff to the decoder,
`decode_out - decode_in` is the decode itself, `present_done - decode_out` is the wait
for the window thread plus the copy-and-convert plus the present.

`dropped: true` with `present_done_us: null` means the decode thread had a newer frame
before the window thread ever presented this one. The handoff holds exactly one frame
and the newest wins — a queued frame is a stale frame, and presenting it would flatter
the latency. Drops are recorded rather than omitted; a file that quietly dropped them
would overstate how well the pipeline kept up.

### `decode_error` — one per refused access unit

`frame`, `recv_done_us`, `au_bytes`, `keyframe`, `detail`. Expected at the head of a
stream: the server's first access units can arrive before its parameter sets, and the
decoder refuses them with *"no SPS/PPS seen yet"* until the next keyframe heals it. A
run of these that never stops is the real signal — hence one line each, and a matching
line on stderr. Sizes and flags only; never a byte of the payload.

### `input` — one per keystroke sent

`seq`, `vk`, `transition` (`down`/`up`) and `sent_us`, stamped immediately before the
write. `seq` is echoed in the server's own `input` stats line, which is how one
keystroke is followed across the two files despite the two clocks.

### `server` — the server's stats lines, passed through

The server sends every line it writes as a type-2 message, so an archived client file is
self-describing without fetching the server's `--out`. Its lines use `record` as their
discriminator where ours use `type`, so a well-formed one is nested **verbatim** under
`line` (the original bytes are concatenated, not re-serialised) rather than merged into
one namespace:

```json
{"type":"server","line":{"record":"frame","frame":1,"au_bytes":900}}
```

A line that is not valid JSON becomes `{"type":"server_raw","text":"…"}` rather than
being dropped. Server field meanings live in `../server/README.md`.

Lines are flushed individually: a run ends with Ctrl-C or a window close, and a buffered
tail lost at that moment is a measurement lost.

---

## Testing without a server

`tests/loopback.rs` runs the whole receive path — socket, framing, dispatch, the real
`DecodeSink` with the real decoder, and the stats file — against a local `TcpListener`
feeding synthetic messages. It asserts that a garbage access unit is *recorded and
survived*: the message after two refused frames must still arrive. That behaviour is
required, not incidental, because a real server's first frames legitimately race its
parameter sets.

Nothing above `app.rs` touches a window, which is what makes that possible. What the
test does not prove is that a real HEVC stream decodes; that needs a real encoder and
is the live run's job.

---

## Known-unvalidated areas

* **No live run has happened.** Everything below compiles, is unit- and
  loopback-tested, and has been reasoned against the server's documented contract, but
  no frame from a real encoder has been through it.
* **Resolution changes mid-stream.** A new SPS/PPS rebuilds the VideoToolbox session
  inside mdrdp's decoder and the window resizes itself on the next frame. The path
  exists; it has never been provoked.
* **Frame drops under load.** The one-frame handoff is expected to drop when decode
  outruns the display's refresh. The accounting is written and unit-tested; the rate it
  actually produces is a live-run question.
* **The 200 ms interrupt poll.** The event loop wakes five times a second to check the
  Ctrl-C flag. It paints nothing, so it should not perturb a measurement — "should
  not" being reasoning, not a measurement.
