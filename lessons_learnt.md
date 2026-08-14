<!-- lessons-format: index-v1 -->
# Lessons learnt

Newest at the top. The **first line of each entry is the lesson** — self-contained, with a
`file:symbol` pointer, under ~120 characters. Only first lines are injected at session
start, so a line that needs the detail below it to make sense is a line that will not work.

Soft target ~25 entries; past ~40, say it is due a prune rather than pruning unasked.

---

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
