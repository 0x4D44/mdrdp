//! "Channels and codecs" — handoff screen 6: what was negotiated, what is carrying
//! traffic, and how long the connect took.
//!
//! The screen is a pure function of one plain-data snapshot. Nothing here reaches into
//! `GfxStats`, `AudioStats`, the clipboard counters or the stage log: the host assembles
//! [`ChannelsSnapshot`] from those and hands it over already formatted where a number
//! needs a unit only the host knows (a channel's counter line, say). That keeps the
//! window drawable in a test, and keeps the arithmetic it *does* own — which stage was
//! slowest, how the codec bars divide, whether the error summary is still green —
//! testable without a window.
//!
//! Stage names are never rewritten. They arrive as `stagelog.rs` recorded them and are
//! drawn verbatim, because the window's closing note promises a report and this screen
//! agree.
//!
//! Layout, from the handoff: header 54 · body split into a flexible left pane
//! (`padding:22px 20px 22px 24px`, gap 20) and a 300px `bg.panel` sidebar
//! (`padding:20px`, gap 14). The 30px menu strip above and the 900×700 window around it
//! belong to the host.

use egui::{Align2, Color32, CornerRadius, FontId, Painter, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::diag::thousands;
use crate::ui::theme;

// --- the snapshot ---------------------------------------------------------------------

/// Whether a channel is carrying traffic, joined but quiet, or was never asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelState {
    /// Joined and in use — `accent` dot.
    Joined,
    /// Joined, but nothing has crossed it — `warn` dot. Worth seeing: a joined channel
    /// with no traffic is the shape most redirection problems take.
    JoinedIdle,
    /// Never requested — `line.subtle` dot, and the whole row drops a shade.
    NotRequested,
}

/// One row of the "static and dynamic channels" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRow {
    /// The protocol channel name, drawn in an 88px mono column. A fixed set, hence
    /// `&'static str` — a typo here would be a lie about the wire.
    pub name: &'static str,
    /// What the channel is doing, in prose: `joined · carries EGFX and audio`.
    pub description: String,
    pub state: ChannelState,
    /// The right-hand counter (`14 transfers · 0 timeouts`, `idle`). `None` draws no
    /// counter at all, which is what a not-requested channel gets.
    pub counter: Option<String>,
}

/// One labelled bar in the "surface codecs" section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecBar {
    /// The codec as it should read to a person — `ClearCodec`, `RFX Progressive`.
    pub label: String,
    pub updates: u64,
    pub painted_bytes: u64,
}

/// One connect stage, named exactly as the stage log recorded it.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    pub stage: String,
    pub elapsed_ms: f64,
}

/// The sidebar's "process" block.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ProcessRows {
    /// `None` before the first sampling interval closes — a zero would read as measured.
    pub cpu_average_percent: Option<f64>,
    pub peak_resident_bytes: Option<u64>,
    pub frames: u64,
    pub bytes_in: u64,
}

/// Everything the window draws, as plain data.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChannelsSnapshot {
    pub decode_errors: u64,
    pub undecoded_regions: u64,
    pub unhandled_pdus: u64,
    pub channels: Vec<ChannelRow>,
    pub codecs: Vec<CodecBar>,
    /// Drawn in the order given; the host supplies them in the order they happened.
    pub timeline: Vec<TimelineEntry>,
    pub total_to_first_frame_ms: Option<f64>,
    pub process: ProcessRows,
    /// The header's identity chunk, `(name, detail)` — for example
    /// `("Temper", "alice@temper:3389 · pid 4821")`. `None` draws the title alone.
    pub session: Option<(String, String)>,
}

/// What the window wants its host to do after the frame is drawn.
///
/// Screen 6 carries no control of its own — the handoff pins a note where the latency
/// window puts its button — so `ui` returns [`ChannelsAction::None`] today. The enum
/// exists because the host drives all three diagnostics windows through one shape, and
/// a metrics-write from the menu bar lands here without changing the signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelsAction {
    #[default]
    None,
    WriteMetrics,
}

// --- geometry (handoff values) --------------------------------------------------------

