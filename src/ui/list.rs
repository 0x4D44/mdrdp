//! A minimal list widget for the favourites launcher: rows of text, a selection and a
//! hover index, scrolling, hit-testing, and double-click detection.
//!
//! Like [`crate::ui::font`], everything here takes plain buffer/coordinate parameters
//! and has no `winit`/`softbuffer` dependency, so it is fully testable without a window.

use super::font::{self, CHAR_H};

/// Left padding, in pixels, between a row's left edge and its text.
const TEXT_PAD_X: i32 = 6;

/// One row in the list: a label and an optional secondary/subtitle line.
#[derive(Debug, Clone)]
pub struct Row {
    pub label: String,
    pub subtitle: Option<String>,
}

impl Row {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            subtitle: None,
        }
    }

    pub fn with_subtitle(label: impl Into<String>, subtitle: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            subtitle: Some(subtitle.into()),
        }
    }
}

/// Background/text colours for the three row states. All colours are `0x00RRGGBB`.
#[derive(Debug, Clone, Copy)]
pub struct ListColours {
    pub bg_normal: u32,
    pub bg_hover: u32,
    pub bg_selected: u32,
    pub text: u32,
    pub subtitle_text: u32,
}

impl Default for ListColours {
    fn default() -> Self {
        Self {
            bg_normal: 0x0020_2020,
            bg_hover: 0x0030_3030,
            bg_selected: 0x0035_5a8f,
            text: 0x00e0_e0e0,
            subtitle_text: 0x0090_9090,
        }
    }
}

/// A keyboard navigation action for [`ListView::handle_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Up,
    Down,
    Enter,
}

/// A scrollable, selectable list of [`Row`]s rendered into a plain pixel buffer.
///
/// `(x, y)` is the top-left origin, `width` and `visible_height` define the on-screen
/// viewport; the row list itself may be taller than the viewport, in which case
/// [`ListView::scroll_by`] (or keyboard navigation) scrolls it.
pub struct ListView {
    pub rows: Vec<Row>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub visible_height: i32,
    pub row_height: i32,
    pub selected: Option<usize>,
    pub hover: Option<usize>,
    /// Vertical scroll offset in pixels, always in `[0, max_scroll]`.
    pub scroll: i32,
    pub colours: ListColours,
}

impl ListView {
    pub fn new(x: i32, y: i32, width: i32, visible_height: i32, row_height: i32) -> Self {
        Self {
            rows: Vec::new(),
            x,
            y,
            width,
            visible_height,
            row_height,
            selected: None,
            hover: None,
            scroll: 0,
            colours: ListColours::default(),
        }
    }

