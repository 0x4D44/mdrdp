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
- **Register CLIPRDR and RDPSND before the MCS join.** They are static channels and cannot
  be added to a live session. When audio is enabled, set `enable_audio_playback = true`:
  false sets `INFO_NOAUDIOPLAYBACK` and tells the server not to redirect audio.
- **One OS process per session.** No-args = favourites launcher; `mdrdp <host>` = one
  session window. The launcher spawns sessions as children. This buys crash isolation
  and independent clipboard state for free.
- **Create one winit `EventLoop` per process and reuse it with `run_app_on_demand`.** Create
  later windows from `new_events` / `about_to_wait`, and perform disconnect cleanup in
  `ApplicationHandler::exiting`; macOS Cmd+Q may terminate without `run_app` returning.
- **Portable by default, platform-specific only at the edges.** Core logic is
  `cfg`-free. Platform code is confined to named modules behind a trait
  (window policy, credential store, hardware video decode).
- **Follow settled display changes and user resizes.** Docking, undocking, or moving
  the session to a different monitor must adapt the remote resolution after the
  monitor and window settle. Fullscreen follows the current monitor; windowed mode
  follows the available inner window size. Sleep/reveal without a monitor change
  still preserves the chosen window geometry through `window_policy`. A user resize
  (window drag, fullscreen toggle) also renegotiates the session resolution. An
  explicit `--size` pins the resolution (drags then letterbox only), and Settings ▸
  Graphics ▸ Dynamic resolution off means letterbox always. (Dock/undock adaptation
  requested by Arthur on 2026-09-04; supersedes the blanket display-change exclusion.)
- **Choose the planned DPI scale in the GCC before logon.** A later RDPEDISP scale change
  makes Windows bitmap-stretch non-DPI-aware apps until they restart; the client cannot
  remove that server-side blur.
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
cargo test                  # unit + integration — does NOT cover vendor/
./scripts/test-vendored.sh  # the vendored crates' own suites
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

Focused first: `cargo test -p mdrdp <module>` before the whole suite. Run suites with
stdin closed — `cargo test </dev/null` — or a test that reads stdin hangs forever.

**Refresh the nested viewer lock after any root dependency change.**
`tools/latency-spike/viewer` is a separate workspace that depends on mdrdp by path, so a
root dependency changes its graph too. Run
`cargo metadata --manifest-path tools/latency-spike/viewer/Cargo.toml --offline
--format-version 1` in the same task; otherwise Deltic's locked metadata check fails during
integration.

**`cargo test` does not test `vendor/`.** Those crates arrive through
`[patch.crates-io]` as path dependencies, not workspace members, so cargo builds them as
libraries and never compiles their `#[cfg(test)]` code. `origin/main` sat red with a
failing `ironrdp-egfx` test while `cargo test` reported ~690 passing
(MDR-BUG-FLUX-00016). Run `./scripts/test-vendored.sh` when you touch anything under
`vendor/`. Two of those crates (`ironrdp-graphics`, `ironrdp-pdu`) have dev-dependencies
and cannot be tested this way at all — the script says so rather than pretending they
passed, and `Cargo.toml` records why making them workspace members is the wrong cure.

**Keep the Windows target compiling.** This repo mandates macOS *and* Windows, and a
Rust project drifts into macOS-only silently — nothing fails locally until someone tries
to build it. Run this after any change that touches platform-facing code (credentials,
paths, sockets, windowing):

```
./scripts/check-windows.sh
```

Verified clean as of 2026-08-16 with the full IronRDP + rustls + keyring stack. It is a
type-check, not a link — it will not catch a missing Windows-only runtime dependency, but
it does catch the `cfg` drift that actually happens (it caught a Windows-only
`eframe::raw_window_handle` import the same day it went green).

The script exists because `ring` compiles C, which needs Microsoft's CRT/UCRT headers
even for a type-check; on macOS those come from a one-time `xwin` provisioning step the
script's header documents (`brew install xwin`, splat to `~/.xwin`, symlink rustup's
`llvm-ar` as `llvm-lib`). Without that step the check dies in `ring`'s build script —
that is a missing toolchain, not `cfg` drift. Never re-enable the vendored connector's
`scard` feature to "fix" a Windows build: smart-card logon is out of scope and its
`winscard → flate2/zlib → libz-sys` subtree is what used to break this check.

## Validation rules specific to this repo

- **Never benchmark or latency-test a debug build.** Decode and colour conversion are
  10-30x slower unoptimised; a debug measurement is noise, not evidence.
- **A latency or throughput claim needs a number and the conditions it was taken under**
  (host, network, resolution, codec, build profile). "Feels fast" is not a result.
- **Quote a distribution, never one run.** A number that will be reasoned from later needs
  n, min, median and max, or it is an anecdote wearing a decimal point. On this WiFi LAN the
  spread is ~4x: mdrdp connect varies 61-238 ms across 8 consecutive runs
  (`connect::ConnectReport`). Twice a single sample has been published as a settled figure —
  the 4.20 ms latency floor, then a 142 ms connect time — and both were wrong enough to
  mislead.
- **Protocol changes need a real server.** A change that only passes against a mock has
  not been validated. Never substitute a mock or a Linux RDP server for protocol work.
