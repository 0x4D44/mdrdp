//! The macOS Dock tile: the app icon with this session's name written above it.
//!
//! A session process is an unbundled executable, so macOS gives it the generic
//! "exec" tile unless something calls `-[NSApplication setApplicationIconImage:]`.
//! The launcher gets that call for free — eframe makes it from the icon we hand
//! `ViewportBuilder::with_icon` — but the session window is raw winit, so until
//! this module every `mdrdp <host>` sat in the Dock as an anonymous "exec" block.
//! With several sessions open that is exactly the moment the tile has to say which
//! host it is, so the tile is composed per session rather than shared.
//!
//! Composition is pure Rust: the embedded icon is cropped, scaled and blitted, the
//! label is rasterised from the embedded IBM Plex Sans face, and the result is a
//! plain RGBA buffer. That keeps the layout and the pixels testable on any target;
//! the only platform code is [`macos::apply`], a dozen lines that wrap the buffer in
//! an `NSImage` and hand it to `NSApplication`.

/// The composed tile: a square RGBA8 image, straight (not premultiplied) alpha.
pub struct Tile {
    pub size: u32,
    pub rgba: Vec<u8>,
}

/// The tile is composed at 512px. The Dock renders at most 256px (128pt at 2x) on
/// this hardware, so 512 leaves headroom for a larger Dock without paying 1024's
/// four megabytes of pixels for a tile nobody sees at that size.
const CANVAS: u32 = 512;

/// The macOS icon-grid margin the source art already uses — measured from the
/// asset's own alpha bounding box, not assumed, so a redrawn icon stays right.
const MARGIN: f32 = 44.0;

/// Height of the label band across the top. The icon takes what is left.
const BAND: f32 = 132.0;

/// Horizontal padding either side of the label band.
const SIDE_PAD: f32 = 32.0;

/// Vertical padding above and below the text inside the band.
const BAND_PAD: f32 = 14.0;

/// Below this the label is unreadable at Dock size (56/512 of the tile is ~14px on
/// a 128pt tile), so a dotted name drops its domain rather than shrink further.
const MIN_POINTS: f32 = 56.0;

/// The icon's own background, reused for the label pill so it reads as part of the
/// art rather than a sticker on top of it.
const PILL: [u8; 3] = [20, 22, 26];
const PILL_ALPHA: f32 = 0.94;
const TEXT: [u8; 3] = [255, 255, 255];

const FONT: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf");
const ICON: &[u8] = include_bytes!("../assets/icon/macos/icon-512.png");