/// Header band, matching the other two diagnostics windows.
const HEADER_H: f32 = 54.0;
/// Header side padding.
const PAD_X: f32 = 24.0;
/// The right sidebar's fixed width.
const SIDEBAR_W: f32 = 300.0;
/// Left pane padding: top, right, bottom, left.
const LEFT_PAD_T: f32 = 22.0;
const LEFT_PAD_R: f32 = 20.0;
const LEFT_PAD_L: f32 = 24.0;
/// Sidebar padding, all four sides.
const SIDE_PAD: f32 = 20.0;
/// Gap between the two left-pane sections.
const SECTION_GAP: f32 = 20.0;
/// Gap from a section label to its content.
const LABEL_GAP: f32 = 11.0;
/// Letter spacing on the small uppercase section labels, as a fraction of the size.
const LABEL_TRACKING: f32 = 0.14;

/// A channel row's vertical padding, above and below.
const ROW_PAD_Y: f32 = 9.0;
/// The row's content band — one line of mono 13.
const ROW_CONTENT_H: f32 = 18.0;
const ROW_H: f32 = ROW_PAD_Y * 2.0 + ROW_CONTENT_H;
/// Status dot diameter.
const DOT: f32 = 6.0;
/// Gap between the row's columns.
const ROW_GAP: f32 = 14.0;
/// The channel-name column.
const NAME_COL_W: f32 = 88.0;

/// A codec block: its label line, the gap, and the bar itself.
const CODEC_LABEL_H: f32 = 18.0;
const CODEC_LABEL_GAP: f32 = 5.0;
const CODEC_BAR_H: f32 = 6.0;
const CODEC_BLOCK_H: f32 = CODEC_LABEL_H + CODEC_LABEL_GAP + CODEC_BAR_H;
/// Gap between codec blocks.
const CODEC_GAP: f32 = 12.0;

/// Gap between sidebar rows, and between its groups.
const SIDE_ROW_GAP: f32 = 9.0;
const SIDE_GROUP_GAP: f32 = 14.0;
/// One sidebar text row.
const SIDE_ROW_H: f32 = 17.0;

/// Bar colours, taken in order, exactly as the handoff paints its three.
///
/// Position rather than codec name: the host orders the bars (heaviest first in the
/// mock), and a colour keyed off a name would need a table of every codec that could
/// ever appear.
const CODEC_COLOURS: [Color32; 3] = [theme::ACCENT, theme::CYAN, theme::WARN];

// --- the arithmetic, kept out of the drawing ------------------------------------------

/// Which stage took longest, by index — the one drawn in `warn`.
///
/// Ties go to the first, so a timeline of equal stages highlights the earliest rather
/// than flickering between them as later readings tie and untie. `None` for an empty
/// timeline: nothing is the slowest of nothing.
pub fn slowest_stage(timeline: &[TimelineEntry]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, entry) in timeline.iter().enumerate() {
        match best {
            // Strictly greater, so an equal later stage never displaces the first.
            Some(b) if entry.elapsed_ms <= timeline[b].elapsed_ms => {}
            _ => best = Some(i),
        }
    }
    best
}

/// The header's right-hand caption.
///
/// Singular and plural both appear because the line is read at a glance, and "1 decode
/// errors" makes a reader stop and re-read the one line that must not need re-reading.
pub fn error_summary(decode_errors: u64, undecoded_regions: u64, unhandled_pdus: u64) -> String {
    format!(
        "{decode_errors} {} · {undecoded_regions} {} · {unhandled_pdus} {}",
        plural(decode_errors, "decode error", "decode errors"),
        plural(undecoded_regions, "undecoded region", "undecoded regions"),
        plural(unhandled_pdus, "unhandled PDU", "unhandled PDUs"),
    )
}

/// The colour that caption is drawn in: `accent` while every count is zero, `danger` the
/// moment any one of them is not.
///
/// One non-zero is enough. These three counts are the ones that say the picture on
/// screen is not the picture the server sent, and a per-count colour would let two of
/// them stay green while the third is red.
pub fn error_summary_colour(
    decode_errors: u64,
    undecoded_regions: u64,
    unhandled_pdus: u64,
) -> Color32 {
    if decode_errors == 0 && undecoded_regions == 0 && unhandled_pdus == 0 {
        theme::ACCENT
    } else {
        theme::DANGER
    }
}