- **Soak-class requirements need soak-class evidence.** The clipboard and
  latency-degradation requirements are about behaviour over hours. A green unit test
  does not discharge them.

## Connecting to a test host without breaking it

**Always disconnect gracefully.** Abandoning a connection leaves the Windows host holding
a session it does not reclaim promptly. On 2026-08-14 an evening of test connects — ours
plus two Gauntlet critics running eight each — ended with `temper` refusing to complete
any new logon: TCP and X.224 negotiation stayed perfectly healthy while everything after
that hung indefinitely.

**The signature, measured precisely:** TCP connects, X.224 still selects HYBRID_EX, and
**the TLS handshake completes normally** (23.5 ms, correct certificate) — then CredSSP
hangs forever. So the fault is in the host's logon path, not the network, not the
transport, and not the credential. `probe stages <host>` isolates this without sending a
credential or holding a session, so it is safe to run against a host that is already
refusing logons — which is exactly when you need it. The story is temper's, but the failure
mode is a Windows one and applies to quench equally.

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
(`com.microsoft.rdc.macos`, `/Applications/Windows App.app`) on this Mac. Confirmed
installed 2026-08-14.

Record its version *and which host it connected to* alongside any measurement taken against
it. A comparison against an unnamed version of the reference is not reproducible, it
updates itself, and quench and temper differ enough that a figure without a host name is
ambiguous.

## The test hosts

Two headless Windows boxes on the LAN, both on port 3389. They cannot reproduce physical
panel transitions such as idle power-off, backlight, lid, or dock changes; use a host with a
real panel before claiming coverage of that class. **ICMP is blocked on both** (Windows
Firewall default), so `ping` fails on a perfectly healthy host — test reachability with a
TCP connect to 3389 instead. Verified on quench 2026-08-17: ping 100% loss, tcp/3389 open.

### `quench` — our test box

`quench.lan.example`, `192.0.2.240`. Use this one for protocol work, live measurement and
day-to-day verification unless a task says otherwise.

- **Log in as `ano`**, which has sudo on the box. The password lives in the macOS keychain
  (service `mdrdp`) — never in a config file, a test fixture, an environment variable, or a
  commit. Read it through the `keyring` crate; if it is not there, stop and ask Arthur.
- **One session at a time.** A second connect kicks the current holder mid-run with reason
  "Another user connected" — this has cut validation runs off at 17-60 s. Check the fleet
  board or coordinate before a long measured run. The kick arrives as a graceful
  server-side Terminate whose reason mdrdp prints (`session.rs`, Terminate arm).
- **UK keyboard layout.** `autoinput`'s `type` maps ASCII to US scancodes, so `"` arrives
  as `@` and `\` as `#`. PowerShell registry paths accept forward slashes, which is the
  layout-safe escape.
- **It runs `DWMFRAMEINTERVAL=15` permanently** (set 2026-08-17): drag frame-gap p50 16.0 ms
  (~60 fps) against 31.8 ms before. Measurements taken before that date are on the old
  ~30 fps baseline and are not comparable to later ones.
- **It sends AVC444v2** (measured at 1920x1080).
- **No settled TCP RTT baseline yet.** Do not borrow temper's floor for it — different
  subnet, different hardware.

### `temper` — the second host, and the older evidence base

`temper.lan.example`, `192.0.2.171`. Still live, and the target of every spike document
written before 2026-08-16. It is measurably laggier than quench under AVC (typing p50
47.8 ms vs ~33 ms, p95 478 vs 87) and still runs the default ~30 fps frame cap, so the two
are **not** interchangeable for a latency number — always say which host a figure came
from.

- **It requires NLA** — it selects HYBRID_EX and rejects legacy RDP security outright.
  There is no unauthenticated path to a first pixel, so the credential path is on the
  critical path for the very first working connection, not a later hardening phase.
- **Its account authenticates as a bare `user@example.com`** — no `temper\` or
  `MicrosoftAccount\` prefix. Same keychain rule as quench.
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

- **It sends AVC444v2 by default since the host's H.264 policy was enabled** (measured
  2026-08-17; before that it sent ClearCodec + RFX Progressive, and the 2026.08.14 spike
  documents that era). `--no-avc` withholds the client's AVC capability and forces the
  old ClearCodec/RFX mix — measured 2026-08-17, that mode has a ~416 ms p50 typing
  round trip (vs ~48 ms under AVC444): the non-AVC server pipeline coalesces small
  updates aggressively, so prefer AVC for latency and use `--no-avc` for diagnosis only.

**Read both of these before designing anything that touches graphics:**
`wrk_docs/2026.08.14 - SPIKE - P1b server codec negotiation against temper.md` for what
that server actually sends, and `wrk_docs/2026.08.14 - INVENTORY - P1a IronRDP client
capability inventory.md` for what IronRDP can and cannot decode. The gap between those two
documents is the graphics work. Both predate quench and describe temper's pre-H.264 era —
read them for the client-side inventory and the method, not for what a server sends today.

See `wrk_docs/2026.08.14 - SPIKE - P1 protocol posture against temper.md` for the
security-negotiation evidence.

**Credentials for either host live in the macOS keychain** (service `mdrdp`), never in a
config file, a test fixture, an environment variable, or a commit. Read them through the
`keyring` crate. If you need a credential that is not there, stop and ask Arthur.

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
