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

**Keep the Windows target compiling.** This repo mandates macOS *and* Windows, and a
Rust project drifts into macOS-only silently — nothing fails locally until someone tries
to build it. Run this after any change that touches platform-facing code (credentials,
paths, sockets, windowing):

```
rustup target add x86_64-pc-windows-msvc   # once
cargo check --target x86_64-pc-windows-msvc --all-targets
```

Verified clean as of 2026-08-14 with the full IronRDP + rustls + keyring stack. It is a
type-check, not a link — it will not catch a missing Windows-only runtime dependency, but
it does catch the `cfg` drift that actually happens.

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

## Connecting to the test host without breaking it

**Always disconnect gracefully.** Abandoning a connection leaves the Windows host holding
a session it does not reclaim promptly. On 2026-08-14 an evening of test connects — ours
plus two Gauntlet critics running eight each — ended with `temper` refusing to complete
any new logon: TCP and X.224 negotiation stayed perfectly healthy while everything after
that hung indefinitely.

**The signature, measured precisely:** TCP connects, X.224 still selects HYBRID_EX, and
**the TLS handshake completes normally** (23.5 ms, correct certificate) — then CredSSP
hangs forever. So the fault is in the host's logon path, not the network, not the
transport, and not the credential. `probe stages temper` isolates this without sending a
credential or holding a session, so it is safe to run against a host that is already
refusing logons — which is exactly when you need it.

It did not clear on its own over 20 minutes of retries; assume it needs a reboot or a
session kick.

`connect` sends a Shutdown Request before exiting and reports `graceful_shutdown` in its
output. Anything that connects in a loop must do the same.

**Budget live connects.** Verification that hammers the host degrades the thing every
later measurement depends on. Prefer a handful of runs with a pause between them over a
tight loop, and treat "the host stopped completing logons" as a signal to stop and let it
settle rather than to retry harder.

## The reference client

Every Gauntlet comparison is against **Microsoft's Windows App 11.3.8**
(`com.microsoft.rdc.macos`, `/Applications/Windows App.app`) on this Mac, connecting to
`temper`. Confirmed installed 2026-08-14.

Record its version alongside any measurement taken against it. A comparison against an
unnamed version of the reference is not reproducible, and it updates itself.

## The test host

`temper` (`temper.lan.example`, `192.0.2.171`) on the LAN, port 3389. It is the reference
target for all protocol work and all comparisons against Microsoft's client.

- **ICMP is blocked** (Windows Firewall default). `ping temper` fails on a perfectly
  healthy host — test reachability with a TCP connect to 3389 instead.
- **It requires NLA** — it selects HYBRID_EX and rejects legacy RDP security outright.
  There is no unauthenticated path to a first pixel, so the credential path is on the
  critical path for the very first working connection, not a later hardening phase.
- **Latency floor: TCP RTT p50 3.31 ms**, 95% CI [3.29, 3.34], n=4000 over 4 distinct UTC
  hours spanning 6.6 h (p95 4.72 ms, **p99 10.82 ms, max 119.9 ms**). Settled — the
  coverage requirement is met.

  **Quote the tail, not just the median.** The afternoon-only sample this replaced showed
  p99 5.84 ms; with evening traffic included p99 is 10.82 ms and the worst sample is
  119.9 ms — a 36x outlier on a "3.3 ms" link. Any latency budget built on the median
  alone will be wrong in exactly the conditions users complain about.

  Regenerate with `probe rtt temper --samples 1000 --out baseline/temper-rtt.jsonl` at
  genuinely separated times, then `probe summarise`. The rule requires a ≥6 h wall-clock
  span, not merely ≥3 distinct UTC hour labels — three short batches either side of two
  hour boundaries tick three labels inside ninety minutes while sampling one time of day.
  A proxy you can satisfy by waiting for a clock to roll over measures the clock.

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

**Never `{:?}` an `ironrdp::connector::Config` or `Credentials`.** Both
`#[derive(Debug)]` upstream, so either one debug-printed renders the password in
plaintext. `crate::creds::Secret` exists precisely so the password cannot print itself;
that protection ends the moment the value is handed to IronRDP. Our own `ConnectReport`
carries no credential field and must stay that way.