fn plural(n: u64, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

/// How far each codec bar fills, as its share of all painted bytes.
///
/// Share of the total rather than a fraction of the largest, because the three bars
/// together are meant to read as the split of the session's pixels — the same total the
/// sidebar's "Bytes in" reports. A largest-bar scale would draw the top codec full-width
/// whatever the mix, which says nothing. The handoff's 186 / 98 / 12 MiB give 63% / 33% /
/// 4%, which is the mock's 61 / 34 / 5 bar to within its eyeballing.
///
/// Bytes, not update counts: an update is not a unit of anything comparable across
/// codecs. All-zero gives all-zero, not a divide.
pub fn codec_fractions(codecs: &[CodecBar]) -> Vec<f32> {
    let total: u64 = codecs.iter().map(|c| c.painted_bytes).sum();
    if total == 0 {
        return vec![0.0; codecs.len()];
    }
    codecs
        .iter()
        .map(|c| c.painted_bytes as f32 / total as f32)
        .collect()
}

/// A byte count in the largest whole unit: `214.0 MiB`, `1.5 KiB`, `512 B`.
///
/// Spaced, unlike `stats.rs`'s overlay formatter — the overlay is a fixed-pitch block
/// where every character costs a column, and this window is not.
pub fn bytes_human(n: u64) -> String {
    const K: f64 = 1024.0;
    let n_f = n as f64;
    if n < 1024 {
        format!("{n} B")
    } else if n_f < K * K {
        format!("{:.1} KiB", n_f / K)
    } else if n_f < K * K * K {
        format!("{:.1} MiB", n_f / (K * K))
    } else {
        format!("{:.1} GiB", n_f / (K * K * K))
    }
}

/// A stage duration as the handoff writes it: whole milliseconds, `3 ms`.
///
/// Sub-millisecond precision on a connect stage is noise — the whole timeline spans
/// hundreds of milliseconds — and a decimal point in every row makes the column harder
/// to scan.
pub fn stage_ms(elapsed_ms: f64) -> String {
    format!("{:.0} ms", elapsed_ms.max(0.0))
}

/// A codec bar's caption: `1,284 updates · 186.0 MiB`.
pub fn codec_caption(bar: &CodecBar) -> String {
    format!(
        "{} {} · {}",
        thousands(bar.updates),
        plural(bar.updates, "update", "updates"),
        bytes_human(bar.painted_bytes)
    )
}

// --- drawing --------------------------------------------------------------------------

/// Draw the whole window body from one snapshot.
pub fn ui(ui: &mut Ui, snapshot: &ChannelsSnapshot) -> ChannelsAction {
    header(ui, snapshot);

    let (body, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), ui.available_height()),
        Sense::hover(),
    );
    let split_x = (body.max.x - SIDEBAR_W).max(body.min.x);
    let left = Rect::from_min_max(body.min, pos2(split_x, body.max.y));
    let sidebar = Rect::from_min_max(pos2(split_x, body.min.y), body.max);

    let painter = ui.painter();
    left_pane(painter, left, snapshot);
    right_sidebar(painter, sidebar, snapshot);

    ChannelsAction::None
}

fn header(ui: &mut Ui, snapshot: &ChannelsSnapshot) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), Sense::hover());
    let painter = ui.painter();
    painter.hline(
        rect.x_range(),
        rect.max.y - 0.5,
        Stroke::new(1.0, theme::LINE_HAIR),
    );

    let cy = rect.center().y;
    let title = painter.text(
        pos2(rect.min.x + PAD_X, cy),
        Align2::LEFT_CENTER,
        "Channels and codecs",
        theme::sans_semibold(15.0),
        theme::TEXT_PRIMARY,
    );
    if let Some((name, detail)) = &snapshot.session {
        let divider_x = title.max.x + 14.0;
        painter.vline(
            divider_x,
            egui::Rangef::new(cy - 9.0, cy + 9.0),
            Stroke::new(1.0, theme::LINE_SUBTLE),
        );
        let dot_x = divider_x + 14.0;
        painter.circle_filled(pos2(dot_x + 3.0, cy), 3.0, theme::ACCENT);
        let name_rect = painter.text(
            pos2(dot_x + 6.0 + 9.0, cy),
            Align2::LEFT_CENTER,
            name,
            theme::mono(12.0),
            theme::TEXT_PRIMARY,
        );
        painter.text(
            pos2(name_rect.max.x + 9.0, cy),
            Align2::LEFT_CENTER,
            detail,
            theme::mono(12.0),
            theme::TEXT_DIM,
        );
    }
    painter.text(
        pos2(rect.max.x - PAD_X, cy),
        Align2::RIGHT_CENTER,
        error_summary(
            snapshot.decode_errors,
            snapshot.undecoded_regions,
            snapshot.unhandled_pdus,
        ),
        theme::mono(11.0),
        error_summary_colour(
            snapshot.decode_errors,
            snapshot.undecoded_regions,
            snapshot.unhandled_pdus,
        ),
    );
}

