//! The `Global\mdrdp-idd` shared-section contract — the byte layout the IddCx
//! driver publishes and this server consumes.
//!
//! Increment 2 of the native transport removes Desktop Duplication from the
//! capture path: the driver copies each committed swapchain buffer into a pool of
//! three named shared D3D11 textures and describes what it did in one named
//! shared-memory section. This module is the portable half of that — the offsets,
//! the seqlock rules, the object names and the coverage invariant. It touches no
//! platform API, so it builds and tests on the Mac the server is written on, which
//! is the only place the offsets can be checked against the C++ driver's without a
//! Windows box in the loop.
//!
//! Layout, all little-endian, 16384 bytes, four 4096-byte pages:
//!
//! ```text
//! page 0 — header
//!    0  u32 layout_version        # 1; anything else is refused loudly
//!    4  u32 generation            # 0 = no pool yet; bumps on every rebuild
//!    8  u64 render_adapter_luid   # LowPart | HighPart << 32
//!   16  u32 width
//!   20  u32 height
//!   24  u32 dxgi_format
//!   28  u32 slot_count            # 3
//!   32  u64 name_suffix           # random, generation-qualifies the object names
//!   40  u32 header_sequence       # seqlock: odd = mid-write
//!   44  u32 reserved
//!
//! page 1+i — slot i, i in 0..3
//!    0  u32 sequence              # seqlock: odd = mid-write
//!    4  u32 reserved
//!    8  u64 frame_seq             # 1-based, contiguous, per generation
//!   16  u64 dirty_since_frame_seq
//!   24  i64 present_qpc           # QPC ticks, this machine's frequency
//!   32  u32 coverage_rect_count   # 0xFFFFFFFF absent, 0xFFFFFFFE overflowed
//!   36  u32 reserved
//!   40  RECT coverage_rects[64]   # i32 left, top, right, bottom
//! ```
//!
//! ## The seqlock
//!
//! The driver writes a record between an odd and an even sequence word. A reader
//! therefore samples the sequence, copies the body, samples again, and accepts the
//! copy only when both samples are equal and even. The "even" half of that rule is
//! pure and lives here ([`parse_header`], [`parse_slot`] refuse an odd word); the
//! re-sample half needs the live mapping and lives in `win::idd_source`.
//!
//! ## The coverage invariant
//!
//! A slot's rect list is the **complete pixel coverage of the interval
//! `(dirty_since_frame_seq, frame_seq]`** — every pixel that changed across those
//! presents lies inside some listed rect. That is what makes it safe to feed
//! Increment 1's raw fast path, whose viewer-side rule ([HLD §5, decision 13])
//! treats a rect update as that frame's *complete* change.
//!
//! The consumer is allowed to skip publishes — the driver never blocks on a busy
//! slot — so the list only describes the consumer's own history when the last
//! frame it actually took is at or past `dirty_since_frame_seq`. When it is not,
//! the change between the consumer's last frame and `dirty_since` is described by
//! nothing at all, and a small rect list would lie by omission. [`coverage_for`]
//! is that rule, and it is the only place it is decided.

/// The one well-known name. The driver creates this section once at start and
/// holds it across swapchain assignments; everything else is opened by a name
/// published *through* it.
pub const SECTION_NAME: &str = r"Global\mdrdp-idd";

/// Total section size. Four pages: one header, three slots.
pub const SECTION_BYTES: usize = 16384;

/// The only layout this server speaks.
pub const LAYOUT_VERSION: u32 = 1;

/// Pool depth. Fixed by the contract, not a tuning knob: the names, the event
/// array and the slot pages all assume it.
pub const SLOT_COUNT: usize = 3;

/// Page stride between records. Page-granular so a slot write never shares a cache
/// line — or a page — with the header or another slot.
pub const PAGE_BYTES: usize = 4096;

/// Most coverage rects a slot record can carry.
pub const MAX_COVERAGE_RECTS: usize = 64;

/// `coverage_rect_count` sentinel: the driver had no metadata for this frame.
pub const COVERAGE_ABSENT: u32 = 0xFFFF_FFFF;