    /// Replace the row list, clamping selection/hover/scroll so they stay valid for
    /// the new content.
    pub fn set_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        if let Some(i) = self.selected
            && i >= self.rows.len()
        {
            self.selected = if self.rows.is_empty() {
                None
            } else {
                Some(self.rows.len() - 1)
            };
        }
        if let Some(i) = self.hover
            && i >= self.rows.len()
        {
            self.hover = None;
        }
        self.scroll = self.scroll.clamp(0, self.max_scroll());
    }

    /// Total content height in pixels (all rows, regardless of viewport).
    fn content_height(&self) -> i32 {
        (self.rows.len() as i32).saturating_mul(self.row_height.max(0))
    }

    /// The largest valid scroll offset — content height minus viewport, floored at 0
    /// so a short list (or a zero/negative viewport) never allows scrolling.
    fn max_scroll(&self) -> i32 {
        (self.content_height() - self.visible_height).max(0)
    }

    /// Scroll by `delta` pixels (positive = down), clamped so the list can never
    /// scroll past either end.
    pub fn scroll_by(&mut self, delta: i32) {
        let max_scroll = self.max_scroll();
        self.scroll = self.scroll.saturating_add(delta).clamp(0, max_scroll);
    }

    /// Hit-test a point against the list. Returns `None` if the point is outside the
    /// list's horizontal or vertical extent, above the first row, or past the last row
    /// (including landing in the dead space below the last row inside the viewport).
    pub fn row_at(&self, px: i32, py: i32) -> Option<usize> {
        if self.rows.is_empty()
            || self.row_height <= 0
            || self.width <= 0
            || self.visible_height <= 0
        {
            return None;
        }
        if px < self.x || px >= self.x + self.width {
            return None;
        }
        if py < self.y || py >= self.y + self.visible_height {
            return None;
        }
        let rel_y = (py - self.y) + self.scroll;
        if rel_y < 0 {
            return None;
        }
        let idx = (rel_y / self.row_height) as usize;
        if idx >= self.rows.len() {
            return None;
        }
        Some(idx)
    }

    /// Update `hover` from a pointer position (convenience wrapper over `row_at`).
    pub fn update_hover(&mut self, px: i32, py: i32) {
        self.hover = self.row_at(px, py);
    }

    /// Move the selection up by one row (clamped at the first row), and scroll to
    /// keep it visible.
    pub fn select_up(&mut self) {
        if self.rows.is_empty() {
            self.selected = None;
            return;
        }
        self.selected = Some(match self.selected {
            Some(i) if i > 0 => i - 1,
            _ => 0,
        });
        self.ensure_selected_visible();
    }

    /// Move the selection down by one row (clamped at the last row), and scroll to
    /// keep it visible.
    pub fn select_down(&mut self) {
        if self.rows.is_empty() {
            self.selected = None;
            return;
        }
        let last = self.rows.len() - 1;
        self.selected = Some(match self.selected {
            None => 0,
            Some(i) if i < last => i + 1,
            Some(_) => last,
        });
        self.ensure_selected_visible();
    }

    fn ensure_selected_visible(&mut self) {
        let Some(i) = self.selected else { return };
        let row_top = i as i32 * self.row_height;
        let row_bottom = row_top + self.row_height;
        if row_top < self.scroll {
            self.scroll = row_top;
        } else if row_bottom > self.scroll + self.visible_height {
            self.scroll = row_bottom - self.visible_height;
        }
        self.scroll = self.scroll.clamp(0, self.max_scroll());
    }

    /// Handle a keyboard navigation action. `Up`/`Down` move the selection and return
    /// `None`; `Enter` activates the current selection and returns it.
    pub fn handle_key(&mut self, action: KeyAction) -> Option<usize> {
        match action {
            KeyAction::Up => {
                self.select_up();
                None
            }
            KeyAction::Down => {
                self.select_down();
                None
            }
            KeyAction::Enter => self.selected,
        }
    }

    /// Draw every visible row's background and text into `buf`. Fully clipped, like
    /// [`font::draw_text`] — an out-of-range origin or a too-small buffer just means
    /// less (or nothing) gets drawn, never a panic.
    ///
    /// Every row is additionally clipped to the list's own viewport band
    /// `[self.y, self.y + self.visible_height)`, not just the buffer bounds: a row
    /// that is only partially inside the viewport (the top row after a scroll, or the
    /// bottom row when the content doesn't divide evenly) must not paint outside the
    /// list, over whatever else is drawn on screen. This keeps `draw` consistent with
    /// [`ListView::row_at`], which already clamps hit-testing to the same band.
    pub fn draw(&self, buf: &mut [u32], buf_w: usize, buf_h: usize) {
        if self.row_height <= 0 || self.rows.is_empty() {
            return;
        }
        let viewport_top = self.y;
        let viewport_bottom = self.y + self.visible_height;
        for (i, row) in self.rows.iter().enumerate() {
            let row_top = self.y + (i as i32 * self.row_height) - self.scroll;
            let row_bottom = row_top + self.row_height;
            if row_bottom <= viewport_top || row_top >= viewport_bottom {
                continue; // fully outside the viewport
            }
            let bg = if self.selected == Some(i) {
                self.colours.bg_selected
            } else if self.hover == Some(i) {
                self.colours.bg_hover
            } else {
                self.colours.bg_normal
            };
            // Intersect the row's rectangle with the viewport band before filling —
            // a partial row must only paint the slice of itself actually inside the
            // list, not its full `row_height` (that's the bug this guards against).
            let fill_top = row_top.max(viewport_top);
            let fill_bottom = row_bottom.min(viewport_bottom);
            if fill_bottom > fill_top {
                fill_rect(
                    buf,
                    buf_w,
                    buf_h,
                    (self.x, fill_top, self.width, fill_bottom - fill_top),
                    bg,
                );
            }

            let text_y = row_top + (self.row_height - CHAR_H) / 2;
            let text_x = self.x + TEXT_PAD_X;
            if let Some(sub) = &row.subtitle {
                let label_y = text_y - CHAR_H / 2;
                let sub_y = text_y + CHAR_H / 2;
                draw_text_clipped(
                    buf,
                    buf_w,
                    buf_h,
                    viewport_top,
                    viewport_bottom,
                    text_x,
                    label_y,
                    &row.label,
                    self.colours.text,
                );
                draw_text_clipped(
                    buf,
                    buf_w,
                    buf_h,
                    viewport_top,
                    viewport_bottom,
                    text_x,
                    sub_y,
                    sub,
                    self.colours.subtitle_text,
                );
            } else {
                draw_text_clipped(
                    buf,
                    buf_w,
                    buf_h,
                    viewport_top,
                    viewport_bottom,
                    text_x,
                    text_y,
                    &row.label,
                    self.colours.text,
                );
            }
        }
    }
}

