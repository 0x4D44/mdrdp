//! Atomic regional/full video update payloads.
//!
//! The fixed header is `frame_seq`, geometry, `kind`, `tile_count`,
//! `block_size`, then the wire-v10 `required_parts` mask (`RAW=1`,
//! `VIDEO=2`, or both). Move preludes use their unchanged independent format.

use crate::adaptive::{Region, MAX_PLAN_BLOCKS};
use crate::framing::DEFAULT_MAX_PAYLOAD;

pub use crate::framing::{RequiredParts, UpdateParts};

const HEADER_LEN: usize = 8 + 4 + 4 + 1 + 1 + 2 + 1;
const TILE_HEADER_LEN: usize = 1 + 1 + 2 + 4;
const REGION_LEN: usize = 4 * 2;
pub const MAX_VIDEO_TILES: usize = 16;
pub const MAX_COVERAGE_RECTS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VideoKind {
    Regional = 0,
    Full = 1,
    Recovery = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTile {
    pub tile_id: u8,
    pub coverage: Vec<Region>,
    pub au: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoUpdate {
    pub frame_seq: u64,
    pub frame_width: u32,
    pub frame_height: u32,
    pub block_size: u16,
    pub kind: VideoKind,
    /// Wire-v10 lanes that must complete this logical update.
    pub required_parts: UpdateParts,
    pub tiles: Vec<VideoTile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoError {
    Truncated { field: &'static str },
    TrailingBytes { extra: usize },
    InvalidGeometry,
    TooManyBlocks { count: u64 },
    InvalidKind { kind: u8 },
    NonZeroReserved { tile_id: u8 },
    InvalidTileCount { count: usize },
    DuplicateTile { tile_id: u8 },
    EmptyCoverage { tile_id: u8 },
    TooManyCoverageRects { count: usize },
    InvalidCoverage { tile_id: u8, rect_index: usize },
    DuplicateCoverage { block_x: u32, block_y: u32 },
    EmptyAccessUnit { tile_id: u8 },
    AccessUnitBytesLimit { bytes: u64, limit: u64 },
    IncompleteFullCoverage { covered: u64, required: u64 },
    RegionalCoversFullFrame,
    RecoveryNotIdr { tile_id: u8 },
    InvalidRequiredParts { bits: u8 },
}

impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "video update: {self:?}")
    }
}

impl std::error::Error for VideoError {}

pub fn encoded_len(update: &VideoUpdate) -> usize {
    HEADER_LEN
        + update
            .tiles
            .iter()
            .map(|tile| TILE_HEADER_LEN + tile.coverage.len() * REGION_LEN + tile.au.len())
            .sum::<usize>()
}

pub fn encode(update: &VideoUpdate, out: &mut Vec<u8>) {
    assert!(!update.tiles.is_empty() && update.tiles.len() <= MAX_VIDEO_TILES);
    assert!(update.block_size > 0);
    out.reserve(encoded_len(update));
    out.extend_from_slice(&update.frame_seq.to_le_bytes());
    out.extend_from_slice(&update.frame_width.to_le_bytes());
    out.extend_from_slice(&update.frame_height.to_le_bytes());
    out.push(update.kind as u8);
    out.push(update.tiles.len() as u8);
    out.extend_from_slice(&update.block_size.to_le_bytes());
    assert!(
        UpdateParts::from_bits(update.required_parts.bits()).is_some(),
        "video update: invalid required-parts mask {:#04x}",
        update.required_parts.bits()
    );
    out.push(update.required_parts.bits());
    for tile in &update.tiles {
        assert!(!tile.coverage.is_empty() && tile.coverage.len() <= u16::MAX as usize);
        assert!(tile.au.len() <= u32::MAX as usize);
        out.push(tile.tile_id);
        out.push(0);
        out.extend_from_slice(&(tile.coverage.len() as u16).to_le_bytes());
        out.extend_from_slice(&(tile.au.len() as u32).to_le_bytes());
        for region in &tile.coverage {
            assert!(
                region.x <= u16::MAX as u32
                    && region.y <= u16::MAX as u32
                    && region.width <= u16::MAX as u32
                    && region.height <= u16::MAX as u32
            );
            out.extend_from_slice(&(region.x as u16).to_le_bytes());
            out.extend_from_slice(&(region.y as u16).to_le_bytes());
            out.extend_from_slice(&(region.width as u16).to_le_bytes());
            out.extend_from_slice(&(region.height as u16).to_le_bytes());
        }
        out.extend_from_slice(&tile.au);
    }
}

pub fn decode(payload: &[u8]) -> Result<VideoUpdate, VideoError> {
    let mut reader = Reader::new(payload);
    let frame_seq = reader.u64("frame_seq")?;
    let frame_width = reader.u32("frame_width")?;
    let frame_height = reader.u32("frame_height")?;
    let kind = match reader.u8("kind")? {
        0 => VideoKind::Regional,
        1 => VideoKind::Full,
        2 => VideoKind::Recovery,
        kind => return Err(VideoError::InvalidKind { kind }),
    };
    let tile_count = reader.u8("tile_count")? as usize;
    let block_size = reader.u16("block_size")?;
    let required_parts_bits = reader.u8("required_parts")?;
    let required_parts =
        UpdateParts::from_bits(required_parts_bits).ok_or(VideoError::InvalidRequiredParts {
            bits: required_parts_bits,
        })?;
    if frame_width == 0
        || frame_height == 0
        || frame_width > u16::MAX as u32
        || frame_height > u16::MAX as u32
        || block_size == 0
    {
        return Err(VideoError::InvalidGeometry);
    }
    if tile_count == 0 || tile_count > MAX_VIDEO_TILES {
        return Err(VideoError::InvalidTileCount { count: tile_count });
    }

    let blocks_w = frame_width.div_ceil(u32::from(block_size));
    let blocks_h = frame_height.div_ceil(u32::from(block_size));
    let required_blocks = u64::from(blocks_w) * u64::from(blocks_h);
    if required_blocks > MAX_PLAN_BLOCKS {
        return Err(VideoError::TooManyBlocks {
            count: required_blocks,
        });
    }
    let mut covered = vec![false; required_blocks as usize];
    let mut covered_count = 0u64;
    let mut seen_tiles = [false; 256];
    let mut coverage_count = 0usize;
    let mut au_bytes = 0u64;
    let mut tiles = Vec::with_capacity(tile_count);

    for _ in 0..tile_count {
        let tile_id = reader.u8("tile.id")?;
        if reader.u8("tile.reserved")? != 0 {
            return Err(VideoError::NonZeroReserved { tile_id });
        }
        if std::mem::replace(&mut seen_tiles[tile_id as usize], true) {
            return Err(VideoError::DuplicateTile { tile_id });
        }
        let tile_coverage_count = reader.u16("tile.coverage_count")? as usize;
        let au_len = reader.u32("tile.au_len")? as usize;
        if tile_coverage_count == 0 {
            return Err(VideoError::EmptyCoverage { tile_id });
        }
        coverage_count = coverage_count.saturating_add(tile_coverage_count);
        if coverage_count > MAX_COVERAGE_RECTS {
            return Err(VideoError::TooManyCoverageRects {
                count: coverage_count,
            });
        }
        au_bytes = au_bytes.saturating_add(au_len as u64);
        if au_bytes > DEFAULT_MAX_PAYLOAD as u64 {
            return Err(VideoError::AccessUnitBytesLimit {
                bytes: au_bytes,
                limit: DEFAULT_MAX_PAYLOAD as u64,
            });
        }

        let mut coverage = Vec::with_capacity(tile_coverage_count);
        for rect_index in 0..tile_coverage_count {
            let region = Region {
                x: u32::from(reader.u16("coverage.x")?),
                y: u32::from(reader.u16("coverage.y")?),
                width: u32::from(reader.u16("coverage.width")?),
                height: u32::from(reader.u16("coverage.height")?),
            };
            let right = region.x + region.width;
            let bottom = region.y + region.height;
            let block = u32::from(block_size);
            if region.width == 0
                || region.height == 0
                || right > frame_width
                || bottom > frame_height
                || region.x % block != 0
                || region.y % block != 0
                || (right != frame_width && right % block != 0)
                || (bottom != frame_height && bottom % block != 0)
            {
                return Err(VideoError::InvalidCoverage {
                    tile_id,
                    rect_index,
                });
            }
            for block_y in region.y / block..bottom.div_ceil(block) {
                for block_x in region.x / block..right.div_ceil(block) {
                    let slot = &mut covered[(block_y * blocks_w + block_x) as usize];
                    if std::mem::replace(slot, true) {
                        return Err(VideoError::DuplicateCoverage { block_x, block_y });
                    }
                    covered_count += 1;
                }
            }
            coverage.push(region);
        }
        if au_len == 0 {
            return Err(VideoError::EmptyAccessUnit { tile_id });
        }
        let au = reader.take(au_len, "tile.au")?.to_vec();
        tiles.push(VideoTile {
            tile_id,
            coverage,
            au,
        });
    }

    if reader.remaining() != 0 {
        return Err(VideoError::TrailingBytes {
            extra: reader.remaining(),
        });
    }
    match kind {
        VideoKind::Regional if covered_count == required_blocks => {
            return Err(VideoError::RegionalCoversFullFrame);
        }
        VideoKind::Full | VideoKind::Recovery if covered_count != required_blocks => {
            return Err(VideoError::IncompleteFullCoverage {
                covered: covered_count,
                required: required_blocks,
            });
        }
        _ => {}
    }
    if kind == VideoKind::Recovery {
        for tile in &tiles {
            if !crate::annexb::avc_contains_idr(&tile.au) {
                return Err(VideoError::RecoveryNotIdr {
                    tile_id: tile.tile_id,
                });
            }
        }
    }

    Ok(VideoUpdate {
        frame_seq,
        frame_width,
        frame_height,
        block_size,
        kind,
        required_parts,
        tiles,
    })
}

struct Reader<'a> {
    payload: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(payload: &'a [u8]) -> Self {
        Self {
            payload,
            position: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.payload.len() - self.position
    }

    fn take(&mut self, count: usize, field: &'static str) -> Result<&'a [u8], VideoError> {
        if count > self.remaining() {
            return Err(VideoError::Truncated { field });
        }
        let bytes = &self.payload[self.position..self.position + count];
        self.position += count;
        Ok(bytes)
    }

    fn u8(&mut self, field: &'static str) -> Result<u8, VideoError> {
        Ok(self.take(1, field)?[0])
    }

    fn u16(&mut self, field: &'static str) -> Result<u16, VideoError> {
        Ok(u16::from_le_bytes(
            self.take(2, field)?.try_into().expect("two bytes"),
        ))
    }

    fn u32(&mut self, field: &'static str) -> Result<u32, VideoError> {
        Ok(u32::from_le_bytes(
            self.take(4, field)?.try_into().expect("four bytes"),
        ))
    }

    fn u64(&mut self, field: &'static str) -> Result<u64, VideoError> {
        Ok(u64::from_le_bytes(
            self.take(8, field)?.try_into().expect("eight bytes"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive::Region;

    fn regional() -> VideoUpdate {
        VideoUpdate {
            frame_seq: 90,
            frame_width: 64,
            frame_height: 32,
            block_size: 16,
            kind: VideoKind::Regional,
            required_parts: UpdateParts::VIDEO,
            tiles: vec![
                VideoTile {
                    tile_id: 0,
                    coverage: vec![Region {
                        x: 0,
                        y: 0,
                        width: 16,
                        height: 16,
                    }],
                    au: vec![0, 0, 1, 0x65, 1],
                },
                VideoTile {
                    tile_id: 1,
                    coverage: vec![Region {
                        x: 32,
                        y: 16,
                        width: 32,
                        height: 16,
                    }],
                    au: vec![0, 0, 1, 0x41, 2],
                },
            ],
        }
    }

    fn payload(update: &VideoUpdate) -> Vec<u8> {
        let mut payload = Vec::new();
        encode(update, &mut payload);
        payload
    }

    #[test]
    fn regional_update_round_trips_with_exact_tile_coverage() {
        let update = regional();
        assert_eq!(decode(&payload(&update)).unwrap(), update);
    }

    #[test]
    fn wire_v10_required_parts_round_trip_and_rejects_empty_or_unknown_masks() {
        let mut update = regional();
        update.required_parts = UpdateParts::VIDEO;
        let bytes = payload(&update);
        assert_eq!(decode(&bytes).unwrap().required_parts, UpdateParts::VIDEO);

        let mut empty = bytes.clone();
        empty[20] = 0;
        assert!(matches!(
            decode(&empty),
            Err(VideoError::InvalidRequiredParts { bits: 0 })
        ));

        let mut unknown = bytes;
        unknown[20] = 0x80;
        assert!(matches!(
            decode(&unknown),
            Err(VideoError::InvalidRequiredParts { bits: 0x80 })
        ));
    }

    #[test]
    fn header_and_first_tile_fields_have_fixed_independent_offsets() {
        let payload = payload(&regional());
        assert_eq!(u64::from_le_bytes(payload[0..8].try_into().unwrap()), 90);
        assert_eq!(u32::from_le_bytes(payload[8..12].try_into().unwrap()), 64);
        assert_eq!(u32::from_le_bytes(payload[12..16].try_into().unwrap()), 32);
        assert_eq!(payload[16], VideoKind::Regional as u8);
        assert_eq!(payload[17], 2);
        assert_eq!(u16::from_le_bytes(payload[18..20].try_into().unwrap()), 16);
        assert_eq!(payload[20], UpdateParts::VIDEO.bits());
        assert_eq!(payload[21], 0);
        assert_eq!(u16::from_le_bytes(payload[23..25].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(payload[25..29].try_into().unwrap()), 5);
    }

    #[test]
    fn duplicate_tile_ids_are_refused() {
        let mut update = regional();
        update.tiles[1].tile_id = 0;
        assert_eq!(
            decode(&payload(&update)),
            Err(VideoError::DuplicateTile { tile_id: 0 })
        );
    }

    #[test]
    fn overlapping_coverage_is_refused() {
        let mut update = regional();
        update.tiles[1].coverage[0] = Region {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
        };
        assert_eq!(
            decode(&payload(&update)),
            Err(VideoError::DuplicateCoverage {
                block_x: 0,
                block_y: 0
            })
        );
    }

    #[test]
    fn full_and_recovery_must_cover_every_block() {
        for kind in [VideoKind::Full, VideoKind::Recovery] {
            let mut update = regional();
            update.kind = kind;
            assert_eq!(
                decode(&payload(&update)),
                Err(VideoError::IncompleteFullCoverage {
                    covered: 3,
                    required: 8
                })
            );
        }
    }

    #[test]
    fn every_truncation_point_is_refused() {
        let payload = payload(&regional());
        for end in 0..payload.len() {
            assert!(decode(&payload[..end]).is_err(), "accepted {end}");
        }
    }

    #[test]
    fn aggregate_access_unit_limit_is_checked_before_allocation() {
        let mut payload = payload(&regional());
        payload[25..29].copy_from_slice(&((DEFAULT_MAX_PAYLOAD as u32) + 1).to_le_bytes());

        assert_eq!(
            decode(&payload),
            Err(VideoError::AccessUnitBytesLimit {
                bytes: DEFAULT_MAX_PAYLOAD as u64 + 1,
                limit: DEFAULT_MAX_PAYLOAD as u64
            })
        );
    }

    #[test]
    fn recovery_requires_an_idr_from_every_tile() {
        let update = VideoUpdate {
            frame_seq: 91,
            frame_width: 32,
            frame_height: 16,
            block_size: 16,
            kind: VideoKind::Recovery,
            required_parts: UpdateParts::VIDEO,
            tiles: vec![VideoTile {
                tile_id: 0,
                coverage: vec![Region {
                    x: 0,
                    y: 0,
                    width: 32,
                    height: 16,
                }],
                au: vec![0, 0, 1, 0x41, 2],
            }],
        };

        assert_eq!(
            decode(&payload(&update)),
            Err(VideoError::RecoveryNotIdr { tile_id: 0 })
        );
    }
}
