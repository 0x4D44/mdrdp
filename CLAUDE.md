# mdrdp — agent instructions

A portable RDP client in Rust. Targets **macOS (Apple Silicon) and Windows** from one
codebase. Read this before touching anything; the fleet `CLAUDE.md` still governs
everything it does not contradict.

## What this repo is for

Microsoft's Windows App (Remote Desktop) on macOS is unreliable in five repeatable ways.
Those five are the product requirements, and they are the bar every change is judged
against:

1. **Clipboard dies mid-session.** Ours must survive an all-day soak with zero wedges.
2. **Window geometry is destroyed** when a monitor sleeps or powers off overnight.
3. **Latency degrades** the longer a session runs.
4. **No visibility** — no cache stats, no codec mix, no way to tell why it feels slow.
5. **Performance** generally.

Plus: a favourites launcher, one server per invocation, and audio output.

## Decided architecture — do not relitigate without raising it first

These were settled before the first commit. Changing one is an architectural fork that
needs Arthur, not a design-phase judgment call.

- **Protocol comes from [IronRDP](https://github.com/Devolutions/IronRDP)**, not
  hand-rolled. Our value is the client layer above it. Do not reimplement PDUs, MCS,
  CredSSP, or codecs that IronRDP already has.
- **One OS process per session.** No-args = favourites launcher; `mdrdp <host>` = one
  session window. The launcher spawns sessions as children. This buys crash isolation
  and independent clipboard state for free.
- **Portable by default, platform-specific only at the edges.** Core logic is
  `cfg`-free. Platform code is confined to named modules behind a trait
  (window policy, credential store, hardware video decode).
- **Session resolution is decoupled from window size by default.** Scale/letterbox in
  the renderer. A display-configuration change must never reflow the remote desktop.
  Dynamic resize is opt-in and only ever fires on a *user-initiated* resize.
- **Credentials live in the OS keychain**, never in a config file, never in a log.

## Scope

**In:** core session, keyboard/mouse, clipboard, audio output (RDPSND), favourites
launcher, stats/HUD, window-geometry persistence.

**Out for now:** drive redirection, multi-monitor, RemoteApp/RAIL, smart-card
redirection, server-side/listener mode. Say so and stop rather than quietly adding one.

## Commands

```
cargo build                 # debug
cargo build --release       # what you benchmark; never benchmark a debug build
cargo test                  # unit + integration
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

Focused first: `cargo test -p mdrdp <module>` before the whole suite. Run suites with
stdin closed — `cargo test </dev/null` — or a test that reads stdin hangs forever.

## Validation rules specific to this repo

- **Never benchmark or latency-test a debug build.** Decode and colour conversion are
  10-30x slower unoptimised; a debug measurement is noise, not evidence.
- **A latency or throughput claim needs a number and the conditions it was taken under**
  (host, network, resolution, codec, build profile). "Feels fast" is not a result.
- **Protocol changes need a real server.** Windows 11 Pro over LAN is the reference
  target. A change that only passes against a mock has not been validated. Arthur can
  provision a Windows terminal-server box to test against — ask him rather than
  substituting a mock or a Linux RDP server for protocol work.
- **Soak-class requirements need soak-class evidence.** The clipboard and
  latency-degradation requirements are about behaviour over hours. A green unit test
  does not discharge them.

## Version policy

Standard fleet rule: **integration owns exactly one version bump**; task branches never
pre-bump. Patch unless semver clearly calls for more.

## Security

Never log, print, or commit credentials, session contents, or clipboard payloads. The
clipboard path in particular handles arbitrary user data — no debug logging of contents,
only sizes and format IDs.
