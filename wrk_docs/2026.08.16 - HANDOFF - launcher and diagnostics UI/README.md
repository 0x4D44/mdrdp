# Handoff: mdrdp launcher, connection wizard, settings and diagnostics

## Overview

mdrdp today opens a 560×640 software-rendered window drawn by a hand-rolled toolkit
(`src/ui/font.rs` — an 8×16 bitmap font — plus `fill_rect`/`stroke_rect` in `src/ui/form.rs`).
This handoff replaces that entire surface with a 900×700 window carrying a real menu bar:

- a **connection wizard** (three steps) that is the front page on first run,
- a **connections list** that is the front page once anything is saved,
- a **settings** modal with six panes,
- three **diagnostics windows** owned by each session process — bitmap cache, latency and
  drift, channels and codecs,
- **twelve dialogs** covering connect progress, failures, certificate pinning, credential
  store problems, edit/remove, session end, quit and About,
- **toasts** plus one extra line in the existing `Ctrl+Alt+S` overlay for transient failures.

The command line stays first-class throughout: the wizard shows the equivalent
`mdrdp <host>` invocation, and Settings ▸ Defaults is explicitly the source of the values
that invocation uses.

## About the design files

The `*.dc.html` files in this bundle are **design references written in HTML** — prototypes
that show the intended look, geometry and behaviour. They are not production code and
nothing should be copied out of them verbatim.

The target is the existing **Rust** codebase (winit + softbuffer today). Recreate these
designs natively there. The toolkit decision is settled as **iced or egui** (either is
acceptable — see Decisions), with **muda** for the menu bar. Read this README rather than
the HTML for values; the HTML is for looking at.

Fonts in the mocks are **IBM Plex Sans** and **IBM Plex Mono** (SIL Open Font License).
Both need embedding in the binary — egui and iced each want the `.ttf` bytes registered at
startup; neither will find a system copy reliably on Windows.

## Fidelity

**High fidelity.** Colours, sizes, spacing, type and copy below are final and exact.
Recreate them as specified. Where a value is not stated, follow the nearest stated one
rather than inventing a new step.

## Decisions already settled

1. **Toolkit** — iced or egui, implementer's choice. The "no UI framework" position in
   `src/ui/mod.rs`'s module doc is superseded; that module can be deleted along with
   `form.rs` and `list.rs` once the new shell renders.
2. **Menus** — `muda`. macOS puts the items in the system menu bar; Windows puts the same
   items in the window's own menu bar. The in-window strip drawn in the mocks *is* the
   Windows presentation; on macOS it is hidden and nothing else in the layout moves.
3. **No cross-process diagnostics.** Each session process is an island and owns its own
   diagnostics windows. The launcher's menus are File / Connection / View / Help only.
   Diagnostics windows are opened from the *session* window's menu bar.
4. **Per-slot cache instrumentation is in scope** (see New instrumentation).
5. **Settings persist to a new `settings.toml`**, a sibling of `favourites.toml`, written
   through the same atomic replace `favourites.rs` already uses. Deliberately a separate
   file so a settings write can never rewrite the favourites list.
6. **`trust.rs` gains a UI hook** that blocks the connecting thread and returns
   accept-once / pin-and-accept / reject.

## Design tokens

### Colour

| Token | Hex | Use |
| --- | --- | --- |
| `bg.window` | `#14161A` | window and modal background |
| `bg.chrome` | `#0E1013` | menu bar, footers, inputs, inset panels |
| `bg.panel` | `#171A1E` | sidebars, secondary panels |
| `bg.raised` | `#1E2126` | cards, list rows, toasts, empty cache slot |
| `bg.row.selected` | `#22262C` | selected list row, active menu title |
| `line.hair` | `#262B31` | dividers between structural areas |
| `line.subtle` | `#2F353D` | window border, quiet input border |
| `line.strong` | `#414A54` | modal border, secondary button border |
| `text.primary` | `#EAEEF3` | headings, values |
| `text.secondary` | `#A6AEB9` | body copy, menu items |
| `text.muted` | `#6F7883` | labels, captions |
| `text.dim` | `#545D68` | metadata, unit suffixes |
| `accent` | `#2BE07A` | primary action, focus ring, live state |
| `accent.on` | `#04150B` | text on `accent` |
| `accent.fill` | `#17603C` | selected segment / sidebar item / menu item |
| `accent.tint` | `#0D2417` | completed step chip background |
| `warn` | `#FFC93C` | pixel-share figure, p99, degraded channel, warning bar |
| `danger` | `#FF5D4D` | evictions, errors, destructive action |
| `danger.on` | `#1A0B08` | text on `danger` |
| `danger.bg` | `#2A1518` | error icon chip |
| `danger.border` | `#7A2A26` | danger modal border |
| `danger.text` | `#FF8B7A` | danger modal heading |
| `danger.body` | `#D3AAB0` | body copy inside a danger/warning card |
| `info` | `#3B8CFF` | baseline reference line |
| `cyan` | `#24D8E0` | second codec bar |
| `scrim` | `#05070A` @ 62% | modal backdrop; content behind also drops to 25% opacity |

