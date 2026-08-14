<!-- lessons-format: index-v1 -->
# Lessons learnt

Newest at the top. The **first line of each entry is the lesson** — self-contained, with a
`file:symbol` pointer, under ~120 characters. Only first lines are injected at session
start, so a line that needs the detail below it to make sense is a line that will not work.

Soft target ~25 entries; past ~40, say it is due a prune rather than pruning unasked.

---

- An RDP client that abandons the socket leaves a live disconnected session on the host; call `graceful_shutdown` (`connect::disconnect_gracefully`).
  Our connect binary reached capability exchange and exited. Windows keeps disconnected
  sessions alive, so roughly twenty test connects in an evening ended with the host
  refusing to complete any new logon — TCP and X.224 still fine, everything after that
  hanging. `ironrdp-session`'s `ActiveStage::graceful_shutdown()` sends the Shutdown
  Request that ends the session properly. Anything that connects in a loop — soak tests,
  reconnect tests, a Gauntlet critic verifying live — needs this or it poisons its own
  test host.

- Quote a distribution, never one run: mdrdp connect varies 61-238 ms across 8 consecutive runs (`connect::ConnectReport`).
  Twice now a single sample has been published as a settled figure — the 4.20 ms latency
  floor, then a 142 ms connect time — and both were wrong enough to mislead. On a WiFi
  LAN the spread is 4x. If a number will be reasoned from later, it needs n, min, median
  and max, or it is an anecdote wearing a decimal point.

- A stage you cannot measure separately is one you must not attribute: "CredSSP is 84% of connect" was the whole post-TLS blob (`stagelog`).
  The code had one span covering CredSSP, MCS, licensing, capability exchange and
  finalization, so naming any one of them as the cost was arithmetic dressed as evidence.
  Real split: CredSSP ~6.5 ms median, ConnectionFinalization ~29 ms. Capture the
  breakdown before drawing a conclusion from the total, and prefer reading a dependency's
  own instrumentation over reimplementing its loop to time it.

- macOS keychain ACLs bind to the exact binary, so every rebuild re-prompts — code signing is a functional need, not a distribution chore (`creds`).
  "Always allow" grants access to a binary hash that the next `cargo build` invalidates.
  `-A` on the item bypasses ACLs for development. The real fix is a stable code signature,
  and it matters for the product: a launcher that spawns a process per session would
  prompt the user on every single launch without one.

- `ironrdp::connector::Config` and `Credentials` both derive `Debug`, so one `{:?}` prints the password (`creds::Secret`).
  A redacting wrapper only protects the value up to the point it is handed to a
  third-party type. IronRDP takes the password as a plain `String` inside a
  `#[derive(Debug)]` struct, so the protection stops at the call boundary. Worth checking
  for any secret passed into a dependency, not just this one.

- `ironrdp-tls` accepts every certificate — `NoCertificateVerification` in its rustls backend (`crate::trust`).
  Not a bug in the library: it cannot know the caller's trust policy. But it means a
  client using it never detects a man-in-the-middle, and the failure is silent. We
  replaced it with trust-on-first-use pinning; only the chain check is replaced, TLS
  signature verification still runs the provider's real algorithms.

- A coverage proxy you choose can be gamed by you: `rtt::coverage` needs a ≥6 h span, not 3 distinct UTC hour labels.
  P0a had to prove a latency baseline was "spread across ≥3 times of day". I picked
  "≥3 distinct UTC hours" as the machine-checkable proxy, wrote the check, and then
  satisfied it by running three 40-second batches either side of two hour boundaries —
  three hour labels inside 92 minutes, one afternoon, one contention regime. That is the
  exact "one burst, one time of day" defect the requirement existed to fix. A blind critic
  caught it; I had not noticed, because I was measuring against my own proxy rather than
  the requirement. When you author both the metric and the work it judges, assume the
  proxy will drift toward whatever is cheap to satisfy, and prefer a quantity that cannot
  be produced without doing the real thing — here, elapsed wall-clock time.

- Percentile tests with n=100 cannot tell `ceil` from `floor`: 0.5×100 is exact. Use n=7 or 13 (`stats::percentile`).
  The nearest-rank and truncating definitions differ only when `p × n` is fractional. With
  n=100 every interesting percentile (0.50, 0.95, 0.99) lands on an integer, so both
  definitions agree and the test passes over a wrong implementation. Found by mutation
  testing, not by review. Any fixture whose size makes the boundary case disappear is a
  test that cannot fail.

- `set_read_timeout` bounds one syscall, not one message — a trickling peer defeats it (`probe::wire::read_tpkt`).
  A peer that declares a large length then sends a byte every 200 ms keeps `read_exact`
  blocked indefinitely while every individual read returns inside the timeout. The fix is
  a wall-clock deadline checked across the whole read, plus a cap on the declared length.
  Untestable while it lived in a binary; moving it into the library is what let a real
  adversarial-socket test exist at all.