/// `coverage_rect_count` sentinel: there were more rects than the record holds, so
/// the list is *incomplete*. Treated exactly like absent — an incomplete coverage
/// list is worse than none, because it looks usable.
pub const COVERAGE_OVERFLOWED: u32 = 0xFFFF_FFFE;

// Header field offsets, page-relative.
const OFF_LAYOUT_VERSION: usize = 0;
const OFF_GENERATION: usize = 4;
const OFF_RENDER_ADAPTER_LUID: usize = 8;
const OFF_WIDTH: usize = 16;
const OFF_HEIGHT: usize = 20;
const OFF_DXGI_FORMAT: usize = 24;
const OFF_SLOT_COUNT: usize = 28;
const OFF_NAME_SUFFIX: usize = 32;
const OFF_HEADER_SEQUENCE: usize = 40;

/// Bytes of the header page the contract actually defines.
pub const HEADER_BYTES: usize = 48;

/// Where the header's seqlock word sits, for a reader that samples it either side
/// of the body copy without parsing anything.
pub const HEADER_SEQUENCE_OFFSET: usize = OFF_HEADER_SEQUENCE;

// Slot field offsets, page-relative.
const OFF_SLOT_SEQUENCE: usize = 0;
const OFF_FRAME_SEQ: usize = 8;
const OFF_DIRTY_SINCE: usize = 16;
const OFF_PRESENT_QPC: usize = 24;
const OFF_COVERAGE_COUNT: usize = 32;
const OFF_COVERAGE_RECTS: usize = 40;
const RECT_BYTES: usize = 16;

/// Bytes of a slot page the contract defines.
pub const SLOT_BYTES: usize = OFF_COVERAGE_RECTS + MAX_COVERAGE_RECTS * RECT_BYTES;

/// Where a slot's seqlock word sits, on the same terms as
/// [`HEADER_SEQUENCE_OFFSET`].
pub const SLOT_SEQUENCE_OFFSET: usize = OFF_SLOT_SEQUENCE;

/// Byte offset of slot `slot` within the section.
pub const fn slot_offset(slot: usize) -> usize {
    PAGE_BYTES * (1 + slot)
}

/// The published header, once a stable read has succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolHeader {
    /// Bumps on every pool rebuild: swapchain reassignment, mode change, redeploy.
    /// `0` means the driver has created the section but published no pool yet.
    pub generation: u32,
    /// The adapter the driver's render device sits on. The consumer must build its
    /// own D3D11 device on this LUID or `OpenSharedResourceByName` has nothing to
    /// share with.
    pub render_adapter_luid: u64,
    pub width: u32,
    pub height: u32,
    pub dxgi_format: u32,
    /// Random per generation; qualifies the texture and event names so a
    /// straggling consumer's open handles cannot collide with a rebuilt pool.
    pub name_suffix: u64,
    /// The seqlock word this header was read at. Even, by construction.
    pub sequence: u32,
}

/// One rectangle exactly as the driver wrote it: raw, unclamped, RECT-shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// What a slot record says about the pixels that changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// The driver had no metadata. **Not** the same as zero rects.
    Absent,
    /// More rects than the record holds: the list that is there is incomplete.
    Overflowed,
    /// A complete coverage list for `(dirty_since_frame_seq, frame_seq]`.
    Rects(Vec<SectionRect>),
}

/// One slot's published record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRecord {
    /// The seqlock word this record was read at. Even, by construction.
    pub sequence: u32,
    /// The driver's contiguous presented-frame counter, 1-based per generation.
    /// `0` means the slot has never been published into.
    pub frame_seq: u64,
    /// The coverage list describes `(dirty_since_frame_seq, frame_seq]`.
    pub dirty_since_frame_seq: u64,
    /// QPC ticks at present. Same machine, so the server's own frequency converts
    /// it — that is the whole reason the decomposition survives the source swap.
    pub present_qpc: i64,
    pub coverage: Coverage,
}