### Heat ramp (bitmap cache)

Continuous linear interpolation in sRGB between eight anchors, `t ∈ [0,1]`:

```
0.00 rgb( 46, 59,255)   0.48 rgb( 43,224,122)
0.16 rgb( 59,140,255)   0.62 rgb(185,245, 60)
0.32 rgb( 36,216,224)   0.76 rgb(255,224, 77)
0.88 rgb(255,154, 46)   1.00 rgb(255, 45, 45)
```

Empty slots are flat `#1E2126` and never take a ramp colour. Red appears only at the hot
end of the ramp and for evictions/destructive actions — never for anything neutral.

### Type

Both families, weights 400 / 500 / 600.

| Role | Font | Size / line-height | Other |
| --- | --- | --- | --- |
| Screen title | Plex Sans 600 | 24px | `letter-spacing:-0.01em` |
| Window title (diagnostics, modal) | Plex Sans 600 | 15–16px | |
| Section heading | Plex Sans 600 | 16px | |
| Body | Plex Sans 400 | 13px / 20–21px | `text-wrap: pretty` |
| Caption | Plex Sans 400 | 12px / 19–20px | |
| Field label | Plex Sans 500 | 11px | `uppercase`, `letter-spacing:0.08–0.09em` |
| Section label | Plex Mono 400 | 11px | `uppercase`, `letter-spacing:0.14em` |
| Wordmark | Plex Mono 600 | 11px | `uppercase`, `letter-spacing:0.14em`, `#2BE07A` |
| Value / identifier / path | Plex Mono 400 | 12–13px | |
| Focused input value | Plex Mono 400 | 14–15px | |
| Headline metric | Plex Mono 500 | 30px (cache) / 34–38px (latency) | `line-height:1.05–1.15` |
| Metric tile value | Plex Mono 400 | 20px | |

Anything that is a hostname, port, account, path, fingerprint, byte count, duration,
percentage, stage name or channel name is **monospace**. Prose is never monospace.

### Spacing, radii, shadow

- Spacing steps in use: 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16, 18, 20, 22, 24, 26, 28, 32, 40, 56.
- Radii: 2px cache cells and legend chips · 3px menu items and small chips · 4px inputs,
  buttons, segments · 5px cards, toasts, popovers · 6px window and inset plot · 7px modal
  · 10px toggle track · full circle for step chips and status dots.
- Shadows: modal `0 24px 60px rgba(0,0,0,0.60)` (settings `0.55`) · popover
  `0 16px 40px rgba(0,0,0,0.50)` · toast `0 12px 30px rgba(0,0,0,0.45)`.
- Hit targets: buttons 34–38px tall, menu items 28px, list rows 64px, cache cells 18px
  (pointer-only, with an 18px hover ring).

### Shared chrome

**Menu bar** — 30px tall, `bg.chrome`, 1px `line.hair` bottom border, `padding:0 10px`,
items in a 2px-gap row. Wordmark `MDRDP` with 12px right padding. Item: 12px
`text.secondary`, `padding:4px 9px`, radius 3. Open/active item: `text.primary` on
`bg.row.selected`.

- Launcher window: `File · Connection · View · Help`
- Session and diagnostics windows: `Session · View · Diagnostics · Help`

