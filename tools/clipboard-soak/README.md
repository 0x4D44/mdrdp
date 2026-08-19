# clipboard-soak

The long-run clipboard evidence rhydra tranche 5's AC8 asks for.

```
MDRDP=/abs/path/to/mdrdp clipboard-soak <host> --hours 8 --ssh-user ano --out report.json
```

Runs **one** `mdrdp --native` session and exercises the clipboard across it,
then reports what the run proved and — the part that matters — what it did not.

## Rules it follows, and why each is load-bearing

- **One session throughout.** It never reconnects and never rebuilds the
  clipboard handle. A harness that reconnects on trouble silently repairs the
  state under test and reports a healthy clipboard for a broken one.
- **Two consecutive misses in one direction stops the run.** Averaging a wedge
  over the remaining hours is how a soak reports 99.8% success on a clipboard
  that died at 02:00. Counters are per-direction: a clipboard working one way
  while the other is dead is exactly what a shared counter hides.
- **A competing session voids the run rather than failing it.** quench serves
  one viewer, so another agent connecting kicks us; that run measured a session
  that was not ours. Detection is a backstop — the board claim is the actual
  protection.
- **A short run is labelled, not rounded up.** Eight hours discharges; four to
  eight is `PROVISIONAL` and says so in the summary; below four proves nothing
  and says that too. The harness exits non-zero for anything it cannot claim.
- **Every payload is synthetic by construction** — a counter and a run prefix,
  never anything read from a real clipboard — which is what makes it safe to
  write a mismatching payload into the report.

## Two limits, stated rather than discovered later

**The latency figures are an upper bound, not a measurement.** Each arrival
check spawns `mdrdp --clipboard-check`, which spawns ssh, worth ~180 ms before
it asks anything. A smoke run measured `min=186 median=188 p95=406 max=406`;
those are the instrument. Good enough for "did it arrive, and did arrival
degrade over hours", and not to be quoted as clipboard latency.

**Only Mac→host is driven.** Setting the host's clipboard needs something
running in its console session — an ssh session is a different window station —
and a soak must not type on the desktop for hours. The missing piece is a small
host-side generator, launched once at session start, writing a predictable nonce
sequence the Mac can watch for. Until that exists, **AC8's "a transfer each way"
is half met**, and the report distinguishes `NotExercised` from a miss so the
gap is visible rather than counted as either success or failure.

## Smoke run

```
$ clipboard-soak quench.lan.example --hours 0.05 --ssh-user ano
attempted 6 (of 12 cycle slots; host->mac is not driven)
arrival latency: n=6 min=186ms median=188ms p95=406ms max=406ms
  (an UPPER BOUND — each check spawns ssh, worth ~180 ms before it asks anything)
client RSS: 276 MB -> 284 MB (growth 8 MB, threshold 100 MB)

TOO SHORT — 0.05 h is below the 4 h floor and proves nothing about a soak-class requirement.
$ echo $?
1
```

That last part is the point: the harness refuses to claim anything from a run
too short to support it.