/// Why a section read could not be believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// The buffer is shorter than the contract's fixed-size record.
    Truncated { need: usize, got: usize },
    /// The seqlock word is odd: the driver was mid-write when this was copied.
    MidWrite(u32),
    /// An all-zero header. The section exists but the driver has not written it
    /// yet — a startup race, not a mismatch, so the consumer retries rather than
    /// refusing.
    Uninitialised,
    /// A layout this build does not speak. Refused loudly: guessing at a changed
    /// byte contract is how a shared-memory reader corrupts a pipeline silently.
    UnsupportedVersion(u32),
    /// `slot_count` is not [`SLOT_COUNT`].
    SlotCount(u32),
    /// `coverage_rect_count` is neither a sentinel nor a count that fits.
    CoverageCount(u32),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { need, got } => {
                write!(f, "section record needs {need} bytes, got {got}")
            }
            Self::MidWrite(seq) => write!(f, "seqlock word {seq} is odd: write in progress"),
            Self::Uninitialised => write!(
                f,
                "{SECTION_NAME} exists but is still all zero: the driver has not published a pool"
            ),
            Self::UnsupportedVersion(v) => write!(
                f,
                "{SECTION_NAME} declares layout_version {v}; this build speaks {LAYOUT_VERSION}"
            ),
            Self::SlotCount(n) => write!(
                f,
                "{SECTION_NAME} declares {n} slots; the contract is {SLOT_COUNT}"
            ),
            Self::CoverageCount(n) => write!(
                f,
                "coverage_rect_count {n} is neither a sentinel nor <= {MAX_COVERAGE_RECTS}"
            ),
        }
    }
}

impl std::error::Error for LayoutError {}

fn need(bytes: &[u8], want: usize) -> Result<(), LayoutError> {
    if bytes.len() < want {
        return Err(LayoutError::Truncated {
            need: want,
            got: bytes.len(),
        });
    }
    Ok(())
}

