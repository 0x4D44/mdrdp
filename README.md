# mdrdp

A fast, portable RDP client in Rust for macOS and Windows.

Built because Microsoft's Windows App (Remote Desktop) on macOS is unreliable in a
specific, repeatable set of ways: the clipboard stops working mid-session, window
geometry is destroyed when a monitor sleeps or powers off, and latency degrades the
longer a session runs.

## Quick start

**1. Put the password in the keychain.** It is never an argument, an environment
variable, or a log line. Run this in a real terminal — the prompt needs a tty:

```
security add-generic-password -s mdrdp -a 'YOUR-ACCOUNT' -w
```

`YOUR-ACCOUNT` is whatever you pass to `--user`, or the `username` on a favourite.

**If the keychain is unavailable**, mdrdp says so and prompts for the password on the
terminal instead (echo off, used for that session only, never written anywhere). So a
misbehaving keychain degrades to typing a password — it does not lock you out of your own
desktop. A *missing* entry is treated differently and is not prompted for: that is a setup
mistake with a known fix, and mdrdp prints the exact command above.

**2. Add a favourite** (optional — you can connect by host without one). The file is
created by hand for now; `mdrdp --list` prints its path:

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

`mode = "fullscreen"` is also accepted; it currently resolves to 1920x1080, since the
session resolution has to be chosen before any window exists.

**3. Run it.**

```
mdrdp                          # favourites launcher — double-click to connect
mdrdp Temper                   # connect to a saved favourite by name
mdrdp temper --user ACCOUNT    # connect to a host directly
mdrdp --list                   # print saved favourites
mdrdp <host> --password-stdin  # read the password from stdin (scripting/CI)
mdrdp <host> --duration 30     # disconnect cleanly after N seconds
mdrdp <host> --screenshot f.bmp  # write the final frame to a file
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

The overlay reports latency percentiles, **drift since connect**, bitmap-cache
effectiveness, frame count and bytes in. Drift is the number that matters for "latency
that degrades over time": the baseline is frozen from the first 100 samples of the
session and never updated, so a session that is slower than it started says so.

Cache effectiveness is reported two ways deliberately. Hit rate flatters a cache that
hits often on tiny regions; the share of *pixels* served from cache is the honest
measure, and the two disagree exactly when the cache is not earning its keep.

## What works

- Connect over NLA/CredSSP with trust-on-first-use certificate pinning
- EGFX graphics — ClearCodec and RFX Progressive, keyboard, mouse and scroll
- Favourites launcher, saved as TOML and written atomically
- Clipboard text in both directions, with explicit timeouts so it cannot wedge
- Audio playback (PCM 16-bit, 44.1/48kHz, mono or stereo)
- Window geometry restored after a monitor sleeps or the screen locks
- Graceful disconnect — abandoning the socket leaves a live session on the host

## What does not work yet

- **ClearCodec fails on most tiles when the desktop is busy** — an upstream parse failure
  in the v-bar path starves the decoder's caches and every later tile referencing them
  fails too. Text and UI regions that do decode render correctly.
- **Fullscreen is not a real fullscreen mode**, just a default resolution.
- No drive redirection, multi-monitor, RemoteApp/RAIL, smart-card, or microphone.
- Favourites are edited by hand; there is no add/remove UI.

## Honest status

Verified against real Windows hosts: connect, graphics, the bitmap cache, the graceful
disconnect, and that the CLIPRDR / RDPSND / DRDYNVC channels all join. The remote desktop
genuinely renders — `--screenshot` writes the presented frame so that claim can be checked
rather than taken on trust. A session prints
which channels are live, so "the clipboard isn't working" and "the clipboard channel
never joined" are distinguishable.

The remote image renders correctly: measured against FreeRDP 3.27.1 on the same host and
wallpaper, mean absolute error 4.3/255 with a detail ratio of 1.03 (i.e. the same amount
of real image variation, not a flat field).

Still unproven, and worth saying plainly:

- **No audio has been heard.** The channel joins and the client no longer tells the
  server to suppress audio, but the test box negotiated no format, so playback has never
  actually run.
- **No clipboard round trip has been observed.** The channel joins and the state machine
  is covered by tests, including the two-consecutive-cycles case, but no text has been
  copied across a live session.
- Some RFX Progressive frames still fail to decode (see above).

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