**Menu popover** — `bg.raised`, 1px `line.strong`, radius 5, `padding:5px`, 1px gaps.
Items 28px tall, `padding:0 11px`, radius 3, 13px `text.secondary`; highlighted item on
`accent.fill` with `text.primary`. Shortcut hint right-aligned, Plex Mono 11px `text.dim`.
Separator 1px `line.subtle` with `margin:5px 0`.

**Buttons** — primary: 38px (34px in dialogs), `padding:0 18–24px`, radius 4, `accent`
fill, 13px/600 `accent.on`. Secondary: same box, transparent, 1px `line.strong`, 13px
`text.secondary`. Destructive: `danger` fill, 13px/600 `danger.on`. Footers align buttons
right with 10–12px gaps.

**Inputs** — default 34–38px tall, `bg.chrome`, 1px `line.subtle`, radius 4,
`padding:0 11–12px`, Plex Mono 13px `text.primary`. Focused: 44px in the wizard, 1px
`accent` border, Plex Mono 15px, plus a 1px × 18–20px `accent` caret at the text end.
Disabled/derived value: `text.dim` on a `line.hair` border. Placeholder: `text.dim`.

**Toggle** — 34×19 track, radius 10, 15px knob with 2px inset. On: `accent` track,
`accent.on` knob, knob right. Off: `line.subtle` track, `text.muted` knob, knob left.

**Checkbox** — 15–16px, radius 3. Checked: `accent` fill with a 10–11px/600 `accent.on`
check. Unchecked: 1px `line.strong`, transparent.

**Segmented control** — either bare (selected segment `accent.fill` + `text.primary`,
others 1px `line.subtle` + `text.muted`, 28–34px tall, `padding:0 11–14px`, radius 4,
6–8px gap) or grouped in a track (`bg.chrome`, 1px `line.hair`, radius 4, `padding:3px`,
segments `padding:5px 11px`, radius 3, 12px, `white-space:nowrap`).

## Screens

All windows are **900×700** with a 1px `line.subtle` border and 6px radius. Heights below
sum exactly to 700; keep them fixed rather than letting content decide.

### 1. Connection wizard (front page when `favourites.toml` has no entries)

Layout per step: menu bar 30 · header block `padding:34px 56px 0` (title 24px, sub 13px
`text.muted`) · step rail `padding:30px 56px 0` · body `flex:1`, `padding:36–40px 56px`,
gap 22–26 · footer 72px.

**Step rail** — 22px circular chips with 14px gaps and `flex:1` 1px `line.subtle`
connectors between them. Completed: `accent.tint` fill, 1px `accent.fill` border, `accent`
check, label `text.secondary` showing the *answer* (`temper:3389`, `alice`). Current:
`accent` fill, `accent.on` numeral, label 13px/500 `text.primary`. Pending: 1px
`line.strong` border, `text.muted` numeral and label.

**Step 1 — Destination.** Title "Set up a connection", sub "No saved connections yet.
This takes three screens — or run `mdrdp <host>` in a terminal and skip it entirely."
Fields: Host (focused, 44px, full width) with a resolution hint below in 12px
`text.muted`; then a row of Display name (`flex:1`, hint "Defaults to the host name") and
Port (140px, hint "Default"). Bottom card, pinned with `margin-top:auto`: `bg.chrome`, 1px
`line.hair`, radius 5, `padding:16px 18px`, label "SAME THING FROM A TERMINAL" 11px
uppercase `text.dim`, then `$ mdrdp temper` in Plex Mono 13px `text.secondary` with the
`$` in `accent`.

**Step 2 — Sign in.** Title "Sign in to temper", sub naming the credential store per
platform and the account key `alice@temper:3389` in mono. Row: Username (focused, hint
"From Settings › Defaults") + Domain 220px, labelled `DOMAIN` with a lowercase
non-tracked "optional" suffix in `text.dim`. Password field full width, 44px, bullets in
Plex Mono 15px `letter-spacing:0.15em`, "Show" affordance right in 12px `text.muted`.
Checkbox row (checked): "Save the password to the system credential store" with sub
"Unchecked, mdrdp asks for it each time and keeps it only for that session." Bottom
warning card: `#1C1418` background, 1px `#48232B`, radius 5, `padding:14px 16px`, `!` in
`danger`, body 12px/20px `danger.body` — first-connection fingerprint notice.

