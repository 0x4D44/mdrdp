# tools/ — differential-test oracles and probes

Testing oracles for the parts of this client where "it looks right" is not evidence.
Each one answers a question our own unit tests cannot: *does our implementation agree
with an independent one?* For the graphics codecs that independent implementation is
FreeRDP, which is the only other client that decodes what `temper` and `quench` send.

**None of this is shipped code.** The repository root `Cargo.toml` has no `[workspace]`
table and each crate here declares its own empty `[workspace]`, so `cargo build` at the
root builds `mdrdp` and nothing else. Verified: `cargo metadata --no-deps` at the root
lists exactly one package.

These were written across several sessions in throwaway scratch directories and were
never committed. They were rescued on 2026-08-16 because they are the oracles for the
hardest open problem in this repo — see the RFX Progressive tile item in
`~/language/mdrdp/scratchpad.md` — and one `rm` from gone.

Read `tools/NOTICE` before copying any of this anywhere. Six files are near-verbatim
IronRDP, and five carry FreeRDP-derived logic.

---

## codec-oracles/

### `rlgrdiff/` — RLGR entropy stage, ours vs FreeRDP
**Live. The most reusable thing here.**

Embeds two RLGR1 decoders in one binary: ours (copied from
`vendor/ironrdp-graphics/src/rlgr.rs`) and `decode_ref`, a Rust transcription of
FreeRDP's `rfx_rlgr_decode`. Runs four seeded campaigns — random streams, encoder
round-trips, truncated streams, and long-run/high-magnitude stress.

```
cd tools/codec-oracles/rlgrdiff && cargo run --release
FIX_UQ=1 cargo run --release      # the interesting one — see below
```

**Read this before believing a result.** The default run reports `Test B ... ref!=orig
= 2831` of 3000. That is *not* a decoder fault: both decoders agree exactly
(`ours!=ref = 0`) and both disagree with the input, which places the fault in the
*encoder*. `vendor/ironrdp-graphics/src/rlgr.rs:125` adapts the RLGR1 zero quantum with
`UP_GR`, while its own decoder at line 322 — and FreeRDP — use `UQ_GR`. `FIX_UQ=1`
takes `ref!=orig` to 0. mdrdp is a client and never runs that encoder, so this is a
latent upstream wart, not a shipped defect; it is recorded in `scratchpad.md`.

So: unfixed, Tests B/C/D are decoder-agreement tests over malformed input. Run with
`FIX_UQ=1` if you want a correctness test.

Two further honest limits: RLGR3 is implemented in both decoders and never exercised,
and Test A reports 4417 cases where our decoder errors while FreeRDP truncates and
carries on — reported but never analysed.

### `dwtdiff/` — inverse DWT, ours vs FreeRDP
**Historical, but the oracle still works.**

Proved that the RFX Progressive inverse DWT matches FreeRDP bit-for-bit *only* if the
i32→i16 narrowing saturates rather than wraps. That fix landed (`d1d1b52`, "saturate the
inverse DWT") and is pinned by `the_dwt_narrowing_saturates_rather_than_wrapping`.

`ours.rs` truncates, `ours_clamped.rs` saturates, and they are otherwise the same file;
`ref.c` and `probe.c` are the FreeRDP side. No Cargo project — compile directly, and
send binaries somewhere outside the repo:

```
D=tools/codec-oracles/dwtdiff; O=$(mktemp -d "$TMPDIR/dwt.XXXXXX")
rustc -O $D/ours.rs -o $O/ours && rustc -O $D/ours_clamped.rs -o $O/ours_clamped
cc -O2 -std=c99 $D/ref.c -o $O/ref
```

Verified 2026-08-16: `ref.c` and `probe.c` both compile clean with `cc -O2`.

### `dwtcmp/` — the same question, one binary
**Historical.** Lifts the narrowing function `t` to a parameter so both behaviours run
from one program; `chk.rs` proves `t()` and `clampi16()` agree over 2M samples. Two
independent `fn main`s, no Cargo project:
`rustc -O tools/codec-oracles/dwtcmp/main.rs -o "$TMPDIR/dwtcmp"`.

### `refdec.c` — FreeRDP as ground truth for a captured frame
**Live, and the most directly useful file for the open tile bug.**

Decodes a captured RFX Progressive payload using FreeRDP's own `progressive_decompress`
and prints per-tile means. That is the reference the open scratchpad item is blocked on:
settling whether grid tile (11, 0) is *wrong* or merely *different* needs FreeRDP's
output for the same frame.

Needs FreeRDP 3 development headers — it is the one tool here that will not build from a
bare checkout:

```
cc -O2 -o "$TMPDIR/refdec" tools/codec-oracles/refdec.c $(pkg-config --cflags --libs freerdp3)
```

On this Mac `pkg-config` resolves to Homebrew's FreeRDP 3.27.1. Without those headers the
build fails with `freerdp/codec/progressive.h: file not found` — that is a missing
dependency, not a broken tool.

### `py/` — throwaway modelling scripts
Python 3 standard library only. Four model decode maths, four score screenshots.

| Script | Does |
| --- | --- |
| `srl_diff.py`, `srl_isolate.py` | Model FreeRDP's SRL reader against ours. **Load-bearing history** — their three-defect finding already landed in `vendor/ironrdp-graphics/src/progressive.rs`; do not re-derive it. |
| `dwt_diff.py` | Python model of `rfx_dwt_2d_decode_block` |
| `cmp.py` | FreeRDP's fixed-point YCbCr→RGB constants |
| `blocks.py`, `flat.py`, `compare.py`, `sat.py` | Parse BMPs and score them for artefacts; shell out to macOS `sips` |

Run from inside `py/` — `srl_isolate.py` imports `srl_diff`.

---

## surface-repro/

### `s2s/` — EGFX surface-to-surface repro
**Live, but for a different bug.** Touches no codec. A standalone reproduction showing
`SurfaceStore::surface_to_surface` failing on a clipped source rect, and partially
writing without bumping the generation counter. `cargo run --release` in that directory;
no dependencies at all.

`src/surface.rs` is a stale snapshot of `src/surface.rs` from the main crate — it will
drift. Re-diff before trusting a result.

---

## audio-probe/

### `cbtest/` — CoreAudio callback timing
**Live.** Nothing to do with codecs; it was filed alongside them by accident and is kept
because it is the cheapest way to re-measure what `src/audio.rs` assumes. Measures cpal's
output-callback rate, buffer size, and whether callbacks fire before `play()` — the
calibration behind mdrdp's underrun counting and the `RING_BUFFER_MS` choice. Needs a
real default output device. `cargo run --release`.

---

## The wart worth knowing

`dwtdiff/` stores the same 427-line IronRDP file three times, differing by one line and a
`main()`, and `ref.c`/`probe.c` duplicate 178 lines of FreeRDP-derived C between
themselves. That is how they were written under time pressure, and they were rescued as
they stood rather than refactored, because changing an oracle risks silently invalidating
the result it produced. If you extend them, fold the shared bodies into one file first —
`dwtcmp/main.rs` already shows the shape.
