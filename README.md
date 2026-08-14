# mdrdp

A fast, portable RDP client in Rust for macOS and Windows.

Built because Microsoft's Windows App (Remote Desktop) on macOS is unreliable in a
specific, repeatable set of ways: the clipboard stops working mid-session, window
geometry is destroyed when a monitor sleeps or powers off, and latency degrades the
longer a session runs.

## What it is

- **One process per session.** `mdrdp` with no arguments opens the favourites launcher;
  `mdrdp <host>` opens a session window. The launcher spawns sessions as child
  processes, so a wedged channel or a crash can never take down the others.
- **A favourites list.** Double-click an entry, get connected.
- **A clipboard that stays alive.** Every clipboard request is bounded by a timeout and
  can never wedge the channel.
- **Window geometry that survives the night.** Session resolution is decoupled from
  window size by default, so a monitor powering off cannot reflow the remote desktop.
- **A stats HUD.** Codec mix, cache hit rates, decode times, frame drops and round-trip
  latency — because "it feels slow" is not a bug report.

## Status

Greenfield. Nothing works yet.

## Building

```
cargo build --release
cargo test
```

## Non-goals (for now)

Drive redirection, multi-monitor, RemoteApp/RAIL, smart-card redirection.
Audio output is in scope.

## Licence

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
