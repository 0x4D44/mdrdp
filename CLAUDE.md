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
- **Protocol changes need a real server.** A change that only passes against a mock has
  not been validated. Never substitute a mock or a Linux RDP server for protocol work.
- **Soak-class requirements need soak-class evidence.** The clipboard and
  latency-degradation requirements are about behaviour over hours. A green unit test
  does not discharge them.

## The test host

`temper` (`temper.lan.example`, `192.0.2.171`) on the LAN, port 3389. It is the reference
target for all protocol work and all comparisons against Microsoft's client.

- **ICMP is blocked** (Windows Firewall default). `ping temper` fails on a perfectly
  healthy host — test reachability with a TCP connect to 3389 instead.
- **It requires NLA** — it selects HYBRID_EX and rejects legacy RDP security outright.
  There is no unauthenticated path to a first pixel, so the credential path is on the
  critical path for the very first working connection, not a later hardening phase.
- **Latency floor — still PROVISIONAL: TCP RTT p50 3.48 ms**, 95% CI [3.46, 3.51],
  n=3000, release build (p95 4.74 ms, p99 5.84 ms). Good enough to reason with, not
  settled: every sample was taken inside one 92-minute afternoon sitting, so it describes
  one contention regime rather than the day.

  To settle it, run `probe rtt temper --samples 1000 --out baseline/temper-rtt.jsonl` at
  genuinely separated times — morning, evening, late night, ideally across more than one
  day — then `probe summarise baseline/temper-rtt.jsonl`. The tool reports its own
  verdict; **never quote a figure it calls PROVISIONAL as settled.**

  The coverage rule requires a ≥6 h wall-clock span, not merely ≥3 distinct UTC hour
  labels. That is deliberate and was learned the hard way: three short batches run either
  side of two hour boundaries tick three labels inside ninety minutes while sampling one
  time of day. A proxy you can satisfy by waiting for a clock to roll over measures the
  clock, not the network.

- **It sends ClearCodec and RFX Progressive over EGFX — not H.264, and not RemoteFX.**
  Measured, with AVC permitted on both sides and still unused. Neither codec is wired into
  IronRDP's client decode path, so decoding them is our work.

**Read both of these before designing anything that touches graphics:**
`wrk_docs/2026.08.14 - SPIKE - P1b server codec negotiation against temper.md` for what
this server actually sends, and `wrk_docs/2026.08.14 - INVENTORY - P1a IronRDP client
capability inventory.md` for what IronRDP can and cannot decode. The gap between those two
documents is the graphics work.

See `wrk_docs/2026.08.14 - SPIKE - P1 protocol posture against temper.md` for the
security-negotiation evidence.

Credentials for it live in the macOS keychain (service `mdrdp`), never in a config file,
a test fixture, an environment variable, or a commit. Read them through the `keyring`
crate. If you need a credential that is not there, stop and ask Arthur. The account
authenticates as a bare `user@example.com` — no `temper\` or `MicrosoftAccount\`
prefix.

**Driving FreeRDP for comparison work:** use `sdl-freerdp`, not `xfreerdp`. The latter is
the X11 client and fails instantly on this Mac (`failed to open display`; XQuartz is not
installed) — a failure that looks like a connection problem and is not.

## Version policy

Standard fleet rule: **integration owns exactly one version bump**; task branches never
pre-bump. Patch unless semver clearly calls for more.

## Security

Never log, print, or commit credentials, session contents, or clipboard payloads. The
clipboard path in particular handles arbitrary user data — no debug logging of contents,
only sizes and format IDs.