/// Draw `s` clipped not only to the buffer bounds (which [`font::draw_text`] already
/// does on its own) but to an arbitrary vertical band `[clip_top, clip_bottom)` —
/// used so a row that is only partially inside the list's viewport has its glyph
/// rows truncated at the viewport edge instead of spilling past it.
///
/// `font::draw_text` only knows how to clip against `y < 0` and `y >= buf_h`, i.e. a
/// band that starts at zero. To clip against an arbitrary `clip_top` too, this hands
/// it a sub-slice of `buf` starting at row `clip_top`: real row `clip_top` becomes
/// virtual row `0`, so `font::draw_text`'s own `y < 0` check now clips everything
/// above `clip_top`, and a shrunk virtual `buf_h` makes its `y >= buf_h` check clip
/// everything at or past `clip_bottom`.
#[allow(clippy::too_many_arguments)]
fn draw_text_clipped(
    buf: &mut [u32],
    buf_w: usize,
    buf_h: usize,
    clip_top: i32,
    clip_bottom: i32,
    x: i32,
    y: i32,
    s: &str,
    colour: u32,
) {
    let clip_top = clip_top.max(0);
    let clip_bottom = clip_bottom.min(buf_h as i32);
    if buf_w == 0 || clip_bottom <= clip_top {
        return;
    }
    let Some(offset) = (clip_top as usize).checked_mul(buf_w) else {
        return;
    };
    if offset >= buf.len() {
        return;
    }
    let virtual_h = (clip_bottom - clip_top) as usize;
    font::draw_text(
        &mut buf[offset..],
        buf_w,
        virtual_h,
        x,
        y - clip_top,
        s,
        colour,
    );
}

/// Fill an axis-aligned rectangle, clipped against the buffer bounds. Shared by
/// [`ListView::draw`]; kept private since it has no reason to be part of the public
/// widget surface yet. `rect` is `(x, y, w, h)`.
fn fill_rect(buf: &mut [u32], buf_w: usize, buf_h: usize, rect: (i32, i32, i32, i32), colour: u32) {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 {
        return;
    }
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(buf_w as i32);
    let y1 = (y + h).min(buf_h as i32);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for py in y0..y1 {
        let Some(row_start) = (py as usize).checked_mul(buf_w) else {
            continue;
        };
        for px in x0..x1 {
            let Some(idx) = row_start.checked_add(px as usize) else {
                continue;
            };
            if let Some(cell) = buf.get_mut(idx) {
                *cell = colour;
            }
        }
    }
}