**Step 3 — Display.** Title "How the desktop appears", sub "The remote resolution stays
fixed at the size chosen here for the whole session." Two selectable cards, 16px gap:
Fullscreen (selected — `bg.raised`, 1px `accent`, radius 5, `padding:18px`) and Explicit
size (1px `line.subtle`, `text.secondary` heading). Each holds a 76px `bg.chrome` preview
strip; the selected one shows a 150×56 `accent.fill` block at 55% opacity, the other two
34px numeric inputs either side of a mono `×`. Card sub-copy 12px/19px. Then a summary
block (`bg.chrome`, 1px `line.hair`, radius 5, `padding:18px 20px`): label "READY TO
CONNECT", a 44px-gap row of HOST / ACCOUNT / DISPLAY / PASSWORD (11px `text.dim` labels
over 13px mono values, PASSWORD reading `keychain` in `accent`), a 1px divider, then
`$ mdrdp temper --user alice`. Finally a checked checkbox "Save as a favourite named
`Temper`".

Footers: left `Step N of 3` 13px `text.dim`; right Cancel/Back secondary + primary
`Continue`, `Save and connect` on step 3.

### 2. Connections list (front page when favourites exist)

Menu bar 30 · header `padding:26px 28px 18px` with title "Connections" 20px/600 and sub
"Double-click to connect · Enter opens the selection · sessions run independently", plus
right-aligned `Edit` secondary (32px) and `New connection` primary (32px) · list `flex:1`,
`padding:0 28px 20px`, 8px gaps · status bar 38px, `bg.chrome`, 1px `line.hair` top,
`padding:0 28px`, Plex Mono 11px `text.dim`: favourites.toml path left, "1 session
running" right.

Rows are 64px, radius 5, `padding:0 18px`, name 14px/500 `text.primary` over
`user @ host[:port] · window size` in Plex Mono 12px `text.muted`.
Default row: `bg.raised` with 1px `line.hair`. Selected row: `bg.row.selected`, 1px
`line.subtle`, **2px `accent` left border**. A row whose session is running shows
`connected 12m` in Plex Mono 11px `accent` plus a 6px `accent` dot; others show
`last used …` in `text.dim`.

Double-click, or Enter on the selection, connects. `N` opens the wizard.

### 3. Settings (modal over the list)

720×580, placed at x=90 y=60 inside the 900×700 window; backdrop as per `scrim`.
Title row 52px with "Settings" and a `×`. Sidebar 186px (`bg.panel`, 1px `line.hair`
right, `padding:12px 10px`, items 32px, radius 4, 13px; selected on `accent.fill`);
bottom of the sidebar shows `favourites.toml`, its directory in Plex Mono 10px/15px
`text.dim` with `word-break:break-all`, and a `Reveal` action. Pane `padding:24px 26px`,
gap 22, rows `label 150px + control`. Footer 64px: "Changes apply to new sessions." left,
Cancel + Save right.

Panes and contents:

- **Defaults** — Username (text), Port (110px), Session window (segmented
  Fullscreen/Explicit), Explicit size (two 88px inputs, dimmed while Fullscreen).
  Divider, then "ON CONNECT": toggle *Keep the launcher open* (on) — "Each session runs in
  its own process, so several can run at once."; toggle *Reconnect the last session on
  launch* (off).
- **Graphics** — checkboxes ClearCodec (on), RFX Progressive (on), Allow uncompressed
  fallback (off); toggle Dynamic resolution on window resize (on).
- **Audio** — playback on/off, output device selector.
- **Clipboard** — Direction (segmented: Both ways / To remote / Off), Max image (96px,
  `1 MiB`), Transfer timeout (96px, `5 s`), note "A transfer that times out is abandoned so
  later transfers still work."
- **Diagnostics** — toggle Show the stats overlay on connect (off), Metrics JSON directory,
  Stage log (segmented Off / Stages / Verbose), note "Reports carry no host, account,
  credential, path, clipboard or pixel data."
- **Certificate trust** — one row per pinned host: `host:port` in mono 12px over
  `SHA-256 9f:2c:…:b7 · pinned 14 Aug` in mono 11px `text.dim`, with a `Forget` action in
  `danger` on the right; rows separated by 1px `line.hair`. Note "Trust on first use. A
  changed fingerprint stops the connection and asks."