fn left_pane(painter: &Painter, pane: Rect, snapshot: &ChannelsSnapshot) {
    let content = Rect::from_min_max(
        pos2(pane.min.x + LEFT_PAD_L, pane.min.y + LEFT_PAD_T),
        pos2(pane.max.x - LEFT_PAD_R, pane.max.y),
    );
    if content.width() <= 0.0 {
        return;
    }

    let mut y = content.min.y;
    y = section_label(painter, content.min.x, y, "STATIC AND DYNAMIC CHANNELS") + LABEL_GAP;

    let last = snapshot.channels.len().saturating_sub(1);
    for (i, row) in snapshot.channels.iter().enumerate() {
        let rect = Rect::from_min_max(pos2(content.min.x, y), pos2(content.max.x, y + ROW_H));
        channel_row(painter, rect, row);
        // The handoff separates rows but does not close the list with a rule.
        if i != last {
            // #1E2126 is the `bg.raised` token, reused here as the faintest rule in the
            // system — one shade below `line.hair`.
            painter.hline(
                rect.x_range(),
                rect.max.y - 0.5,
                Stroke::new(1.0, theme::BG_RAISED),
            );
        }
        y = rect.max.y;
    }

    y += SECTION_GAP;
    y = section_label(painter, content.min.x, y, "SURFACE CODECS") + LABEL_GAP;

    let fractions = codec_fractions(&snapshot.codecs);
    for (i, bar) in snapshot.codecs.iter().enumerate() {
        let rect = Rect::from_min_max(
            pos2(content.min.x, y),
            pos2(content.max.x, y + CODEC_BLOCK_H),
        );
        codec_block(painter, rect, bar, fractions[i], CODEC_COLOURS[i % 3]);
        y = rect.max.y + CODEC_GAP;
    }
}

fn channel_row(painter: &Painter, rect: Rect, row: &ChannelRow) {
    let cy = rect.min.y + ROW_PAD_Y + ROW_CONTENT_H / 2.0;
    let (dot, name_colour, description_colour) = match row.state {
        ChannelState::Joined => (theme::ACCENT, theme::TEXT_PRIMARY, theme::TEXT_SECONDARY),
        ChannelState::JoinedIdle => (theme::WARN, theme::TEXT_PRIMARY, theme::TEXT_SECONDARY),
        // A channel that was never requested reads a shade back: it is context, not news.
        ChannelState::NotRequested => (theme::LINE_SUBTLE, theme::TEXT_MUTED, theme::TEXT_DIM),
    };
    painter.circle_filled(pos2(rect.min.x + DOT / 2.0, cy), DOT / 2.0, dot);

    let name_x = rect.min.x + DOT + ROW_GAP;
    painter.text(
        pos2(name_x, cy),
        Align2::LEFT_CENTER,
        row.name,
        theme::mono(13.0),
        name_colour,
    );
    painter.text(
        pos2(name_x + NAME_COL_W + ROW_GAP, cy),
        Align2::LEFT_CENTER,
        &row.description,
        theme::sans(12.0),
        description_colour,
    );
    if let Some(counter) = &row.counter {
        painter.text(
            pos2(rect.max.x, cy),
            Align2::RIGHT_CENTER,
            counter,
            theme::mono(12.0),
            theme::TEXT_DIM,
        );
    }
}

