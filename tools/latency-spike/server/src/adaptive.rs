//! Pure coverage planning for Rhydra's mutually-exclusive adaptive updates.
//!
//! Capture and codec code are deliberately absent. This module turns pixel damage
//! into codec-block ownership and is shared by the server tests on every platform.

/// A non-empty pixel rectangle in desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One codec block. Edge blocks may be smaller than `block_size` in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Block {
    pub x: u32,
    pub y: u32,
}

/// Disjoint ownership of every changed block in one incremental update.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IncrementalPlan {
    pub moves: Vec<Block>,
    pub raw: Vec<Block>,
    pub video: Vec<Block>,
}

/// Full fallback is a distinct state, so callers cannot accidentally combine it
/// with incremental operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatePlan {
    Incremental(IncrementalPlan),
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    InvalidGeometry,
    TooManyBlocks { count: u64, limit: u64 },
    ZeroArea { set: &'static str, index: usize },
    OutOfFrame { set: &'static str, index: usize },
    CandidateOutsideDamage { set: &'static str, block: Block },
    InvalidBlock { index: usize },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidGeometry => {
                write!(f, "adaptive plan: frame and block size must be non-zero")
            }
            Self::TooManyBlocks { count, limit } => write!(
                f,
                "adaptive plan: {count} codec blocks exceeds the {limit} block limit"
            ),
            Self::ZeroArea { set, index } => {
                write!(f, "adaptive plan: {set} rectangle {index} has zero area")
            }
            Self::OutOfFrame { set, index } => {
                write!(
                    f,
                    "adaptive plan: {set} rectangle {index} is outside the frame"
                )
            }
            Self::CandidateOutsideDamage { set, block } => write!(
                f,
                "adaptive plan: {set} candidate block {},{} is not damaged",
                block.x, block.y
            ),
            Self::InvalidBlock { index } => write!(
                f,
                "adaptive plan: block {index} is out of frame, duplicated, or out of order"
            ),
        }
    }
}

/// Coalesce row-major, unique codec blocks into exact pixel rectangles.
pub fn blocks_to_regions(
    blocks: &[Block],
    frame_width: u32,
    frame_height: u32,
    block_size: u32,
) -> Result<Vec<Region>, PlanError> {
    if frame_width == 0 || frame_height == 0 || block_size == 0 {
        return Err(PlanError::InvalidGeometry);
    }
    let blocks_w = frame_width.div_ceil(block_size);
    let blocks_h = frame_height.div_ceil(block_size);
    let mut regions: Vec<Region> = Vec::new();
    let mut previous_runs = std::collections::BTreeMap::<(u32, u32), usize>::new();
    let mut previous_y = None;
    let mut index = 0;

    while index < blocks.len() {
        let row_y = blocks[index].y;
        if row_y >= blocks_h || previous_y.is_some_and(|y| row_y <= y) {
            return Err(PlanError::InvalidBlock { index });
        }
        if previous_y.is_some_and(|y| row_y != y + 1) {
            previous_runs.clear();
        }
        let mut current_runs = std::collections::BTreeMap::new();
        while index < blocks.len() && blocks[index].y == row_y {
            let start = blocks[index].x;
            if start >= blocks_w
                || (index > 0 && blocks[index - 1].y == row_y && blocks[index - 1].x >= start)
            {
                return Err(PlanError::InvalidBlock { index });
            }
            let mut end = start + 1;
            index += 1;
            while index < blocks.len() && blocks[index].y == row_y && blocks[index].x == end {
                if blocks[index].x >= blocks_w {
                    return Err(PlanError::InvalidBlock { index });
                }
                end += 1;
                index += 1;
            }

            let row_height = (frame_height - row_y * block_size).min(block_size);
            let region_index = if let Some(region_index) = previous_runs.remove(&(start, end)) {
                regions[region_index].height += row_height;
                region_index
            } else {
                let x = start * block_size;
                regions.push(Region {
                    x,
                    y: row_y * block_size,
                    width: (frame_width - x).min((end - start) * block_size),
                    height: row_height,
                });
                regions.len() - 1
            };
            current_runs.insert((start, end), region_index);
        }
        previous_runs = current_runs;
        previous_y = Some(row_y);
    }
    Ok(regions)
}