### 4. Bitmap cache (session process window)

Heights: menu bar 30 · header 54 · headline band 96 · body 520.

**Header** — `padding:0 24px`: title "Bitmap cache" 15px/600, 1px×18 `line.subtle`
divider, then the session label (6px `accent` dot + `Temper` mono 12px `text.primary` +
`alice@temper:3389 · pid 4821` mono 12px `text.dim`). Right: `refresh 1s` mono 11px
`text.dim` and a `Write metrics JSON` secondary button (26px).

**Headline band** — three cells with 1px `line.hair` separators, all `box-sizing:border-box`:
210px *Hit rate* (mono 30px `text.primary`, sub `2043 hits / 8 misses`), 230px *Pixels from
cache* (mono 30px `warn`, sub `214.0 MiB of 349.0 MiB painted`), then a `flex:1` cell with
the 12px/20px `text.muted` explanation: "A hit on a 32×32 tile counts the same as a hit on
a 448×448 one, so hit rate flatters the cache. The pixel share is what it actually saved.
When these two disagree the cache is not earning its keep."

**Body, left pane** (`flex:1`, `padding:18px 20px 20px 24px`, gap 14) — a metric segmented
control (`Served · Hits · Recency · Return · Occupancy`, grouped-track style, `flex:none`)
with the grid caption right (`504 slots · 314 live · 50 evicted`, mono 11px `text.dim`,
`white-space:nowrap`, 16px left margin). Then the grid: `display:grid`,
`grid-template-columns:repeat(27, 18px)`, 2px gap, `align-content:start` — 18px cells,
radius 2. Hover `box-shadow:0 0 0 1px #EAEEF3`; selected `0 0 0 2px #EAEEF3`. Legend pinned
with `margin-top:auto`: low label, a 210×9 radius-2 bar carrying the ramp as a
`linear-gradient(90deg …)` at the anchor stops above, high label, a 1px×14 divider, a 14×9
`#1E2126` chip, and "empty slot · never filled". Labels 11px `text.dim`, and they change
with the metric:

| Metric | Low label | High label | Value |
| --- | --- | --- | --- |
| Served | no bytes served | carrying the session | rank percentile of `bytes_served` among filled slots |
| Hits | never hit | hit constantly | `log1p(hits) / log1p(max hits)` |
| Recency | stale | hit just now | `1 − min(1, age_ms / 180000)` |
| Return | stored, barely used | paid back ×20 | `min(1, log1p(served/stored) / log1p(20))` |
| Occupancy | evicted | live entry | live → `accent`, evicted → `danger` (no ramp) |

Served is **rank-binned deliberately**: a handful of 448×448 slots otherwise pin every
other filled slot into the top two stops and the legend advertises a scale the view never
uses.

**Body, right sidebar** — 300px, `box-sizing:border-box`, `bg.panel`, 1px `line.hair` left,
`padding:16px 20px`, 10px gaps. Three sections separated by 1px dividers:

1. `SLOT 229` with the slot's state right-aligned in its state colour (live `accent`,
   evicted `danger`, empty `text.dim`), then label/value rows: Size (`448×448`), Codec in,
   Hits, Served, Stored, Return (`×11.7`, `accent`), Last hit (`1.4s ago`).
2. `TOTALS`: Entries (`314 / 504`), Evictions (`danger`), Served, Stored, From wire.
3. `CODEC IN`: three bars — ClearCodec 61% `accent`, RFX Progressive 34% `cyan`,
   Uncompressed 5% `warn`. Track `bg.raised`, 5px tall, radius 3, with the percentage in
   mono 11px `text.muted` above right.

Clicking any cell selects it and fills section 1. Default selection is the highest-served
live slot, never an empty one.

### 5. Latency and drift (session process window)

Menu bar 30 · header 54 (as the cache window, right side `n = 2,148 · window 512`) ·
metric row `padding:24px 24px 0` · chart area `flex:1`, `padding:24px`.

Metric row: a `flex:1` drift card (`bg.chrome`, 1px `line.hair`, radius 6,
`padding:18px 20px`) with label "DRIFT SINCE CONNECT", value Plex Mono 38px/500 `accent`
(`+0.4`) + `ms` in 14px `text.muted`, and the sub "Median now, less the baseline frozen
from the first 100 samples. Never re-baselined."; beside it a `flex:1` 2×2 grid of tiles
(same card style, `padding:14px 16px`) for p50, p95, p99 (`warn`), max.