fn u32_at(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

fn i32_at(bytes: &[u8], off: usize) -> i32 {
    u32_at(bytes, off) as i32
}

fn u64_at(bytes: &[u8], off: usize) -> u64 {
    u64::from(u32_at(bytes, off)) | (u64::from(u32_at(bytes, off + 4)) << 32)
}

fn i64_at(bytes: &[u8], off: usize) -> i64 {
    u64_at(bytes, off) as i64
}

/// The header's seqlock word, without parsing anything else. Sampled either side
/// of the body copy; equal-and-even is what makes the copy believable.
pub fn header_sequence(page: &[u8]) -> Result<u32, LayoutError> {
    need(page, OFF_HEADER_SEQUENCE + 4)?;
    Ok(u32_at(page, OFF_HEADER_SEQUENCE))
}

/// Parse the header page. Refuses a mid-write copy, an unwritten section, and any
/// layout this build does not speak.
pub fn parse_header(page: &[u8]) -> Result<PoolHeader, LayoutError> {
    need(page, HEADER_BYTES)?;
    let sequence = u32_at(page, OFF_HEADER_SEQUENCE);
    if sequence % 2 == 1 {
        return Err(LayoutError::MidWrite(sequence));
    }
    let version = u32_at(page, OFF_LAYOUT_VERSION);
    if version == 0 {
        // A section is zero-filled at creation, so version 0 is the window between
        // CreateFileMapping and the driver's first header write — a race to wait
        // out, not a contract mismatch to refuse.
        return Err(LayoutError::Uninitialised);
    }
    if version != LAYOUT_VERSION {
        return Err(LayoutError::UnsupportedVersion(version));
    }
    let slots = u32_at(page, OFF_SLOT_COUNT);
    if slots != SLOT_COUNT as u32 {
        return Err(LayoutError::SlotCount(slots));
    }
    Ok(PoolHeader {
        generation: u32_at(page, OFF_GENERATION),
        render_adapter_luid: u64_at(page, OFF_RENDER_ADAPTER_LUID),
        width: u32_at(page, OFF_WIDTH),
        height: u32_at(page, OFF_HEIGHT),
        dxgi_format: u32_at(page, OFF_DXGI_FORMAT),
        name_suffix: u64_at(page, OFF_NAME_SUFFIX),
        sequence,
    })
}

/// A slot's seqlock word, on the same terms as [`header_sequence`].
pub fn slot_sequence(page: &[u8]) -> Result<u32, LayoutError> {
    need(page, OFF_SLOT_SEQUENCE + 4)?;
    Ok(u32_at(page, OFF_SLOT_SEQUENCE))
}

/// Parse one slot page.
///
/// A rect list is only read when the count is a real count; both sentinels return
/// their own [`Coverage`] variant, so no caller can mistake "we do not know what
/// changed" for "nothing changed".
pub fn parse_slot(page: &[u8]) -> Result<SlotRecord, LayoutError> {
    need(page, SLOT_BYTES)?;
    let sequence = u32_at(page, OFF_SLOT_SEQUENCE);
    if sequence % 2 == 1 {
        return Err(LayoutError::MidWrite(sequence));
    }
    let count = u32_at(page, OFF_COVERAGE_COUNT);
    let coverage = match count {
        COVERAGE_ABSENT => Coverage::Absent,
        COVERAGE_OVERFLOWED => Coverage::Overflowed,
        n if n as usize <= MAX_COVERAGE_RECTS => {
            let mut rects = Vec::with_capacity(n as usize);
            for i in 0..n as usize {
                let base = OFF_COVERAGE_RECTS + i * RECT_BYTES;
                rects.push(SectionRect {
                    left: i32_at(page, base),
                    top: i32_at(page, base + 4),
                    right: i32_at(page, base + 8),
                    bottom: i32_at(page, base + 12),
                });
            }
            Coverage::Rects(rects)
        }
        n => return Err(LayoutError::CoverageCount(n)),
    };
    Ok(SlotRecord {
        sequence,
        frame_seq: u64_at(page, OFF_FRAME_SEQ),
        dirty_since_frame_seq: u64_at(page, OFF_DIRTY_SINCE),
        present_qpc: i64_at(page, OFF_PRESENT_QPC),
        coverage,
    })
}

/// Slots holding a frame this consumer has not taken yet, **newest first**.
///
/// `None` entries are slots whose seqlock never settled — skipped rather than
/// guessed at. `frame_seq <= last_consumed` covers both the already-taken slot and
/// the never-published one (`frame_seq` is 1-based, so 0 is "empty"). Ties break
/// towards the lower index, which cannot happen with a contiguous counter but must
/// still be defined for the order to be a function.
///
/// The list is a list, not a winner, because the newest slot can turn out to be
/// poisoned (`WAIT_ABANDONED`) at acquire time; the consumer then walks to the next
/// one instead of losing the wakeup entirely.
pub fn ready_slots(records: &[Option<SlotRecord>], last_consumed: u64) -> Vec<usize> {
    let mut ready: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.as_ref().is_some_and(|r| r.frame_seq > last_consumed))
        .map(|(i, _)| i)
        .collect();
    ready.sort_by(|&a, &b| {
        let (fa, fb) = (
            records[a].as_ref().map_or(0, |r| r.frame_seq),
            records[b].as_ref().map_or(0, |r| r.frame_seq),
        );
        fb.cmp(&fa).then(a.cmp(&b))
    });
    ready
}

/// The coverage invariant. `Some` only when this consumer's own history makes the
/// slot's rect list a complete description of what it has not seen.
///
/// Two ways to get `None`, and they are the same answer for the pipeline — take
/// the full-frame path, which is always safe:
///
/// * the count was a sentinel, so the list is absent or incomplete; or
/// * `dirty_since_frame_seq` is ahead of the last frame this consumer took, so the
///   list starts after a gap it does not describe.
///
/// `last_consumed == 0` — no baseline at all — is refused outright, even against
/// the generation's first publish (`dirty_since = 0`). That record's list truly
/// covers `(0, 1]`, but the consumer's real gap is the whole surface, not one
/// frame's delta: a caret-blink rect offered as "complete" there would seed the
/// exactness chain with a lie. The pipeline's `Recreated` handling and the
/// viewer's before-base skip both happen to mask this today; the invariant must
/// not depend on either of them (review, M2).
pub fn coverage_for(slot: &SlotRecord, last_consumed: u64) -> Option<&[SectionRect]> {
    match &slot.coverage {
        Coverage::Rects(rects)
            if last_consumed > 0 && last_consumed >= slot.dirty_since_frame_seq =>
        {
            Some(rects)
        }
        _ => None,
    }
}

