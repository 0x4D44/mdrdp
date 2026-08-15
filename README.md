# mdrdp

A fast, portable RDP client in Rust for macOS and Windows.

Built because Microsoft's Windows App (Remote Desktop) on macOS is unreliable in a
specific, repeatable set of ways: the clipboard stops working mid-session, window
geometry is destroyed when a monitor sleeps or powers off, and latency degrades the
longer a session runs.

## Quick start

**1. Open `mdrdp` and choose New connection.** Enter a display name, host, port,
username, password, and either fullscreen or an explicit session size. The password goes
straight to the platform credential store. The saved favourite contains only the account
key used to retrieve it.

**2. Double-click the saved connection.** The launcher stays open and starts each session
in its own process, so several desktops can run independently.

For a direct command-line connection on macOS, seed the matching account in Keychain first:

```
security add-generic-password -s mdrdp -a 'YOUR-ACCOUNT' -w
```

`YOUR-ACCOUNT` is whatever you pass to `--user`. GUI-created connections use a distinct
`username@host:port` key so two hosts with the same username can have different passwords.
For unattended tests on either platform, `--password-stdin` reads one password from
standard input without putting it in the process list or shell history.

**If the keychain is unavailable**, mdrdp says so and prompts for the password on the
terminal instead (echo off, used for that session only, never written anywhere). So a
misbehaving keychain degrades to typing a password — it does not lock you out of your own
desktop. A *missing* entry is treated differently and is not prompted for: that is a setup
mistake with a known fix, and mdrdp prints the exact command above.

The favourites file is ordinary password-free TOML and is written atomically. Advanced
users can inspect or edit it at:

- macOS: `~/Library/Application Support/mdrdp/favourites.toml`
- Windows: `%APPDATA%\mdrdp\favourites.toml`

```toml
[[favourite]]
name = "Temper"
host = "temper"
port = 3389
username = "YOUR-ACCOUNT"

[favourite.window_size]
mode = "explicit"
width = 1920
height = 1080
```

`mode = "fullscreen"` opens a real borderless fullscreen window. The remote resolution
remains fixed at 1920x1080 unless an explicit size is chosen.

**3. Command-line alternatives.**

```
mdrdp                          # favourites launcher — double-click to connect
mdrdp Temper                   # connect to a saved favourite by name
mdrdp temper --user ACCOUNT    # connect to a host directly
mdrdp --list                   # print saved favourites
mdrdp <host> --password-stdin  # read the password from stdin (scripting/CI)
mdrdp <host> --duration 30     # disconnect cleanly after N seconds
mdrdp <host> --screenshot f.bmp  # write the final frame to a file
mdrdp <host> --metrics-json run.json  # write redacted acceptance metrics
```

Command-line flags override whatever the favourite specifies.

The launcher **stays open** after you connect, and each session runs in its own process.
So you can open several, and a session that wedges or crashes cannot take down the
others or the launcher. Closing the launcher leaves running sessions alone.

## In a session

| Key | Does |
| --- | --- |
| `Ctrl+Alt+S` | Toggle the stats overlay |

Every other keystroke goes to the remote desktop. `Ctrl+Alt+S` is consumed locally, so
it is one combination you cannot send to Windows.

The overlay reports latency p50/p95/p99/max, **drift since connect**, active graphics
codecs, bitmap-cache effectiveness, frame count, bytes in, and audio counters. Drift is
the number that matters for "latency that degrades over time": the baseline is frozen
from the first 100 samples and never updated.

Cache effectiveness is reported two ways deliberately. Hit rate flatters a cache that
hits often on tiny regions; the share of *pixels* served from cache is the honest
measure, and the two disagree exactly when the cache is not earning its keep.

`--metrics-json` writes the same latency, graphics, cache, codec, channel, audio, CPU, and
peak-memory evidence in a stable JSON schema. It never includes the host, username,
credential, file paths, clipboard data, pixels, or audio payloads. The report is assembled
completely before writing begins, so a serialization failure cannot
leave a plausible-looking partial report.

## What works

- Connect over NLA/CredSSP with trust-on-first-use certificate pinning
- EGFX graphics — ClearCodec and RFX Progressive, keyboard, mouse and scroll
- Favourites launcher with a graphical New connection form and secure password storage
- Real borderless fullscreen with a fixed remote resolution
- Clipboard Unicode text, empty content, and bounded CF_DIB images in both directions,
  with explicit timeouts so one failed transfer cannot wedge later transfers
- Audio playback (PCM 16-bit, 44.1/48kHz, mono or stereo)
- Window geometry restored after a monitor sleeps or the screen locks
- Graceful disconnect — abandoning the socket leaves a live session on the host

## What does not work yet

- No drive redirection, multi-monitor, RemoteApp/RAIL, smart-card, or microphone.
- Saved connections can be added in the launcher, but there is no edit/remove UI yet.

## Honest status

Verified against real Windows hosts: NLA/TLS connect, ClearCodec/RFX Progressive and
uncompressed graphics, the bitmap cache, fixed-resolution windows, graceful disconnect,
desktop input, two-way clipboard transfer, real audio playback, and
CLIPRDR/RDPSND/RDPDR/DRDYNVC channel joins. A
35-second optimized Quench run at 1920x1080
ended gracefully with zero decode, undecoded-region, surface, cache-miss, or unhandled-PDU
errors. All 1,021 bitmap-cache lookups hit. The redacted report measured 0.57% average CPU
and 169 MiB peak resident memory for that short idle first-run screen; it is a smoke result,
not a multi-hour stability claim. `--screenshot` captured the fully rendered final frame,
so the counters were checked against pixels rather than taken on trust.

The remote image renders correctly. The current first-run UI exposed NSCodec rectangles
nested inside ClearCodec; the formerly empty decoder branch left horizontal stale bands.
The corrected 448x448 command now matches FreeRDP 3.27.1 byte-for-byte across its residual,
band, and subcodec layers, and the exit screenshot has no missing regions. Earlier wallpaper
comparison on the same host measured mean absolute error 4.3/255 with a detail ratio of 1.03.

Still unproven, and worth saying plainly:

- **Audio has live playback evidence, but not a long soak.** Quench sent 222 packets at
  44.1 kHz stereo through `AUDIO_PLAYBACK_DVC`. The client converted them to a 48 kHz mono
  loopback capture measured at -25.3 dB mean and -9.5 dB peak. Unit tests cover all four
  44.1/48 kHz mono/stereo wire formats, buffering, resampling, and fail-safe device loss.
- **Clipboard soak and induced live timeout recovery remain unproven.** Quench round trips
  Unicode text, empty text, a 1 MiB payload, rapid changes, and a 1920x1080 image in both
  directions. Deterministic protocol tests cover timeout recovery.
- Quench's Windows first-run flow is complete. Direct RDP scancodes exercised the same input
  channel as the real window and reached the full desktop; macOS Accessibility policy still
  prevents unattended host-generated UI input.

## Building

```
cargo build --release
cargo test
```

`vendor/ironrdp-connector` carries a one-line change to the published crate without which
EGFX never opens; see `vendor/README.md`.

## Licence

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
