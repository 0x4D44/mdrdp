//! Decode captured RFX Progressive payloads offline and print row-0 tile means.
//!
//! Exists so the decoder can be compared against FreeRDP's own output for the SAME bytes
//! without a network, a server, or a display. FreeRDP's numbers come from a small C
//! harness linked against libfreerdp3; this prints ours in the same form.
//!
//!     progcmp <frame00.bin> <frame01.bin> ...
//!
//! Each file is the raw `bitmap_data` of one RDPGFX_WIRE_TO_SURFACE_PDU_2.

use ironrdp_graphics::progressive::ProgressiveDecoder;

const W: usize = 1920;
const H: usize = 1080;
const TILE: usize = 64;

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: progcmp <frame.bin> ...");
        std::process::exit(2);
    }

    let mut surface = vec![0u8; W * H * 4]; // RGBA
    let mut decoder = ProgressiveDecoder::new();

    for path in &files {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  {path} -> {e}");
                continue;
            }
        };
        match decoder.decode_bitmap(1, W as u16, H as u16, &data) {
            Ok(tiles) => {
                println!("  {path} -> {} tiles", tiles.len());
                for t in tiles {
                    let x0 = usize::from(t.x_idx) * TILE;
                    let y0 = usize::from(t.y_idx) * TILE;
                    for row in 0..TILE {
                        let y = y0 + row;
                        if y >= H {
                            break;
                        }
                        for col in 0..TILE {
                            let x = x0 + col;
                            if x >= W {
                                break;
                            }
                            let src = (row * TILE + col) * 4;
                            let dst = (y * W + x) * 4;
                            surface[dst..dst + 4].copy_from_slice(&t.pixels[src..src + 4]);
                        }
                    }
                }
            }
            Err(e) => println!("  {path} -> ERROR {e}"),
        }
    }

    println!("ours row-0 tile means (R,G,B):");
    for tx in 0..(W / TILE) {
        let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
        let mut y = 4;
        while y < TILE {
            let mut x = tx * TILE + 4;
            while x < tx * TILE + TILE {
                let p = (y * W + x) * 4;
                r += u64::from(surface[p]);
                g += u64::from(surface[p + 1]);
                b += u64::from(surface[p + 2]);
                n += 1;
                x += 6;
            }
            y += 6;
        }
        println!("   tile {tx:2} rgb({}, {}, {})", r / n, g / n, b / n);
    }
}
