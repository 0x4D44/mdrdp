mod stats {
    #[derive(Debug, Default, Clone, Copy)]
    pub struct CacheStats {
        pub hits: u64,
        pub misses: u64,
        pub entries: u64,
        pub evictions: u64,
        pub bytes_served: u64,
        pub bytes_stored: u64,
        pub bytes_from_wire: u64,
    }
}
mod surface;

use surface::{Rect, SurfaceStore, BPP};

const RED: [u8; 4] = [255, 0, 0, 255];

fn solid(w: u16, h: u16, rgba: [u8; 4]) -> Vec<u8> {
    rgba.iter()
        .copied()
        .cycle()
        .take(w as usize * h as usize * BPP)
        .collect()
}

fn main() {
    // Scenario 1 from the claim: 4x4 source, overhanging src rect.
    let mut store = SurfaceStore::new();
    store.create(1, 4, 4);
    store.create(2, 4, 4);
    store
        .blit_rgba(1, Rect::new(0, 0, 4, 4), &solid(4, 4, RED), 4)
        .unwrap();
    let g0 = store.generation();
    let r = store.surface_to_surface(1, Rect::new(2, 2, 6, 6), 2, &[(0, 0)]);
    let dest = store.get(2).unwrap();
    let painted = dest.pixels().iter().any(|&b| b != 0);
    println!("S1 result={:?} painted={} gen_moved={}", r, painted, store.generation() != g0);

    // Scenario 2: partial write then error, generation not bumped.
    let mut store = SurfaceStore::new();
    store.create(1, 4, 4);
    store.create(2, 4, 4);
    store
        .blit_rgba(1, Rect::new(0, 0, 4, 4), &solid(4, 4, RED), 4)
        .unwrap();
    let g0 = store.generation();
    let r = store.surface_to_surface(1, Rect::new(2, 2, 6, 6), 2, &[(3, 3), (0, 0)]);
    let dest = store.get(2).unwrap();
    let painted = dest.pixels().iter().any(|&b| b != 0);
    println!("S2 result={:?} painted={} gen_moved={}", r, painted, store.generation() != g0);

    // Control: in-bounds source rect, many destination points incl. overhang.
    let mut store = SurfaceStore::new();
    store.create(1, 4, 4);
    store.create(2, 4, 4);
    store
        .blit_rgba(1, Rect::new(0, 0, 4, 4), &solid(4, 4, RED), 4)
        .unwrap();
    let g0 = store.generation();
    let r = store.surface_to_surface(1, Rect::new(0, 0, 4, 4), 2, &[(3, 3), (0, 0), (2, 1)]);
    println!("S3 (in-bounds src) result={:?} gen_moved={}", r, store.generation() != g0);

    // Exhaustive: any in-bounds src rect + any dest point can never error.
    let mut worst = None;
    for w in 1..=6u16 {
        for h in 1..=6u16 {
            for l in 0..w {
                for t in 0..h {
                    for rgt in (l + 1)..=w {
                        for bot in (t + 1)..=h {
                            for dx in 0..=8u16 {
                                for dy in 0..=8u16 {
                                    let mut st = SurfaceStore::new();
                                    st.create(1, w, h);
                                    st.create(2, 5, 5);
                                    let g = st.generation();
                                    let res = st.surface_to_surface(
                                        1,
                                        Rect::new(l, t, rgt, bot),
                                        2,
                                        &[(dx, dy)],
                                    );
                                    if res.is_err() || st.generation() == g {
                                        worst = Some((w, h, l, t, rgt, bot, dx, dy, res));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    println!("exhaustive in-bounds failure = {:?}", worst);
}