/// Put `label` on this process's Dock tile. A no-op off macOS, and on any failure —
/// a tile is cosmetic and must never take a session down.
pub fn set_label(label: &str) {
    #[cfg(target_os = "macos")]
    {
        if let Some(tile) = compose(label) {
            macos::apply(&tile);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = label;
    }
}

/// The label a tile actually shows.
///
/// A full DNS name is drawn if it fits legibly; otherwise the first element carries
/// the identity (`quench.lan.example` → `quench`) — that is the part that differs
/// between two sessions on one LAN. An address is never cut: `192` names nothing.
fn tile_label(label: &str) -> &str {
    let trimmed = label.trim();
    let Some((head, _)) = trimmed.split_once('.') else {
        return trimmed;
    };
    if head.is_empty() || head.bytes().all(|b| b.is_ascii_digit()) {
        return trimmed;
    }
    head
}

/// The point size that fits `width_at_1pt` into `max_width`, capped by `max_points`.
fn fit_points(width_at_1pt: f32, max_width: f32, max_points: f32) -> f32 {
    if width_at_1pt <= 0.0 {
        return max_points;
    }
    (max_width / width_at_1pt).min(max_points)
}

/// Compose the tile for `label`. `None` only if an embedded asset fails to decode.
pub fn compose(label: &str) -> Option<Tile> {
    use ab_glyph::{Font, FontRef, PxScale, ScaleFont as _, point};

    let side = CANVAS as usize;
    let mut rgba = vec![0u8; side * side * 4];

    // --- the icon, cropped to its own art and scaled into the band's remainder ---
    let (icon, iw, ih) = decode_icon(ICON)?;
    let (x0, y0, x1, y1) = alpha_bounds(&icon, iw, ih)?;
    let cropped = crop(&icon, iw, x0, y0, x1, y1);
    let art = (CANVAS as f32 - BAND - MARGIN).max(1.0) as usize;
    let scaled = resample_box(&cropped, x1 - x0, y1 - y0, art, art);
    blit(
        &mut rgba,
        side,
        &scaled,
        art,
        art,
        (side - art) / 2,
        BAND as usize,
    );

    // --- the label, fitted to the band ---
    let font = FontRef::try_from_slice(FONT).ok()?;
    let text = tile_label(label);
    // Cap by height first: the band is the hard constraint for a short name.
    let unit_height = font.height_unscaled() / font.units_per_em()?;
    let by_height = (BAND - 2.0 * BAND_PAD) / unit_height;
    let max_width = CANVAS as f32 - 2.0 * SIDE_PAD;
    let mut points = fit_points(advance_width(&font, text, 1.0), max_width, by_height);
    let text = if points < MIN_POINTS {
        // Too long to read whole: try the identity-carrying head instead. If that is
        // the whole string already, the fitted size stands — a tiny label still beats
        // a clipped one.
        let short = tile_label(text);
        points = fit_points(advance_width(&font, short, 1.0), max_width, by_height);
        short
    } else {
        text
    };

    let scale = PxScale::from(points);
    let scaled_font = font.as_scaled(scale);
    let width = advance_width(&font, text, points);
    let ascent = scaled_font.ascent();
    let height = scaled_font.height();
    let baseline = (BAND - height) / 2.0 + ascent;
    let left = (CANVAS as f32 - width) / 2.0;

    // The pill sits behind the text so the label reads on a light Dock too.
    let pill_h = height + 2.0 * 6.0;
    let pill_w = width + 2.0 * 20.0;
    fill_round_rect(
        &mut rgba,
        side,
        (CANVAS as f32 - pill_w) / 2.0,
        (BAND - pill_h) / 2.0,
        pill_w,
        pill_h,
        pill_h / 2.0,
        PILL,
        PILL_ALPHA,
    );

    let mut pen = left;
    let mut previous: Option<ab_glyph::GlyphId> = None;
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        if let Some(prev) = previous {
            pen += scaled_font.kern(prev, id);
        }
        let glyph = id.with_scale_and_position(scale, point(pen, baseline));
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let x = bounds.min.x as i32 + gx as i32;
                let y = bounds.min.y as i32 + gy as i32;
                blend(&mut rgba, side, x, y, TEXT, coverage);
            });
        }
        pen += scaled_font.h_advance(id);
        previous = Some(id);
    }

    Some(Tile { size: CANVAS, rgba })
}

/// Advance width of `text` at `points`, kerning included.
fn advance_width(font: &ab_glyph::FontRef<'_>, text: &str, points: f32) -> f32 {
    use ab_glyph::{Font, PxScale, ScaleFont as _};
    let scaled = font.as_scaled(PxScale::from(points));
    let mut width = 0.0;
    let mut previous = None;
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        if let Some(prev) = previous {
            width += scaled.kern(prev, id);
        }
        width += scaled.h_advance(id);
        previous = Some(id);
    }
    width
}

/// Decode an embedded RGBA8 PNG.
fn decode_icon(bytes: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    let icon = crate::ui::help::decode_icon(bytes)?;
    Some((icon.rgba, icon.width as usize, icon.height as usize))
}