/// Intersect two global-coordinate regions without translating the result.
pub fn intersect(a: Region, b: Region) -> Option<Region> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = a.x.saturating_add(a.width).min(b.x.saturating_add(b.width));
    let bottom =
        a.y.saturating_add(a.height)
            .min(b.y.saturating_add(b.height));
    (right > left && bottom > top).then_some(Region {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

impl std::error::Error for PlanError {}

/// A hard allocation ceiling independent of hostile or corrupt geometry.
pub const MAX_PLAN_BLOCKS: u64 = 1_048_576;

/// Partition changed codec blocks once. Move ownership wins over raw ownership;
/// raw wins over video. Candidate coverage outside `changed` is rejected rather
/// than silently creating an update for pixels capture did not report as changed.
pub fn partition_blocks(
    frame_width: u32,
    frame_height: u32,
    block_size: u32,
    changed: &[Region],
    move_destinations: &[Region],
    raw_candidates: &[Region],
    force_full: bool,
) -> Result<UpdatePlan, PlanError> {
    if frame_width == 0 || frame_height == 0 || block_size == 0 {
        return Err(PlanError::InvalidGeometry);
    }

    let grid = Grid {
        frame_width,
        frame_height,
        block_size,
        blocks_w: frame_width.div_ceil(block_size),
    };
    let blocks_h = frame_height.div_ceil(block_size);
    let block_count = u64::from(grid.blocks_w) * u64::from(blocks_h);
    if block_count > MAX_PLAN_BLOCKS {
        return Err(PlanError::TooManyBlocks {
            count: block_count,
            limit: MAX_PLAN_BLOCKS,
        });
    }
    if force_full {
        return Ok(UpdatePlan::Full);
    }

    let mut ownership = vec![Owner::Unchanged; block_count as usize];
    mark_regions(
        &mut ownership,
        grid,
        changed,
        "changed",
        Owner::Video,
        false,
    )?;
    mark_regions(
        &mut ownership,
        grid,
        raw_candidates,
        "raw",
        Owner::Raw,
        true,
    )?;
    mark_regions(
        &mut ownership,
        grid,
        move_destinations,
        "move",
        Owner::Move,
        true,
    )?;

    let mut plan = IncrementalPlan::default();
    for (index, owner) in ownership.into_iter().enumerate() {
        let block = Block {
            x: index as u32 % grid.blocks_w,
            y: index as u32 / grid.blocks_w,
        };
        match owner {
            Owner::Unchanged => {}
            Owner::Move => plan.moves.push(block),
            Owner::Raw => plan.raw.push(block),
            Owner::Video => plan.video.push(block),
        }
    }
    Ok(UpdatePlan::Incremental(plan))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Unchanged,
    Move,
    Raw,
    Video,
}

#[derive(Clone, Copy)]
struct Grid {
    frame_width: u32,
    frame_height: u32,
    block_size: u32,
    blocks_w: u32,
}

fn mark_regions(
    ownership: &mut [Owner],
    grid: Grid,
    regions: &[Region],
    set: &'static str,
    owner: Owner,
    require_damage: bool,
) -> Result<(), PlanError> {
    for (index, region) in regions.iter().enumerate() {
        if region.width == 0 || region.height == 0 {
            return Err(PlanError::ZeroArea { set, index });
        }
        let Some(right) = region.x.checked_add(region.width) else {
            return Err(PlanError::OutOfFrame { set, index });
        };
        let Some(bottom) = region.y.checked_add(region.height) else {
            return Err(PlanError::OutOfFrame { set, index });
        };
        if right > grid.frame_width || bottom > grid.frame_height {
            return Err(PlanError::OutOfFrame { set, index });
        }

        let first_x = region.x / grid.block_size;
        let last_x = (right - 1) / grid.block_size;
        let first_y = region.y / grid.block_size;
        let last_y = (bottom - 1) / grid.block_size;
        for y in first_y..=last_y {
            for x in first_x..=last_x {
                let slot = &mut ownership[(y * grid.blocks_w + x) as usize];
                if require_damage && *slot == Owner::Unchanged {
                    return Err(PlanError::CandidateOutsideDamage {
                        set,
                        block: Block { x, y },
                    });
                }
                *slot = owner;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(index: u32) -> Region {
        Region {
            x: index % 4,
            y: index / 4,
            width: 1,
            height: 1,
        }
    }

    fn regions(mask: u16) -> Vec<Region> {
        (0..16)
            .filter(|bit| mask & (1 << bit) != 0)
            .map(cell)
            .collect()
    }

    fn mask(blocks: &[Block]) -> u16 {
        blocks
            .iter()
            .fold(0, |mask, block| mask | 1 << (block.y * 4 + block.x))
    }

    #[test]
    fn partitions_every_changed_block_exactly_once() {
        for changed_mask in 0u16..=u16::MAX {
            let move_mask = changed_mask & 0x1111;
            let raw_mask = changed_mask & 0x3333;
            let plan = partition_blocks(
                4,
                4,
                1,
                &regions(changed_mask),
                &regions(move_mask),
                &regions(raw_mask),
                false,
            )
            .unwrap();
            let UpdatePlan::Incremental(plan) = plan else {
                panic!("incremental request selected full fallback");
            };
            let moves = mask(&plan.moves);
            let raw = mask(&plan.raw);
            let video = mask(&plan.video);
            assert_eq!(moves & raw, 0);
            assert_eq!(moves & video, 0);
            assert_eq!(raw & video, 0);
            assert_eq!(moves | raw | video, changed_mask);
            assert_eq!((moves | raw | video) & !changed_mask, 0);
        }
    }

    #[test]
    fn move_then_raw_precedence_is_deterministic() {
        let plan = partition_blocks(
            4,
            1,
            1,
            &[Region {
                x: 0,
                y: 0,
                width: 4,
                height: 1,
            }],
            &[Region {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }],
            &[Region {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
            }],
            false,
        )
        .unwrap();
        assert_eq!(
            plan,
            UpdatePlan::Incremental(IncrementalPlan {
                moves: vec![Block { x: 0, y: 0 }],
                raw: vec![Block { x: 1, y: 0 }],
                video: vec![Block { x: 2, y: 0 }, Block { x: 3, y: 0 }],
            })
        );
    }

    #[test]
    fn full_fallback_is_exclusive() {
        let plan = partition_blocks(
            4,
            1,
            1,
            &[Region {
                x: 0,
                y: 0,
                width: 4,
                height: 1,
            }],
            &[Region {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }],
            &[Region {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
            }],
            true,
        )
        .unwrap();
        assert_eq!(plan, UpdatePlan::Full);
    }

    #[test]
    fn rejects_candidate_coverage_capture_did_not_mark_changed() {
        let error = partition_blocks(
            4,
            1,
            1,
            &[Region {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }],
            &[],
            &[Region {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
            }],
            false,
        )
        .unwrap_err();
        assert_eq!(
            error,
            PlanError::CandidateOutsideDamage {
                set: "raw",
                block: Block { x: 1, y: 0 },
            }
        );
    }

    #[test]
    fn validates_geometry_before_allocating() {
        assert_eq!(
            partition_blocks(0, 1, 1, &[], &[], &[], false),
            Err(PlanError::InvalidGeometry)
        );
        assert!(matches!(
            partition_blocks(u32::MAX, u32::MAX, 1, &[], &[], &[], false),
            Err(PlanError::TooManyBlocks { .. })
        ));
        assert!(matches!(
            partition_blocks(
                4,
                4,
                1,
                &[Region {
                    x: 3,
                    y: 3,
                    width: 2,
                    height: 1
                }],
                &[],
                &[],
                false,
            ),
            Err(PlanError::OutOfFrame {
                set: "changed",
                index: 0
            })
        ));
    }

    #[test]
    fn adjacent_blocks_coalesce_without_claiming_unchanged_pixels() {
        let regions = blocks_to_regions(
            &[
                Block { x: 0, y: 0 },
                Block { x: 1, y: 0 },
                Block { x: 0, y: 1 },
                Block { x: 1, y: 1 },
                Block { x: 3, y: 1 },
            ],
            64,
            32,
            16,
        )
        .unwrap();

        assert_eq!(
            regions,
            [
                Region {
                    x: 0,
                    y: 0,
                    width: 32,
                    height: 32
                },
                Region {
                    x: 48,
                    y: 16,
                    width: 16,
                    height: 16
                },
            ]
        );
    }

    #[test]
    fn partial_edge_blocks_stop_at_the_real_frame_edge() {
        assert_eq!(
            blocks_to_regions(&[Block { x: 3, y: 1 }], 63, 17, 16).unwrap(),
            [Region {
                x: 48,
                y: 16,
                width: 15,
                height: 1
            }]
        );
    }

    #[test]
    fn coalescing_refuses_an_out_of_frame_block_hidden_after_a_valid_run() {
        assert_eq!(
            blocks_to_regions(&[Block { x: 3, y: 0 }, Block { x: 4, y: 0 }], 63, 16, 16),
            Err(PlanError::InvalidBlock { index: 1 })
        );
    }

    #[test]
    fn global_coverage_intersects_a_tile_without_changing_coordinates() {
        let coverage = Region {
            x: 16,
            y: 0,
            width: 32,
            height: 16,
        };
        let tile = Region {
            x: 32,
            y: 0,
            width: 32,
            height: 32,
        };
        assert_eq!(
            intersect(coverage, tile),
            Some(Region {
                x: 32,
                y: 0,
                width: 16,
                height: 16
            })
        );
        assert_eq!(
            intersect(
                coverage,
                Region {
                    x: 48,
                    y: 0,
                    width: 16,
                    height: 32
                }
            ),
            None
        );
    }
}
