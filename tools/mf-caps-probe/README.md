# mf-caps-probe

What a Windows host's Media Foundation **encoders** will actually accept — measured, not
inferred from the registry.

The question: for H.264 / HEVC / AV1 / VP9, which chroma formats and bit depths can this
box encode in hardware, at 1080p and at 5K? Enumeration lies in both directions. An MFT can
advertise a 4:4:4 input type and refuse the matching output profile; it can accept the
profile and then emit a bitstream whose SPS says 4:2:0. So the probe runs escalating tests
and prints all of them — the disagreements between them are the finding.

1. **Host** — CPU brand, Windows build, process session id, DXGI adapters.
2. **Inventory** — `MFTEnumEx` per output subtype, once with
   `HARDWARE|ASYNCMFT|SYNCMFT|SORTANDFILTER` and once with `ALL|SORTANDFILTER`, with each
   MFT's registered input subtypes. 4:4:4-capable inputs are marked `**like this**`.
3. **Acceptance** — `SetOutputType` per (profile, size), then the input types the encoder
   offers back for the profile it just accepted.
4. **Real encode** — 12 synthetic frames through the transform, and the chroma the **output
   bitstream** claims, parsed from the H.264/HEVC SPS, the AV1 sequence-header OBU, or the
   VP9 uncompressed header.
5. **Summary matrix** — one row per *encoder* × profile.
6. **D3D12 Video Encode capabilities** — `CheckFeatureSupport` against the API the Microsoft
   DX12 encoder MFTs wrap, per hardware adapter. No encoding, no session. This closes the
   hole section 4 leaves when a DX12 MFT accepts an output type and then refuses to encode,
   and it separates "no AV1 MFT is registered" from "this silicon cannot encode AV1".
   Sub-tests: (a) codec, (b) profile/level, (c) input-format matrix, (d) output-resolution
   limits, (e) full encoder-support query.

Nothing panics on a single failure: each attempt records its HRESULT and the probe carries
on. Every wait is bounded at 5 s, so a wedged driver costs one attempt, not the run. Exit
code is always 0.

## Build

```
./build.sh            # -> target/x86_64-pc-windows-msvc/release/mf-caps-probe.exe
```

Cross-compiles from macOS with the same xwin recipe as `tools/latency-spike/server`; see
that script's header for the one-time provisioning. On Windows the native MSVC toolchain is
used instead.

## Run

Copy the exe to the host and run it. No arguments, no configuration, no network.

## Two things that will bite you

**Session 0 hides the vendor hardware MFTs.** Over SSH (session 0) the Intel MFTs
*enumerate* perfectly and then every `ActivateObject` returns `E_FAIL 0x80004005`. Run it in
the console session to get hardware results:

```
schtasks /create /tn mfcapsprobe /tr "cmd /c C:\mdrdp\probe\mf-caps-probe.exe > C:\mdrdp\probe\out-interactive.txt 2>&1" /sc once /st 00:00 /it /rl highest /f
schtasks /run /tn mfcapsprobe
schtasks /delete /tn mfcapsprobe /f
```

An empty or software-only hardware section is far more likely to be the session than the
hardware. Both routes are worth capturing: the difference between them is itself a result.

**An `IMFActivate` is single-use.** `ShutdownObject` poisons it, and every later
`ActivateObject` on the same one returns `MF_E_INVALIDREQUEST 0xC00D36B2`. The probe records
each MFT's ordinal in the `ALL|SORTANDFILTER` enumeration and re-enumerates for every
attempt. Identity is (ordinal, friendly name), never CLSID — the DX12 and store-extension
encoders all publish an all-zero CLSID, so a CLSID-keyed dedupe silently drops every one
after the first.

## Reading the output

`accepted` and `encoded` answer different questions, and the gap between them is the point:
an encoder that accepts `Main_444_8` and then emits `chroma_format_idc=1` has silently
clamped to 4:2:0. Only the `bitstream says` column is evidence.

In section 6, test (c) is the decisive one — it is a direct per-(profile, format) answer
from the driver. In test (e), a row whose `ValidationFlags` say
`CODEC_CONFIGURATION_NOT_SUPPORTED` is **inconclusive, not negative**: the driver is
rejecting the codec configuration the probe supplied, so that row says nothing about the
chroma format. On Intel Arc/Xe the HEVC codec-configuration query
(`D3D12_FEATURE_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT`) answers neither struct revision
— `HEVC1` and `HEVC` both fail — so every HEVC row in (e) is currently inconclusive there.
H.264 completes cleanly, which is what shows the struct filling itself is correct.
