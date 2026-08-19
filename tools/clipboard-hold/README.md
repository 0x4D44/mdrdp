# clipboard-hold

Hold the Windows clipboard open for a bounded time, so another process's
`OpenClipboard` is refused on purpose. Built for rhydra tranche 5's AC4, which
asks for a live observation of what the clipboard path does when the OS refuses
it.

```
clipboard-hold.exe [seconds]     # default 10
```

It prints `opened=`, `held_secs=` and `closed=`, and exits non-zero if the hold
was not real. **That last part is the point.** An earlier PowerShell version
reported nothing but success while not actually holding the clipboard, and the
process under test wrote to it happily throughout — a test whose refusal never
happens measures nothing and looks exactly like a passing one.

Run it in the session whose clipboard you mean. An ssh session on Windows is a
different window station, so a hold there blocks nothing the console session
does.

## What it measured on quench, and why AC4's live half is unproven

Three runs, two implementations (PowerShell P/Invoke; this binary with a sleep;
this binary with a message pump), all on quench's console session:

```
opened=true
held_secs=30.00
closed=false error=Thread does not have a clipboard open. (0x8007058A)
```

The process is demonstrably alive for the whole interval — it reports 30.00
seconds and then calls `CloseClipboard` — and the lock is gone by the time it
does. Throughout, rhydra's capture server wrote to the clipboard successfully
(`write-failed 0`).

So **the clipboard lock is being broken on that host**, consistently, by
something other than the holder. Adding a message pump did not change it, which
rules out the most likely "badly behaved owner" explanation.

Two things follow:

- AC4's live clause "the thread reports the refusal" is **not producible this
  way on quench**, and was recorded as unproven rather than quietly passed.
- Incidentally, that bounds the real-world severity of the `EmptyClipboard`
  wedge on this host: the OS does not let a clipboard lock persist. That is
  encouraging, but it is an observation about one machine and not a guarantee to
  design against — the code still handles refusal, and the unit half proves it.