fn codec_block(painter: &Painter, rect: Rect, bar: &CodecBar, fraction: f32, colour: Color32) {
    let cy = rect.min.y + CODEC_LABEL_H / 2.0;
    painter.text(
        pos2(rect.min.x, cy),
        Align2::LEFT_CENTER,
        &bar.label,
        theme::sans(13.0),
        theme::TEXT_PRIMARY,
    );
    painter.text(
        pos2(rect.max.x, cy),
        Align2::RIGHT_CENTER,
        codec_caption(bar),
        theme::mono(12.0),
        theme::TEXT_SECONDARY,
    );

    let track = Rect::from_min_max(
        pos2(rect.min.x, rect.max.y - CODEC_BAR_H),
        pos2(rect.max.x, rect.max.y),
    );
    let radius = CornerRadius::same((CODEC_BAR_H / 2.0) as u8);
    painter.rect_filled(track, radius, theme::BG_RAISED);
    let filled = track.width() * fraction.clamp(0.0, 1.0);
    if filled > 0.0 {
        painter.rect_filled(
            Rect::from_min_size(track.min, vec2(filled, CODEC_BAR_H)),
            radius,
            colour,
        );
    }
}

fn right_sidebar(painter: &Painter, pane: Rect, snapshot: &ChannelsSnapshot) {
    painter.rect_filled(pane, CornerRadius::ZERO, theme::BG_PANEL);
    painter.vline(
        pane.min.x + 0.5,
        pane.y_range(),
        Stroke::new(1.0, theme::LINE_HAIR),
    );

    let content = Rect::from_min_max(
        pos2(pane.min.x + SIDE_PAD, pane.min.y + SIDE_PAD),
        pos2(pane.max.x - SIDE_PAD, pane.max.y - SIDE_PAD),
    );
    if content.width() <= 0.0 {
        return;
    }

    let mut y = content.min.y;
    y = section_label(painter, content.min.x, y, "CONNECT TIMELINE") + SIDE_GROUP_GAP;

    let slowest = slowest_stage(&snapshot.timeline);
    for (i, entry) in snapshot.timeline.iter().enumerate() {
        let is_slowest = slowest == Some(i);
        let (stage_colour, ms_colour) = if is_slowest {
            (theme::TEXT_PRIMARY, theme::WARN)
        } else {
            (theme::TEXT_SECONDARY, theme::TEXT_DIM)
        };
        let cy = y + SIDE_ROW_H / 2.0;
        painter.text(
            pos2(content.min.x, cy),
            Align2::LEFT_CENTER,
            &entry.stage,
            theme::mono(12.0),
            stage_colour,
        );
        painter.text(
            pos2(content.max.x, cy),
            Align2::RIGHT_CENTER,
            stage_ms(entry.elapsed_ms),
            theme::mono(11.0),
            ms_colour,
        );
        y += SIDE_ROW_H + SIDE_ROW_GAP;
    }
    // The loop leaves a trailing row gap; drop it, but only if a row actually ran, or an
    // empty timeline pulls the divider up into its own label.
    if !snapshot.timeline.is_empty() {
        y -= SIDE_ROW_GAP;
    }
    y += SIDE_GROUP_GAP;

    y = divider(painter, content, y) + SIDE_GROUP_GAP;
    let total = match snapshot.total_to_first_frame_ms {
        Some(ms) => stage_ms(ms),
        // No first frame yet: a zero would claim one arrived instantly.
        None => "—".to_owned(),
    };
    let total_colour = match snapshot.total_to_first_frame_ms {
        Some(_) => theme::ACCENT,
        None => theme::TEXT_MUTED,
    };
    // The total's own label sits a shade back from the process rows below it, as the
    // handoff has it: the number is the news, not the words.
    y = key_value(
        painter,
        content,
        y,
        ("Total to first frame", theme::TEXT_MUTED),
        &total,
        total_colour,
    ) + SIDE_GROUP_GAP;

    y = divider(painter, content, y) + SIDE_GROUP_GAP;
    y = section_label(painter, content.min.x, y, "PROCESS") + SIDE_GROUP_GAP;

    let p = &snapshot.process;
    let rows = [
        (
            "CPU average",
            p.cpu_average_percent
                .map_or_else(|| "—".to_owned(), |v| format!("{v:.2}%")),
        ),
        (
            "Peak resident",
            p.peak_resident_bytes
                .map_or_else(|| "—".to_owned(), bytes_human),
        ),
        ("Frames", thousands(p.frames)),
        ("Bytes in", bytes_human(p.bytes_in)),
    ];
    for (label, value) in rows {
        y = key_value(
            painter,
            content,
            y,
            (label, theme::TEXT_SECONDARY),
            &value,
            theme::TEXT_PRIMARY,
        ) + SIDE_ROW_GAP;
    }

    // Pinned to the bottom, as `margin-top:auto` does in the handoff.
    let note = painter.layout(
        "Stage names are the ones the stage log records, so a report and this window \
         agree."
            .to_owned(),
        theme::sans(12.0),
        theme::TEXT_DIM,
        content.width(),
    );
    let note_y = (content.max.y - note.size().y).max(y);
    painter.galley(pos2(content.min.x, note_y), note, theme::TEXT_DIM);
}