/// Double-click detection, kept separate from `ListView` so it is deterministically
/// testable: the caller supplies the clock (`now_ms`), this never calls `Instant::now()`
/// itself.
///
/// Three rapid clicks on the same row: click 1 arms the state, click 2 (within the
/// threshold) reports a double-click and *clears* the armed state, click 3 (within the
/// threshold of click 2) is therefore treated as a fresh single click and reports
/// `false`. A triple-click is a double followed by a single, not two doubles — the
/// clear-on-fire is what stops a burst of clicks from reporting double every time.
pub struct DoubleClick {
    threshold_ms: u64,
    last: Option<(usize, u64)>,
}

impl DoubleClick {
    pub fn new(threshold_ms: u64) -> Self {
        Self {
            threshold_ms,
            last: None,
        }
    }

    /// Register a click on `row` at time `now_ms`. Returns `true` if this click and
    /// the immediately preceding one form a double-click: same row, within
    /// `threshold_ms` of each other.
    pub fn click(&mut self, now_ms: u64, row: usize) -> bool {
        let is_double = matches!(
            self.last,
            Some((r, t)) if r == row && now_ms.saturating_sub(t) <= self.threshold_ms
        );
        self.last = if is_double { None } else { Some((row, now_ms)) };
        is_double
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<Row> {
        (0..n).map(|i| Row::new(format!("row {i}"))).collect()
    }

    // --- row_at boundaries -------------------------------------------------

    #[test]
    fn row_at_exactly_on_row_border() {
        let mut lv = ListView::new(0, 0, 100, 100, 20);
        lv.set_rows(rows(5));
        // Row 0 spans y in [0, 20); row 1 spans [20, 40).
        assert_eq!(lv.row_at(10, 19), Some(0));
        assert_eq!(lv.row_at(10, 20), Some(1));
    }

    #[test]
    fn row_at_above_the_list() {
        let mut lv = ListView::new(0, 10, 100, 100, 20);
        lv.set_rows(rows(5));
        assert_eq!(lv.row_at(10, 9), None);
        assert_eq!(lv.row_at(10, 0), None);
        assert_eq!(lv.row_at(10, -50), None);
    }

    #[test]
    fn row_at_below_the_last_row() {
        let mut lv = ListView::new(0, 0, 100, 60, 20);
        lv.set_rows(rows(2)); // content is 40px tall, viewport is 60px — dead space below
        assert_eq!(lv.row_at(10, 39), Some(1));
        assert_eq!(lv.row_at(10, 40), None); // past last row, still inside viewport
        assert_eq!(lv.row_at(10, 59), None); // inside viewport, past content
        assert_eq!(lv.row_at(10, 60), None); // outside viewport entirely
    }

    #[test]
    fn row_at_outside_horizontal_extent() {
        let mut lv = ListView::new(10, 0, 50, 100, 20);
        lv.set_rows(rows(5));
        assert_eq!(lv.row_at(9, 5), None); // just left of the list
        assert_eq!(lv.row_at(60, 5), None); // just right of the list
        assert_eq!(lv.row_at(10, 5), Some(0)); // left edge, inclusive
        assert_eq!(lv.row_at(59, 5), Some(0)); // right edge, exclusive at 60
    }

    #[test]
    fn row_at_empty_list_is_always_none() {
        let lv = ListView::new(0, 0, 100, 100, 20);
        assert_eq!(lv.row_at(10, 10), None);
    }

    // --- scroll clamping -----------------------------------------------------

    #[test]
    fn scroll_clamps_at_top() {
        let mut lv = ListView::new(0, 0, 100, 60, 20);
        lv.set_rows(rows(10));
        lv.scroll_by(-1000);
        assert_eq!(lv.scroll, 0);
    }

    #[test]
    fn scroll_clamps_at_bottom() {
        let mut lv = ListView::new(0, 0, 100, 60, 20);
        lv.set_rows(rows(10)); // content 200px, viewport 60px -> max_scroll 140
        lv.scroll_by(10_000);
        assert_eq!(lv.scroll, 140);
        // Further downward scroll must not move it past max.
        lv.scroll_by(50);
        assert_eq!(lv.scroll, 140);
    }

    #[test]
    fn scroll_is_a_no_op_when_content_shorter_than_viewport() {
        let mut lv = ListView::new(0, 0, 100, 400, 20);
        lv.set_rows(rows(3)); // content 60px, viewport 400px
        lv.scroll_by(1000);
        assert_eq!(lv.scroll, 0);
        lv.scroll_by(-1000);
        assert_eq!(lv.scroll, 0);
    }

    #[test]
    fn scroll_by_accumulates_and_clamps_incrementally() {
        let mut lv = ListView::new(0, 0, 100, 60, 20);
        lv.set_rows(rows(10)); // max_scroll = 140
        lv.scroll_by(50);
        assert_eq!(lv.scroll, 50);
        lv.scroll_by(50);
        assert_eq!(lv.scroll, 100);
        lv.scroll_by(50);
        assert_eq!(lv.scroll, 140); // clamped, would have been 150
    }

    // --- double click ----------------------------------------------------

    #[test]
    fn double_click_same_row_within_threshold_is_true() {
        let mut dc = DoubleClick::new(400);
        assert!(!dc.click(1000, 2));
        assert!(dc.click(1200, 2));
    }

    #[test]
    fn double_click_same_row_beyond_threshold_is_false() {
        let mut dc = DoubleClick::new(400);
        assert!(!dc.click(1000, 2));
        assert!(!dc.click(1500, 2)); // 500ms later, past the 400ms threshold
    }

    #[test]
    fn double_click_different_row_within_threshold_is_false() {
        let mut dc = DoubleClick::new(400);
        assert!(!dc.click(1000, 2));
        assert!(!dc.click(1100, 3)); // different row, well within time
    }

    #[test]
    fn double_click_exactly_at_threshold_boundary_is_true() {
        let mut dc = DoubleClick::new(400);
        assert!(!dc.click(1000, 2));
        assert!(dc.click(1400, 2)); // exactly 400ms later: <= threshold
    }

    #[test]
    fn double_click_one_ms_past_threshold_is_false() {
        let mut dc = DoubleClick::new(400);
        assert!(!dc.click(1000, 2));
        assert!(!dc.click(1401, 2)); // 401ms later: > threshold
    }

    #[test]
    fn triple_click_is_double_then_single() {
        let mut dc = DoubleClick::new(400);
        assert!(!dc.click(1000, 2)); // click 1: arm
        assert!(dc.click(1100, 2)); // click 2: double, then clears
        assert!(!dc.click(1200, 2)); // click 3: treated as a fresh single
        assert!(dc.click(1300, 2)); // click 4: doubles with click 3
    }

    // --- keyboard navigation ----------------------------------------------

    #[test]
    fn keyboard_navigation_moves_selection_and_clamps() {
        let mut lv = ListView::new(0, 0, 100, 60, 20);
        lv.set_rows(rows(5));
        assert_eq!(lv.handle_key(KeyAction::Down), None);
        assert_eq!(lv.selected, Some(0));
        lv.handle_key(KeyAction::Down);
        assert_eq!(lv.selected, Some(1));
        lv.handle_key(KeyAction::Up);
        assert_eq!(lv.selected, Some(0));
        lv.handle_key(KeyAction::Up); // already at top, stays
        assert_eq!(lv.selected, Some(0));
        assert_eq!(lv.handle_key(KeyAction::Enter), Some(0));
    }

    #[test]
    fn keyboard_navigation_scrolls_to_keep_selection_visible() {
        let mut lv = ListView::new(0, 0, 100, 40, 20); // 2 rows visible at once
        lv.set_rows(rows(10));
        // First Down from `None` lands on row 0, so 6 presses reach row 5.
        for _ in 0..6 {
            lv.handle_key(KeyAction::Down);
        }
        assert_eq!(lv.selected, Some(5));
        // Row 5 spans [100, 120); it must be within [scroll, scroll+40).
        assert!(lv.scroll <= 100 && lv.scroll + 40 >= 120);
    }

    // --- draw() must not panic on degenerate inputs -----------------------

    #[test]
    fn draw_does_not_panic_on_zero_sized_buffer() {
        let mut lv = ListView::new(0, 0, 100, 60, 20);
        lv.set_rows(rows(5));
        let mut buf: Vec<u32> = Vec::new();
        lv.draw(&mut buf, 0, 0);
    }

    #[test]
    fn draw_does_not_panic_with_negative_origin() {
        let mut lv = ListView::new(-30, -10, 100, 60, 20);
        lv.set_rows(rows(5));
        let mut buf = vec![0u32; 40 * 40];
        lv.draw(&mut buf, 40, 40);
    }

    #[test]
    fn set_rows_clamps_selection_and_hover_to_new_length() {
        let mut lv = ListView::new(0, 0, 100, 60, 20);
        lv.set_rows(rows(5));
        lv.selected = Some(4);
        lv.hover = Some(4);
        lv.set_rows(rows(2));
        assert_eq!(lv.selected, Some(1)); // clamped to last valid index
        assert_eq!(lv.hover, None); // hover just drops out of range
    }

    // --- draw() clips rows to the list's own viewport, not just the framebuffer ---

    /// Build the exact repro from the adversarial review: the launcher's real list
    /// geometry, 13 rows, then 13 `select_down`s (what ArrowDown does) — this lands
    /// on the last row and scrolls to `scroll == 20`, leaving row 0 spanning real
    /// y `[36, 80)`: 20px above the viewport top (`y == 56`) and 24px inside it.
    fn repro_geometry() -> ListView {
        let mut lv = ListView::new(16, 56, 528, 552, 44);
        lv.set_rows(rows(13));
        for _ in 0..13 {
            lv.select_down();
        }
        assert_eq!(
            lv.scroll, 20,
            "test assumes this exact scroll from the repro"
        );
        lv
    }

    const SENTINEL: u32 = 0x00ab_cdef;

    #[test]
    fn draw_clips_top_partial_row_to_the_viewport_not_the_framebuffer() {
        let lv = repro_geometry();
        let buf_w = 560usize;
        let buf_h = 624usize;
        let mut buf = vec![SENTINEL; buf_w * buf_h];
        lv.draw(&mut buf, buf_w, buf_h);

        // Nothing above the viewport top (the header band, y < 56) may have been
        // touched, anywhere across row 0's horizontal span — this is exactly the
        // band the launcher's "double-click to connect" hint lives in.
        for y in 0..lv.y {
            for x in [lv.x, lv.x + 10, lv.x + lv.width / 2, lv.x + lv.width - 1] {
                let idx = y as usize * buf_w + x as usize;
                assert_eq!(
                    buf[idx], SENTINEL,
                    "pixel ({x},{y}) above the viewport (y < {}) was painted",
                    lv.y
                );
            }
        }
        // The visible sliver of row 0 (y in [56, 80)) must still be painted with its
        // background colour — the fix must not degrade into "draw nothing".
        let visible_idx = (lv.y as usize) * buf_w + (lv.x as usize + 10);
        assert_eq!(buf[visible_idx], lv.colours.bg_normal);
    }

    #[test]
    fn draw_clips_bottom_partial_row_to_the_viewport_not_the_framebuffer() {
        // viewport is y in [0, 50), row_height 20: row 2 spans [40, 60), so only
        // [40, 50) of it should ever be painted.
        let mut lv = ListView::new(0, 0, 100, 50, 20);
        lv.set_rows(rows(5));
        let buf_w = 100usize;
        let buf_h = 80usize; // extra rows below the viewport to catch any spill
        let mut buf = vec![SENTINEL; buf_w * buf_h];
        lv.draw(&mut buf, buf_w, buf_h);

        let viewport_bottom = lv.y + lv.visible_height; // 50
        for y in viewport_bottom..buf_h as i32 {
            for x in [0i32, 25, 50, 99] {
                let idx = y as usize * buf_w + x as usize;
                assert_eq!(
                    buf[idx], SENTINEL,
                    "pixel ({x},{y}) below the viewport (y >= {viewport_bottom}) was painted"
                );
            }
        }
        // The visible sliver of row 2 (y in [40, 50)) must still be painted.
        let idx = 45usize * buf_w + 10usize;
        assert_eq!(buf[idx], lv.colours.bg_normal);
    }

    #[test]
    fn draw_and_row_at_agree_on_which_pixels_belong_to_the_list() {
        let lv = repro_geometry();
        let buf_w = 560usize;
        let buf_h = 624usize;
        let mut buf = vec![SENTINEL; buf_w * buf_h];
        lv.draw(&mut buf, buf_w, buf_h);

        // Sample a grid spanning well beyond the list's own extent in every
        // direction, including the partial-row bands above/below the viewport.
        let xs: Vec<i32> = (0..buf_w as i32).step_by(7).collect();
        let ys: Vec<i32> = (0..buf_h as i32).step_by(3).collect();
        for &py in &ys {
            for &px in &xs {
                let painted = buf[py as usize * buf_w + px as usize] != SENTINEL;
                let hit = lv.row_at(px, py).is_some();
                assert_eq!(
                    painted, hit,
                    "draw/row_at disagree at ({px},{py}): painted={painted}, row_at={hit}"
                );
            }
        }
    }

    #[test]
    fn draw_paints_exactly_the_row_that_row_at_reports() {
        let buf_w = 560usize;
        let buf_h = 624usize;
        // Probe: the visible sliver of the clipped top row, a fully interior row,
        // a point in row 0's own clipped-off sliver just above the viewport (the
        // exact spill band from the defect: y in [36, 56)), a point further above
        // the viewport, and a point below the viewport entirely.
        let points = [(300, 60), (300, 300), (300, 45), (300, 20), (300, 620)];
        for (px, py) in points {
            let lv = repro_geometry();
            let hit = lv.row_at(px, py);
            for idx in 0..lv.rows.len() {
                let mut lv = repro_geometry();
                lv.selected = Some(idx);
                lv.hover = None;
                let mut buf = vec![SENTINEL; buf_w * buf_h];
                lv.draw(&mut buf, buf_w, buf_h);
                let painted_selected =
                    buf[py as usize * buf_w + px as usize] == lv.colours.bg_selected;
                assert_eq!(
                    painted_selected,
                    hit == Some(idx),
                    "point ({px},{py}) row_at={hit:?}, selected idx={idx}, painted_selected={painted_selected}"
                );
            }
        }
    }

    #[test]
    fn draw_still_renders_every_row_when_the_list_fully_fits_the_viewport() {
        // Viewport (200px) is taller than the content (5 rows * 20px = 100px), so
        // every row is fully visible and none of them should be clipped.
        let mut lv = ListView::new(0, 0, 100, 200, 20);
        lv.set_rows(rows(5));
        let buf_w = 100usize;
        let buf_h = 200usize;
        let mut buf = vec![SENTINEL; buf_w * buf_h];
        lv.draw(&mut buf, buf_w, buf_h);
        for i in 0..5 {
            let y = i * 20 + 10; // mid-row, away from the glyph baseline
            let idx = y * buf_w + 90; // right side of the row, clear of the label
            assert_eq!(buf[idx], lv.colours.bg_normal, "row {i} was not painted");
        }
    }
}