/// A section rect clamped to the pool's pixel dimensions, as the pipeline wants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Clamp one driver rect to the pool surface. Degenerate, inverted and fully
/// off-surface rects vanish — a `CopySubresourceRegion` box outside the source is
/// undefined, and the wire's coordinates are `u16`.
pub fn clamp(r: &SectionRect, width: u32, height: u32) -> Option<CoverageRect> {
    let x0 = r.left.max(0) as u32;
    let y0 = r.top.max(0) as u32;
    let x1 = (r.right.max(0) as u32).min(width);
    let y1 = (r.bottom.max(0) as u32).min(height);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(CoverageRect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

/// Name of the shared texture backing slot `slot` of `generation`.
///
/// Generation-qualified with the header's random suffix on purpose: a fixed name
/// collides with a straggling consumer's still-open handle on the next pool
/// rebuild (`DXGI_ERROR_NAME_ALREADY_EXISTS`) and invites pre-creation squatting.
pub fn texture_name(generation: u32, name_suffix: u64, slot: usize) -> String {
    format!(r"Global\mdrdp-idd-tex-{generation}-{name_suffix:016x}-{slot}")
}

/// Name of the event the driver signals after publishing into slot `slot`.
pub fn event_name(generation: u32, name_suffix: u64, slot: usize) -> String {
    format!(r"Global\mdrdp-idd-evt-{generation}-{name_suffix:016x}-{slot}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole section with a **distinct** value in every field, written at
    /// literal offsets.
    ///
    /// Literal offsets, not the module's constants: a fixture that indexes through
    /// the same constant the parser does agrees with itself whatever the constant
    /// says, so it cannot catch a transposed offset — which is the one bug this
    /// contract can have that nothing else would find until a live driver run.
    /// Distinct values everywhere, for the same reason: two fields holding 1920
    /// cannot tell a swap from a correct read.
    fn fixture() -> Vec<u8> {
        let mut b = vec![0u8; 16384];
        // Header page.
        b[0..4].copy_from_slice(&1u32.to_le_bytes()); // layout_version
        b[4..8].copy_from_slice(&7u32.to_le_bytes()); // generation
        b[8..16].copy_from_slice(&0x0000_002A_1234_5678u64.to_le_bytes()); // luid
        b[16..20].copy_from_slice(&1920u32.to_le_bytes()); // width
        b[20..24].copy_from_slice(&1080u32.to_le_bytes()); // height
        b[24..28].copy_from_slice(&87u32.to_le_bytes()); // dxgi_format (BGRA8)
        b[28..32].copy_from_slice(&3u32.to_le_bytes()); // slot_count
        b[32..40].copy_from_slice(&0x0BAD_C0DE_DEAD_BEEFu64.to_le_bytes()); // name_suffix
        b[40..44].copy_from_slice(&12u32.to_le_bytes()); // header_sequence (even)

        // Slot 0 at 4096, with two rects.
        b[4096..4100].copy_from_slice(&4u32.to_le_bytes()); // sequence
        b[4104..4112].copy_from_slice(&99u64.to_le_bytes()); // frame_seq
        b[4112..4120].copy_from_slice(&95u64.to_le_bytes()); // dirty_since
        b[4120..4128].copy_from_slice(&1_234_567_890_123i64.to_le_bytes()); // present_qpc
        b[4128..4132].copy_from_slice(&2u32.to_le_bytes()); // coverage_rect_count
        for (i, v) in [11i32, 22, 33, 44, 55, 66, 77, 88].iter().enumerate() {
            let off = 4136 + i * 4;
            b[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }

        // Slot 1 at 8192 — an older frame, so slot selection has something to reject.
        b[8192..8196].copy_from_slice(&6u32.to_le_bytes());
        b[8200..8208].copy_from_slice(&98u64.to_le_bytes());
        b[8208..8216].copy_from_slice(&97u64.to_le_bytes());
        b[8216..8224].copy_from_slice(&999i64.to_le_bytes());
        b[8224..8228].copy_from_slice(&COVERAGE_ABSENT.to_le_bytes());

        // Slot 2 at 12288 — never published.
        b[12288..12292].copy_from_slice(&2u32.to_le_bytes());
        b[12296..12304].copy_from_slice(&0u64.to_le_bytes());
        b[12304..12312].copy_from_slice(&0u64.to_le_bytes());
        b[12312..12320].copy_from_slice(&0i64.to_le_bytes());
        b[12320..12324].copy_from_slice(&COVERAGE_OVERFLOWED.to_le_bytes());
        b
    }

    fn header_page(b: &[u8]) -> &[u8] {
        &b[..PAGE_BYTES]
    }

    fn slot_page(b: &[u8], slot: usize) -> &[u8] {
        &b[slot_offset(slot)..slot_offset(slot) + PAGE_BYTES]
    }

    #[test]
    fn the_section_geometry_is_the_documented_one() {
        // The C++ driver is written against these numbers; a change here without a
        // change there is a silent contract break, so they are asserted as
        // literals rather than derived.
        assert_eq!(SECTION_BYTES, 16384);
        assert_eq!(PAGE_BYTES, 4096);
        assert_eq!(SLOT_COUNT, 3);
        assert_eq!(MAX_COVERAGE_RECTS, 64);
        assert_eq!(slot_offset(0), 4096);
        assert_eq!(slot_offset(1), 8192);
        assert_eq!(slot_offset(2), 12288);
        assert_eq!(SLOT_BYTES, 40 + 64 * 16);
        assert_eq!(HEADER_BYTES, 48);
        assert_eq!(HEADER_SEQUENCE_OFFSET, 40);
        assert_eq!(SLOT_SEQUENCE_OFFSET, 0);
        assert!(
            slot_offset(SLOT_COUNT - 1) + SLOT_BYTES <= SECTION_BYTES,
            "the last slot record runs off the end of the section"
        );
    }

    #[test]
    fn every_header_field_reads_back_from_its_own_offset() {
        let b = fixture();
        let h = parse_header(header_page(&b)).unwrap();
        assert_eq!(h.generation, 7);
        assert_eq!(h.render_adapter_luid, 0x0000_002A_1234_5678);
        assert_eq!(h.width, 1920);
        assert_eq!(h.height, 1080);
        assert_eq!(h.dxgi_format, 87);
        assert_eq!(h.name_suffix, 0x0BAD_C0DE_DEAD_BEEF);
        assert_eq!(h.sequence, 12);
        assert_eq!(header_sequence(header_page(&b)).unwrap(), 12);
    }

    #[test]
    fn every_slot_field_reads_back_from_its_own_offset() {
        let b = fixture();
        let s = parse_slot(slot_page(&b, 0)).unwrap();
        assert_eq!(s.sequence, 4);
        assert_eq!(s.frame_seq, 99);
        assert_eq!(s.dirty_since_frame_seq, 95);
        assert_eq!(s.present_qpc, 1_234_567_890_123);
        assert_eq!(
            s.coverage,
            Coverage::Rects(vec![
                SectionRect {
                    left: 11,
                    top: 22,
                    right: 33,
                    bottom: 44,
                },
                SectionRect {
                    left: 55,
                    top: 66,
                    right: 77,
                    bottom: 88,
                },
            ])
        );
        assert_eq!(slot_sequence(slot_page(&b, 0)).unwrap(), 4);
    }

    #[test]
    fn a_negative_present_qpc_and_rect_survive_the_signed_reads() {
        // present_qpc and the rect fields are the only signed values in the
        // contract; read as unsigned they would come back astronomically large,
        // which a clamp would then silently swallow.
        let mut b = fixture();
        b[4120..4128].copy_from_slice(&(-42i64).to_le_bytes());
        b[4136..4140].copy_from_slice(&(-5i32).to_le_bytes());
        let s = parse_slot(slot_page(&b, 0)).unwrap();
        assert_eq!(s.present_qpc, -42);
        let Coverage::Rects(rects) = &s.coverage else {
            panic!("expected rects, got {:?}", s.coverage);
        };
        assert_eq!(rects[0].left, -5);
    }

    #[test]
    fn an_odd_seqlock_word_is_refused_rather_than_read() {
        let mut b = fixture();
        b[40..44].copy_from_slice(&13u32.to_le_bytes());
        assert_eq!(
            parse_header(header_page(&b)),
            Err(LayoutError::MidWrite(13))
        );
        // The bare sequence read still works — that is what the retry loop samples.
        assert_eq!(header_sequence(header_page(&b)).unwrap(), 13);

        let mut b = fixture();
        b[4096..4100].copy_from_slice(&5u32.to_le_bytes());
        assert_eq!(parse_slot(slot_page(&b, 0)), Err(LayoutError::MidWrite(5)));
        assert_eq!(slot_sequence(slot_page(&b, 0)).unwrap(), 5);
    }

    #[test]
    fn a_zero_header_is_a_startup_race_and_a_wrong_version_is_a_refusal() {
        // The two must not collapse into one answer: the first is waited out, the
        // second is fatal.
        let zero = vec![0u8; PAGE_BYTES];
        assert_eq!(parse_header(&zero), Err(LayoutError::Uninitialised));

        let mut b = fixture();
        b[0..4].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            parse_header(header_page(&b)),
            Err(LayoutError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn a_slot_count_that_is_not_three_is_refused() {
        let mut b = fixture();
        b[28..32].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(
            parse_header(header_page(&b)),
            Err(LayoutError::SlotCount(4))
        );
    }

    #[test]
    fn a_truncated_record_is_refused_rather_than_read_short() {
        assert!(matches!(
            parse_header(&[0u8; 8]),
            Err(LayoutError::Truncated { need: 48, got: 8 })
        ));
        assert!(matches!(
            parse_slot(&[0u8; 64]),
            Err(LayoutError::Truncated { .. })
        ));
    }

    #[test]
    fn both_coverage_sentinels_are_their_own_state_not_a_rect_count() {
        let b = fixture();
        assert_eq!(
            parse_slot(slot_page(&b, 1)).unwrap().coverage,
            Coverage::Absent
        );
        assert_eq!(
            parse_slot(slot_page(&b, 2)).unwrap().coverage,
            Coverage::Overflowed
        );
    }

    #[test]
    fn a_coverage_count_past_the_record_is_refused() {
        let mut b = fixture();
        b[4128..4132].copy_from_slice(&65u32.to_le_bytes());
        assert_eq!(
            parse_slot(slot_page(&b, 0)),
            Err(LayoutError::CoverageCount(65))
        );
        // 64 is the boundary and must still parse.
        b[4128..4132].copy_from_slice(&64u32.to_le_bytes());
        let s = parse_slot(slot_page(&b, 0)).unwrap();
        assert!(matches!(s.coverage, Coverage::Rects(ref r) if r.len() == 64));
    }

    fn record(frame_seq: u64, dirty_since: u64, coverage: Coverage) -> SlotRecord {
        SlotRecord {
            sequence: 2,
            frame_seq,
            dirty_since_frame_seq: dirty_since,
            present_qpc: 1,
            coverage,
        }
    }

    fn one_rect() -> Coverage {
        Coverage::Rects(vec![SectionRect {
            left: 1,
            top: 2,
            right: 3,
            bottom: 4,
        }])
    }

    #[test]
    fn slot_selection_prefers_the_highest_frame_seq_and_drops_the_rest() {
        // Distinct frame_seqs in a deliberately non-monotonic slot order: a
        // selection that returned the last slot, or the first, would still pass
        // against an ascending fixture.
        let slots = [
            Some(record(41, 0, one_rect())),
            Some(record(97, 0, one_rect())),
            Some(record(63, 0, one_rect())),
        ];
        assert_eq!(ready_slots(&slots, 0), vec![1, 2, 0]);
        // Already-consumed frames drop out, newest-first order survives.
        assert_eq!(ready_slots(&slots, 50), vec![1, 2]);
        assert_eq!(ready_slots(&slots, 97), Vec::<usize>::new());
    }

    #[test]
    fn a_torn_or_empty_slot_is_never_selected() {
        // `None` is a slot whose seqlock never settled; frame_seq 0 is one the
        // driver has never published into. Neither may be handed to the pipeline.
        let slots = [
            None,
            Some(record(0, 0, one_rect())),
            Some(record(5, 0, one_rect())),
        ];
        assert_eq!(ready_slots(&slots, 0), vec![2]);
    }

    #[test]
    fn coverage_is_offered_only_when_the_consumers_own_history_covers_the_gap() {
        // The rule this feeds is the viewer's exactness invariant, so each way of
        // failing it must produce None rather than a plausible-looking rect list.
        let complete = record(10, 4, one_rect());
        assert_eq!(coverage_for(&complete, 4).map(<[_]>::len), Some(1));
        assert_eq!(coverage_for(&complete, 9).map(<[_]>::len), Some(1));
        // dirty_since ahead of what this consumer last took: the interval starts
        // after a gap nothing describes.
        assert!(coverage_for(&complete, 3).is_none());
        assert!(coverage_for(&complete, 0).is_none());
        // Sentinels are None however good the history is.
        assert!(coverage_for(&record(10, 4, Coverage::Absent), 9).is_none());
        assert!(coverage_for(&record(10, 4, Coverage::Overflowed), 9).is_none());
    }

    #[test]
    fn a_consumer_with_no_baseline_is_refused_even_by_the_generations_first_publish() {
        // The record's list truly covers (0, 1], but a consumer at last_consumed 0
        // has no baseline at all — its gap is the whole surface, and one frame's
        // delta offered as "complete" would seed the exactness chain with a lie.
        assert!(coverage_for(&record(1, 0, one_rect()), 0).is_none());
        // With a baseline, the same record is honest coverage.
        assert!(coverage_for(&record(2, 1, one_rect()), 1).is_some());
        // And a missed first publish is refused whatever the baseline state.
        assert!(coverage_for(&record(4, 3, one_rect()), 0).is_none());
    }

    #[test]
    fn clamping_keeps_the_inside_and_drops_the_degenerate() {
        let r = |l, t, rt, b| SectionRect {
            left: l,
            top: t,
            right: rt,
            bottom: b,
        };
        assert_eq!(
            clamp(&r(10, 20, 30, 50), 1920, 1080),
            Some(CoverageRect {
                x: 10,
                y: 20,
                w: 20,
                h: 30
            })
        );
        // Overhanging on both axes, and negative origin: clamped, not dropped.
        assert_eq!(
            clamp(&r(-5, -9, 1930, 1090), 1920, 1080),
            Some(CoverageRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080
            })
        );
        // Empty, inverted, and wholly off-surface all vanish.
        assert_eq!(clamp(&r(10, 10, 10, 20), 1920, 1080), None);
        assert_eq!(clamp(&r(30, 10, 20, 20), 1920, 1080), None);
        assert_eq!(clamp(&r(2000, 10, 2100, 20), 1920, 1080), None);
    }

    #[test]
    fn object_names_carry_the_generation_the_suffix_and_the_slot() {
        // All three inputs distinct and none a substring of another, so a name
        // built from the wrong one cannot accidentally match.
        assert_eq!(
            texture_name(7, 0x0BAD_C0DE_DEAD_BEEF, 2),
            r"Global\mdrdp-idd-tex-7-0badc0dedeadbeef-2"
        );
        assert_eq!(
            event_name(7, 0x0BAD_C0DE_DEAD_BEEF, 2),
            r"Global\mdrdp-idd-evt-7-0badc0dedeadbeef-2"
        );
        // The suffix is zero-padded to 16 lowercase hex digits: an unpadded name
        // would not match what the driver created.
        assert_eq!(
            texture_name(1, 0xFF, 0),
            r"Global\mdrdp-idd-tex-1-00000000000000ff-0"
        );
    }
}