/// A "label right-aligned value" sidebar row. Returns its bottom edge.
fn key_value(
    painter: &Painter,
    content: Rect,
    y: f32,
    label: (&str, Color32),
    value: &str,
    value_colour: Color32,
) -> f32 {
    let cy = y + SIDE_ROW_H / 2.0;
    painter.text(
        pos2(content.min.x, cy),
        Align2::LEFT_CENTER,
        label.0,
        theme::sans(12.0),
        label.1,
    );
    painter.text(
        pos2(content.max.x, cy),
        Align2::RIGHT_CENTER,
        value,
        theme::mono(12.0),
        value_colour,
    );
    y + SIDE_ROW_H
}

fn divider(painter: &Painter, content: Rect, y: f32) -> f32 {
    painter.hline(
        content.x_range(),
        y + 0.5,
        Stroke::new(1.0, theme::LINE_HAIR),
    );
    y + 1.0
}

/// A small tracked uppercase section label. Returns its bottom edge.
fn section_label(painter: &Painter, x: f32, y: f32, text: &str) -> f32 {
    let galley = painter.layout_job(spaced(
        text,
        theme::mono(11.0),
        theme::TEXT_MUTED,
        11.0 * LABEL_TRACKING,
    ));
    let h = galley.size().y;
    painter.galley(pos2(x, y), galley, theme::TEXT_MUTED);
    y + h
}

