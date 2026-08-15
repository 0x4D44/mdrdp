# Scratchpad

Out-of-scope observations. A separate human-invoked review triages these.

- [ ] 2026-08-15: two nearest-rank percentile implementations —
  `~/language/mdrdp/src/probe/stats.rs:30` (`percentile`, `&[u64]`, f64 fraction) and
  `~/language/mdrdp/src/stats.rs:127` (`nearest_rank`, `&[u32]`, u32 percent).
  Same statistic, two bodies. Both are pinned by hand-computed tests so they cannot silently
  disagree today, which is why this is parked rather than fixed. Unify behind one generic
  helper if a third caller appears, or if either grows past the plain formula.