Chart: section label "ROUND TRIP, LAST 512 SAMPLES" with a legend right — 14×2 `info`
swatch "baseline p50 7.8 ms" and 14×2 `warn` swatch "p95". Plot is `bg.chrome`, 1px
`line.hair`, radius 6, `padding:16px`; bars fill the content box with 2px gaps, radius 1,
coloured `accent` under p95, `warn` between p95 and p99, `danger` above p99.
**Both reference lines must be derived from the same full-scale as the bar heights** (the
mock uses 45 ms full-scale, so baseline sits at 17.3% and p95 at 31.3% from the bottom of
the plot's *content* box) — an eyeballed offset makes the chart contradict the drift
figure, which is the one number this window exists to show. Below: `min 5.1 ms · exact
percentiles, sorted per read` left, `Write metrics JSON` secondary right.

### 6. Channels and codecs (session process window)

Menu bar 30 · header 54 (right side `0 decode errors · 0 undecoded regions · 0 unhandled
PDUs` in mono 11px `accent`, switching to `danger` when non-zero) · body `flex:1`.

Left pane (`padding:22px 20px 22px 24px`, gap 20): section "STATIC AND DYNAMIC CHANNELS" —
rows of `padding:9px 0` separated by 1px `#1E2126`, each a 6px status dot (`accent` joined,
`warn` joined-but-idle, `line.subtle` not requested), the channel name in mono 13px on an
88px column, a 12px `text.secondary` description, and a mono 12px `text.dim` counter right.
Rows: DRDYNVC (2 DVCs open), CLIPRDR (14 transfers · 0 timeouts), RDPSND (222 packets),
RDPDR (idle), AINPUT (not requested · also ECHO, RAIL). Then section "SURFACE CODECS" —
three labelled 6px bars with update counts and painted bytes.

Right sidebar 300px (`bg.panel`, `padding:20px`, gap 14): "CONNECT TIMELINE" listing the
stage names exactly as `stagelog.rs` records them — `tcp_connect` 3 ms,
`x224_negotiation` 6 ms, `tls_handshake` 41 ms, `Credssp` 118 ms (`warn`, as the slowest),
`LicensingExchange` 12 ms, `CapabilitiesExchange` 9 ms, `post_tls_sequence` 4 ms,
`egfx_observation` 62 ms — then Total to first frame 255 ms in `accent`; divider;
"PROCESS" with CPU average, Peak resident, Frames, Bytes in; and a closing note pinned with
`margin-top:auto`.

### 7. Dialogs

Shared: `bg.window`, 1px `line.strong`, radius 7, modal shadow. Body
`padding:22px 24px 18px`, gap 12–14. Footer `padding:14px 24px`, `bg.chrome`, 1px
`line.hair` top, buttons right at 34px. Error dialogs lead with a 22px circular
`danger.bg` chip holding a `danger` `!`. Danger variants add a 1px `danger.border` frame
and a 4px `danger` bar across the top; warning variants a 4px `warn` bar.

| Dialog | Width | Primary | Notes |
| --- | --- | --- | --- |
| Connecting | 520 | Cancel only | Stage list; 4px progress bar above the footer, `accent` fill; elapsed ms in the footer left |
| Could not reach host | 520 | Retry | sub "failed at tcp_connect" |
| Sign-in rejected | 520 | Change password | footer left `tried alice@temper:3389`; sub "failed at Credssp · STATUS_LOGON_FAILURE" |
| Port refused | 520 | Try port 3389 | sub "failed at tcp_connect · ECONNREFUSED" |
| First connection (TOFU) | 560 | Pin and connect | plus `Connect once`; fingerprint card; footnote naming the `known_hosts` path |
| Certificate CHANGED | 560 | Close | danger variant; **no accept-anyway path** — pinned vs offered fingerprints shown, user must forget the pin in Settings |
| No saved password | 560 | Enter password | shows the exact `security add-generic-password -s mdrdp -a '…' -w` command with a Copy affordance |
| Password prompt | 480 | Connect | focused password field; `warn` mono line naming the store failure |
| Edit connection | 560 | Save | 50px title row; password row reads `stored in keychain` with a `Replace` button and the account key beneath; footer note "Renaming the account moves the keychain entry."; `Remove connection` in `danger` at the body's bottom right |
| Remove connection | 440 | Remove (destructive) | unchecked "Also delete the keychain password" |
| Session ended | 460 | Done | 30px-gap stats row: duration, drift, pixels cached; footer left `Save metrics JSON` |
| Session lost | 480 | Reconnect | `warn` bar; explains the session is probably still alive on the host |
| Quit with sessions running | 480 | Quit anyway (destructive) | lists each running session as dot + `name · pid · uptime` |
| About mdrdp | 440 | Close | centred 72px icon, version `0.1.25 · macOS arm64`, rows Licence `MIT OR Apache-2.0`, Protocol `IronRDP 0.17`, Vendored `ironrdp-connector`, and the one-flag note |

Stage rows in the Connecting dialog: 15px circular marker (done = `accent.tint` fill with
1px `accent.fill` border and an `accent` check; current = solid `accent`; pending = 1px
`line.subtle`), stage name in mono 13px (`text.secondary` done, `text.primary` current,
`text.dim` pending) with any qualifier appended in `text.dim` (`HYBRID_EX`) or `accent`
(`pinned`), and elapsed ms right.

### 8. Transients

**Toasts** — 400px wide, `bg.raised`, 1px `line.strong`, **2px left border** in `warn` or
`danger`, radius 5, toast shadow, `padding:14px 16px`. Title 13px/500 `text.primary`, body
12px/19px `text.secondary`. Bottom-right of the session window, 6 s, stacked, never over
the cursor. Copy: "Clipboard image too large" / "Audio device lost" / "Host refused the new
resolution".

**Overlay** — keep `window.rs::draw_overlay`'s structure and its existing dim
(`(px >> 1) & 0x7f7f7f`), `padding:16px 18px`, lines in Plex Mono 12px/20px. A transient
adds one `warn` line (`audio    device lost 4s ago  playback stopped`) and removes it when
the condition clears. Zero-valued lines stay hidden, as they already do.

## Interactions and behaviour

- **Front page** chooses itself: no entries in `favourites.toml` → wizard; otherwise the
  list. Cancelling the wizard on first run leaves an empty-state list carrying the CLI hint.
- **Wizard** is `Tab`-ordered top to bottom, `Enter` advances (and connects on step 3),
  `Esc` cancels. Validation is per step, on advance, with the message under the offending
  field; the existing `FormError` variants in `ui/form.rs` carry the wording to reuse.
- **Connect** runs as a modal in the launcher. On success the launcher stays open (per the
  Defaults toggle) and the session window opens in its own process. On failure the modal is
  replaced by the matching failure dialog. Cancel aborts the connect thread.
- **Certificate** decisions block the connect thread: accept-once, pin-and-accept, reject.
  A CHANGED fingerprint has no accept path at all.
- **Cache window** polls at 1 s. The metric segmented control re-colours in place without
  changing selection; clicking a cell selects it; selection survives a refresh.
- **Diagnostics** windows are per-session and non-modal; the session window keeps running
  while they are open. Closing a session closes its diagnostics windows.
- **Destructive actions** (Remove, Quit anyway, Forget pin) never proceed without a second
  press, and each states what durable file it touches.

## State

Launcher process: `favourites: Vec<Favourite>`, `selected: Option<usize>`,
`running: Vec<SessionHandle{pid, name, started}>`, `wizard: Option<WizardState{step, fields,
error}>`, `modal: Option<Modal>`, `settings: Settings`.

Session process: `stats: SessionStats` (existing), `slot_stats: Vec<SlotStat>` (new),
`toasts: VecDeque<Toast{kind, title, body, expires}>`, `overlay_visible: bool`,
`diagnostics: { cache: Option<Window>, latency: Option<Window>, channels: Option<Window> }`,
and per-window view state (`cache.metric`, `cache.selected_slot`).

## New instrumentation

`stats.rs` keeps aggregate `CacheStats` only. Add per-slot records, updated where
`gfx.rs` already tracks `cache_dims` — `surface_to_cache` (fill/refill), `cache_to_surface`
(hit) and `on_evict_cache_entry`:

```rust
pub struct SlotStat {
    pub slot: u16,
    pub width: u16,
    pub height: u16,
    pub codec: &'static str,   // as reported for the update that filled it
    pub state: SlotState,      // Empty | Live | Evicted
    pub hits: u32,
    pub bytes_served: u64,     // hits × stored, accumulated
    pub bytes_stored: u64,
    pub last_hit: Option<Instant>,
}
```

The grid needs nothing else. `metrics.rs` should carry the same per-slot vector into the
redacted report (slot indices and sizes are not sensitive); keep the existing rule that no
host, account, credential, path, clipboard or pixel data appears.

## Settings store

New `settings.toml` beside `favourites.toml`, same atomic replace, same config-dir
resolution already implemented in `trust.rs`:

```toml
[defaults]
username = "alice"
port = 3389
window = "fullscreen"        # or "explicit"
width = 1920
height = 1080
keep_launcher_open = true
reconnect_last = false

[graphics]
clear_codec = true
rfx_progressive = true
allow_uncompressed = false
dynamic_resolution = true

[audio]
playback = true
device = "default"

[clipboard]
direction = "both"           # both | to_remote | from_remote | off
max_image_bytes = 1048576
timeout_secs = 5

[diagnostics]
overlay_on_connect = false
metrics_dir = "~/mdrdp/runs"
stage_log = "stages"         # off | stages | verbose
```

`[defaults] username` already exists in `favourites.toml`; on first run, migrate it here
and stop reading it from the favourites file.

## Screen map

| Screen | Existing source to read / change |
| --- | --- |
| Launcher shell, list, menus | `src/launcher.rs`, `src/ui/list.rs` (replaced), `src/favourites.rs` |
| Wizard | `src/ui/form.rs` (replaced — keep `FormError` wording and validation), `src/creds.rs` |
| Settings | new store; `src/favourites.rs` (atomic write), `src/trust.rs` (config dir) |
| Connecting dialog, failures | `src/connect.rs`, `src/stagelog.rs` |
| Certificate dialogs | `src/trust.rs` (`known_hosts`, fingerprint, CHANGED refusal) |
| Credential dialogs | `src/creds.rs` |
| Bitmap cache window | `src/stats.rs` (`CacheStats`), `src/gfx.rs` (`cache_dims`, cache PDU handlers) |
| Latency window | `src/stats.rs` (`Latency`, `Percentiles`, `BASELINE_SAMPLES`) |
| Channels window | `src/connect.rs` (channel list), `src/stagelog.rs`, `src/process_metrics.rs` |
| Toasts, overlay | `src/window.rs` (`draw_overlay`), `src/clipboard.rs`, `src/audio.rs` |
| Session end / lost / quit | `src/session.rs`, `src/window.rs`, `src/window_policy.rs` |
| About | `Cargo.toml`, `vendor/README.md` |

Favourites need two schema additions: a last-used timestamp per entry, and edit/remove
operations (the README currently lists both as missing).

## Assets

- `assets/icon/mdrdp.svg` — app icon master, full-bleed, 64-unit viewBox.
- `assets/icon/mdrdp-macos.svg` — same mark with the 8.6% inset macOS expects.
- `assets/icon/windows/icon-{16,24,32,48,64,128,256}.png` — pack into `.ico`.
- `assets/icon/macos/icon-{16,32,64,128,256,512,1024}.png` — pack with
  `iconutil -c icns`.

Stroke weight in the icon steps up at small sizes (2.5 units at ≥64px, 3 at 32, 4 at 16)
so the 16px rendering stays legible; the PNGs already have this baked in.

## Files

- `mdrdp Launcher.dc.html` — wizard (1a), bitmap cache (1c), settings (1e), connections
  list with menus (1f). The cache view is interactive: metric switching and slot selection
  both work.
- `mdrdp Dialogs.dc.html` — all twelve dialogs (4a–4e), latency (4f), channels (4g),
  toasts and overlay (4h).
- `mdrdp Icon.dc.html` — the icon at real sizes on dark and light.
- `support.js` — runtime the three HTML files load. Keep it beside them.

Open any of the HTML files in a browser. They pan and zoom as a canvas.