/// The bounding box of the non-transparent pixels, as `(x0, y0, x1, y1)` half-open.
fn alpha_bounds(rgba: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if rgba[(y * w + x) * 4 + 3] != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

fn crop(rgba: &[u8], w: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
    for y in y0..y1 {
        let row = (y * w + x0) * 4;
        out.extend_from_slice(&rgba[row..row + (x1 - x0) * 4]);
    }
    out
}

/// Box-filter resample. Averaging happens on premultiplied alpha, or transparent
/// pixels drag their (arbitrary) colour into the edge and the icon grows a halo.
fn resample_box(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        let sy0 = dy * sh / dh;
        let sy1 = (((dy + 1) * sh).div_ceil(dh)).max(sy0 + 1).min(sh);
        for dx in 0..dw {
            let sx0 = dx * sw / dw;
            let sx1 = (((dx + 1) * sw).div_ceil(dw)).max(sx0 + 1).min(sw);
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            let mut n = 0.0f32;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let i = (sy * sw + sx) * 4;
                    let sa = src[i + 3] as f32 / 255.0;
                    r += src[i] as f32 * sa;
                    g += src[i + 1] as f32 * sa;
                    b += src[i + 2] as f32 * sa;
                    a += sa;
                    n += 1.0;
                }
            }
            let o = (dy * dw + dx) * 4;
            if a > 0.0 {
                out[o] = (r / a).round().clamp(0.0, 255.0) as u8;
                out[o + 1] = (g / a).round().clamp(0.0, 255.0) as u8;
                out[o + 2] = (b / a).round().clamp(0.0, 255.0) as u8;
            }
            out[o + 3] = (a / n * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn blit(dst: &mut [u8], dw: usize, src: &[u8], sw: usize, sh: usize, ox: usize, oy: usize) {
    for y in 0..sh {
        for x in 0..sw {
            let s = (y * sw + x) * 4;
            let d = ((y + oy) * dw + (x + ox)) * 4;
            dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
}

/// Source-over one pixel with straight alpha. Out of bounds is a no-op.
fn blend(dst: &mut [u8], w: usize, x: i32, y: i32, colour: [u8; 3], coverage: f32) {
    let a = coverage.clamp(0.0, 1.0);
    if a <= 0.0 || x < 0 || y < 0 || x as usize >= w {
        return;
    }
    let i = (y as usize * w + x as usize) * 4;
    if i + 4 > dst.len() {
        return;
    }
    let da = dst[i + 3] as f32 / 255.0;
    let out_a = a + da * (1.0 - a);
    if out_a <= 0.0 {
        return;
    }
    for c in 0..3 {
        let s = colour[c] as f32 / 255.0;
        let d = dst[i + c] as f32 / 255.0;
        let v = (s * a + d * da * (1.0 - a)) / out_a;
        dst[i + c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dst[i + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// Anti-aliased rounded rectangle, via the signed distance to the shape.
#[allow(clippy::too_many_arguments)]
fn fill_round_rect(
    dst: &mut [u8],
    w: usize,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    colour: [u8; 3],
    alpha: f32,
) {
    let (hw, hh) = (width / 2.0, height / 2.0);
    let (cx, cy) = (x + hw, y + hh);
    let r = radius.min(hw).min(hh);
    let (ix, iy) = (hw - r, hh - r);
    let y0 = (y.floor() as i32 - 1).max(0);
    let y1 = ((y + height).ceil() as i32 + 1).max(0);
    let x0 = (x.floor() as i32 - 1).max(0);
    let x1 = ((x + width).ceil() as i32 + 1).max(0);
    for py in y0..y1 {
        for px in x0..x1 {
            let dx = ((px as f32 + 0.5) - cx).abs() - ix;
            let dy = ((py as f32 + 0.5) - cy).abs() - iy;
            let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
            let inside = dx.max(dy).min(0.0);
            let distance = outside + inside - r;
            let coverage = (0.5 - distance).clamp(0.0, 1.0);
            blend(dst, w, px, py, colour, coverage * alpha);
        }
    }
}

/// Hand the composed tile to AppKit.
#[cfg(target_os = "macos")]
mod macos {
    use super::Tile;
    use objc2::AnyThread as _;
    use objc2_app_kit::{NSApplication, NSBitmapImageRep, NSDeviceRGBColorSpace, NSImage};
    use objc2_foundation::{MainThreadMarker, NSSize};

    pub fn apply(tile: &Tile) {
        let Some(mtm) = MainThreadMarker::new() else {
            return; // AppKit is main-thread only; a tile is not worth a panic.
        };
        let side = tile.size as isize;

        // `NSBitmapImageRep` does NOT copy the planes it is given, and the Dock
        // re-renders the tile whenever it likes (resolution change, Dock resize), so
        // the buffer has to outlive this call. One deliberate 1 MB leak, once per
        // process. The rep is built from raw RGBA rather than the PNG on purpose:
        // some macOS builds load an arbitrary libpng for `NSImage`-from-PNG and
        // SIGBUS (egui#7155).
        let pixels: &'static mut [u8] = Box::leak(tile.rgba.clone().into_boxed_slice());
        let mut planes = [pixels.as_mut_ptr()];

        // SAFETY: the planes pointer is a live `size * size * 4` RGBA8 buffer that
        // outlives the process, and every scalar below describes exactly that layout.
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                planes.as_mut_ptr(),
                side,
                side,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                side * 4,
                32,
            )
        };
        let Some(rep) = rep else { return };

        let image = NSImage::initWithSize(
            NSImage::alloc(),
            NSSize::new(tile.size as f64, tile.size as f64),
        );
        image.addRepresentation(&rep);

        // SAFETY: main thread (proven by `mtm`), and a valid `NSImage`.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pixel at (x, y) as RGBA.
    fn px(tile: &Tile, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * tile.size + x) * 4) as usize;
        [
            tile.rgba[i],
            tile.rgba[i + 1],
            tile.rgba[i + 2],
            tile.rgba[i + 3],
        ]
    }

    #[test]
    fn a_dotted_name_keeps_only_the_element_that_identifies_it() {
        assert_eq!(tile_label("quench.lan.example"), "quench");
        assert_eq!(tile_label("temper.lan.example"), "temper");
        assert_eq!(tile_label("quench"), "quench");
        assert_eq!(tile_label("  quench  "), "quench");
    }

    #[test]
    fn an_address_is_never_cut_down_to_its_first_octet() {
        assert_eq!(tile_label("192.0.2.240"), "192.0.2.240");
        assert_eq!(tile_label(".home.arpa"), ".home.arpa");
    }

    #[test]
    fn fitting_scales_to_the_available_width_and_stops_at_the_cap() {
        // 10 units wide at 1pt into 200px of band is 20pt, under the 100pt cap.
        assert!((fit_points(10.0, 200.0, 100.0) - 20.0).abs() < 1e-3);
        // A short label would overflow the band's height, so the cap wins.
        assert!((fit_points(1.0, 200.0, 100.0) - 100.0).abs() < 1e-3);
    }

    #[test]
    fn the_label_fits_inside_the_tile() {
        use ab_glyph::FontRef;
        let font = FontRef::try_from_slice(FONT).expect("the embedded face parses");
        let max_width = CANVAS as f32 - 2.0 * SIDE_PAD;
        for label in [
            "quench",
            "temper",
            "192.0.2.240",
            "a-very-long-favourite-name-indeed",
        ] {
            let text = tile_label(label);
            let points = fit_points(advance_width(&font, text, 1.0), max_width, 200.0);
            let width = advance_width(&font, text, points);
            assert!(
                width <= max_width + 0.5,
                "{label}: {width} wide at {points}pt, band holds {max_width}"
            );
        }
    }

    #[test]
    fn a_long_dotted_name_is_shortened_rather_than_rendered_unreadably_small() {
        use ab_glyph::FontRef;
        let font = FontRef::try_from_slice(FONT).expect("the embedded face parses");
        let max_width = CANVAS as f32 - 2.0 * SIDE_PAD;
        // The whole dotted name would be drawn below the legibility floor…
        let full = "session.long.example.internal";
        let whole = fit_points(advance_width(&font, full, 1.0), max_width, 200.0);
        assert!(
            whole < MIN_POINTS,
            "{full} fits at {whole}pt — pick a longer one"
        );
        // …so composition falls back to the head, which clears it.
        let head = fit_points(
            advance_width(&font, tile_label(full), 1.0),
            max_width,
            200.0,
        );
        assert!(head > MIN_POINTS, "{full} head still only {head}pt");
    }

    #[test]
    fn the_tile_carries_the_icon_below_the_label_band() {
        let tile = compose("quench").expect("the embedded assets decode");
        assert_eq!(tile.size, CANVAS);
        assert_eq!(tile.rgba.len(), (CANVAS * CANVAS * 4) as usize);
        // The middle of the art is opaque icon, the very top corner is bare tile.
        assert_eq!(px(&tile, CANVAS / 2, 330)[3], 255);
        assert_eq!(px(&tile, 2, 2)[3], 0);
    }

    #[test]
    fn the_label_is_actually_drawn_and_differs_per_host() {
        let quench = compose("quench").expect("composes");
        let temper = compose("temper").expect("composes");
        let band: usize = (BAND as usize) * (CANVAS as usize) * 4;
        assert_ne!(
            quench.rgba[..band],
            temper.rgba[..band],
            "two hosts produced an identical label band"
        );
        // The art below the band is the same icon in both.
        assert_eq!(quench.rgba[band..], temper.rgba[band..]);
        // Some pixel in the band is white text, not just the pill.
        assert!(
            quench.rgba[..band]
                .chunks_exact(4)
                .any(|p| p[0] > 240 && p[1] > 240 && p[2] > 240 && p[3] > 200),
            "no text pixels in the label band"
        );
    }
}