/// A single run of text with letter spacing, which [`Painter::text`] cannot express.
///
/// The twin of `latency.rs`'s private helper; both belong in `diag/mod.rs` once that
/// file is not owned by another change in flight.
fn spaced(text: &str, font: FontId, colour: Color32, spacing: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: font,
            color: colour,
            extra_letter_spacing: spacing,
            ..Default::default()
        },
    );
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handoff's own timeline, stage names verbatim.
    fn mock_timeline() -> Vec<TimelineEntry> {
        [
            ("tcp_connect", 3.0),
            ("x224_negotiation", 6.0),
            ("tls_handshake", 41.0),
            ("Credssp", 118.0),
            ("LicensingExchange", 12.0),
            ("CapabilitiesExchange", 9.0),
            ("post_tls_sequence", 4.0),
            ("egfx_observation", 62.0),
        ]
        .into_iter()
        .map(|(stage, elapsed_ms)| TimelineEntry {
            stage: stage.to_owned(),
            elapsed_ms,
        })
        .collect()
    }

    /// The handoff's three codec bars.
    fn mock_codecs() -> Vec<CodecBar> {
        const MIB: u64 = 1024 * 1024;
        vec![
            CodecBar {
                label: "ClearCodec".to_owned(),
                updates: 1_284,
                painted_bytes: 186 * MIB,
            },
            CodecBar {
                label: "RFX Progressive".to_owned(),
                updates: 702,
                painted_bytes: 98 * MIB,
            },
            CodecBar {
                label: "Uncompressed".to_owned(),
                updates: 41,
                painted_bytes: 12 * MIB,
            },
        ]
    }

    /// The handoff's five channel rows, every field distinct.
    fn mock_channels() -> Vec<ChannelRow> {
        vec![
            ChannelRow {
                name: "DRDYNVC",
                description: "joined · carries EGFX and audio".to_owned(),
                state: ChannelState::Joined,
                counter: Some("2 DVCs open".to_owned()),
            },
            ChannelRow {
                name: "CLIPRDR",
                description: "joined · text and CF_DIB, both ways".to_owned(),
                state: ChannelState::Joined,
                counter: Some("14 transfers · 0 timeouts".to_owned()),
            },
            ChannelRow {
                name: "RDPSND",
                description: "joined · 44.1 kHz stereo → 48 kHz mono".to_owned(),
                state: ChannelState::Joined,
                counter: Some("222 packets".to_owned()),
            },
            ChannelRow {
                name: "RDPDR",
                description: "joined · no redirection offered".to_owned(),
                state: ChannelState::JoinedIdle,
                counter: Some("idle".to_owned()),
            },
            ChannelRow {
                name: "AINPUT",
                description: "not requested · also ECHO, RAIL".to_owned(),
                state: ChannelState::NotRequested,
                counter: None,
            },
        ]
    }

    fn mock_snapshot() -> ChannelsSnapshot {
        ChannelsSnapshot {
            decode_errors: 0,
            undecoded_regions: 0,
            unhandled_pdus: 0,
            channels: mock_channels(),
            codecs: mock_codecs(),
            timeline: mock_timeline(),
            total_to_first_frame_ms: Some(255.0),
            process: ProcessRows {
                cpu_average_percent: Some(0.57),
                peak_resident_bytes: Some(169 * 1024 * 1024),
                frames: 2_027,
                bytes_in: 296 * 1024 * 1024,
            },
            session: Some((
                "Temper".to_owned(),
                "alice@temper:3389 · pid 4821".to_owned(),
            )),
        }
    }

    #[test]
    fn the_slowest_stage_is_the_longest_one_and_a_tie_goes_to_the_first() {
        // The handoff's timeline: Credssp at 118 ms, index 3 — not the last, not the
        // largest index, and not the one at the largest position in the list.
        assert_eq!(slowest_stage(&mock_timeline()), Some(3));

        let tie = vec![
            TimelineEntry {
                stage: "tcp_connect".to_owned(),
                elapsed_ms: 7.0,
            },
            TimelineEntry {
                stage: "tls_handshake".to_owned(),
                elapsed_ms: 44.0,
            },
            TimelineEntry {
                stage: "Credssp".to_owned(),
                elapsed_ms: 44.0,
            },
        ];
        assert_eq!(slowest_stage(&tie), Some(1), "the earlier of two equals");

        assert_eq!(
            slowest_stage(&[]),
            None,
            "nothing is the slowest of nothing"
        );
        assert_eq!(
            slowest_stage(&[TimelineEntry {
                stage: "tcp_connect".to_owned(),
                elapsed_ms: 3.0,
            }]),
            Some(0)
        );
    }

    #[test]
    fn the_error_summary_stays_green_only_while_every_count_is_zero() {
        assert_eq!(
            error_summary(0, 0, 0),
            "0 decode errors · 0 undecoded regions · 0 unhandled PDUs"
        );
        assert_eq!(error_summary_colour(0, 0, 0), theme::ACCENT);

        // Distinct values in every position: a swapped argument cannot pass.
        assert_eq!(
            error_summary(4, 17, 2),
            "4 decode errors · 17 undecoded regions · 2 unhandled PDUs"
        );
        assert_eq!(
            error_summary(1, 1, 1),
            "1 decode error · 1 undecoded region · 1 unhandled PDU"
        );

        // Each count alone is enough to turn the line red.
        assert_eq!(error_summary_colour(1, 0, 0), theme::DANGER);
        assert_eq!(error_summary_colour(0, 1, 0), theme::DANGER);
        assert_eq!(error_summary_colour(0, 0, 1), theme::DANGER);
    }

    #[test]
    fn codec_bars_split_the_painted_bytes_between_them() {
        let f = codec_fractions(&mock_codecs());
        // 186 / 98 / 12 of 296 MiB — hand-computed, not read back off the code.
        assert!((f[0] - 0.628_378).abs() < 1e-4, "got {}", f[0]);
        assert!((f[1] - 0.331_081).abs() < 1e-4, "got {}", f[1]);
        assert!((f[2] - 0.040_540).abs() < 1e-4, "got {}", f[2]);
        assert!(
            (f.iter().sum::<f32>() - 1.0).abs() < 1e-5,
            "bars fill the bar"
        );

        // Update counts do not move a bar: only bytes do.
        let mut skewed = mock_codecs();
        skewed[2].updates = 99_999;
        assert_eq!(codec_fractions(&skewed), f);

        // A session that has painted nothing draws no fill, rather than dividing by zero.
        let idle: Vec<CodecBar> = mock_codecs()
            .into_iter()
            .map(|c| CodecBar {
                painted_bytes: 0,
                ..c
            })
            .collect();
        assert_eq!(codec_fractions(&idle), vec![0.0, 0.0, 0.0]);
        assert!(codec_fractions(&[]).is_empty());
    }

    #[test]
    fn byte_counts_read_in_the_largest_whole_unit() {
        assert_eq!(bytes_human(0), "0 B");
        assert_eq!(bytes_human(512), "512 B");
        assert_eq!(bytes_human(1023), "1023 B");
        assert_eq!(bytes_human(1024), "1.0 KiB");
        assert_eq!(bytes_human(1_536), "1.5 KiB");
        assert_eq!(bytes_human(224_395_264), "214.0 MiB");
        assert_eq!(
            bytes_human(224_460_800),
            "214.1 MiB",
            "one decimal, rounded"
        );
        assert_eq!(bytes_human(296 * 1024 * 1024), "296.0 MiB");
        assert_eq!(bytes_human(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn stage_durations_and_codec_captions_read_as_the_handoff_writes_them() {
        assert_eq!(stage_ms(3.0), "3 ms");
        assert_eq!(stage_ms(118.4), "118 ms");
        assert_eq!(stage_ms(255.0), "255 ms");
        assert_eq!(stage_ms(0.4), "0 ms");

        let codecs = mock_codecs();
        assert_eq!(codec_caption(&codecs[0]), "1,284 updates · 186.0 MiB");
        assert_eq!(codec_caption(&codecs[2]), "41 updates · 12.0 MiB");
        assert_eq!(
            codec_caption(&CodecBar {
                label: "ClearCodec".to_owned(),
                updates: 1,
                painted_bytes: 4_096,
            }),
            "1 update · 4.0 KiB"
        );
    }

    /// Run one real egui frame of the window.
    ///
    /// `ui()` cannot be asserted on pixel by pixel, but it can be *run*: this lays out
    /// and tessellates the whole screen, which is what catches a bad rect, a negative
    /// size, or a divide by zero in the bar arithmetic.
    fn frame(snapshot: &ChannelsSnapshot) -> ChannelsAction {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let input = egui::RawInput {
            // The session's 900×700 diagnostics window, less its 30px menu strip.
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 670.0))),
            ..Default::default()
        };
        // Seeded with the variant the screen must *not* return, so a closure that never
        // runs cannot pass this test.
        let mut action = ChannelsAction::WriteMetrics;
        let output = ctx.run_ui(input, |u| {
            action = super::ui(u, snapshot);
        });
        output.drop_without_applying_deltas();
        action
    }

    #[test]
    fn a_real_frame_draws_a_populated_screen_and_an_empty_one() {
        assert_eq!(frame(&mock_snapshot()), ChannelsAction::None);

        // Non-zero counts take the danger path through the header.
        let mut broken = mock_snapshot();
        broken.decode_errors = 3;
        broken.unhandled_pdus = 11;
        assert_eq!(frame(&broken), ChannelsAction::None);

        // A session that has only just connected: no channels, no codecs, no timeline,
        // no first frame and no process sample. Every `None` and every empty vector at
        // once, which is the state the window is in for its first second.
        assert_eq!(frame(&ChannelsSnapshot::default()), ChannelsAction::None);

        // And one with a timeline but nothing else, so the sidebar runs past its
        // dividers with an empty left pane beside it.
        let partial = ChannelsSnapshot {
            timeline: mock_timeline(),
            ..Default::default()
        };
        assert_eq!(frame(&partial), ChannelsAction::None);
    }
}
