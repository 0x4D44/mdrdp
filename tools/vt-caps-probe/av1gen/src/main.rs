//! Scratch: emit tiny AV1 4:4:4 and 4:2:2 8-bit IVF streams with rav1e
//! (no 4:4:4-capable AV1 encoder is installed on this box).
use rav1e::prelude::*;
use std::io::Write;

const W: usize = 640;
const H: usize = 480;
const FRAMES: usize = 8;

fn ivf_header(frames: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(32);
    h.extend_from_slice(b"DKIF");
    h.extend_from_slice(&0u16.to_le_bytes());
    h.extend_from_slice(&32u16.to_le_bytes());
    h.extend_from_slice(b"AV01");
    h.extend_from_slice(&(W as u16).to_le_bytes());
    h.extend_from_slice(&(H as u16).to_le_bytes());
    h.extend_from_slice(&30u32.to_le_bytes()); // timebase den
    h.extend_from_slice(&1u32.to_le_bytes()); // timebase num
    h.extend_from_slice(&frames.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    h
}

fn encode(cs: ChromaSampling, path: &str) {
    let enc = EncoderConfig {
        width: W,
        height: H,
        bit_depth: 8,
        chroma_sampling: cs,
        min_key_frame_interval: 4,
        max_key_frame_interval: 4,
        low_latency: true,
        speed_settings: SpeedSettings::from_preset(10),
        ..Default::default()
    };
    let cfg = Config::new().with_encoder_config(enc).with_threads(4);
    let mut ctx: Context<u8> = cfg.new_context().expect("context");
    let (cw, ch) = match cs {
        ChromaSampling::Cs444 => (W, H),
        ChromaSampling::Cs422 => (W / 2, H),
        ChromaSampling::Cs420 => (W / 2, H / 2),
        ChromaSampling::Cs400 => (0, 0),
    };
    for i in 0..FRAMES {
        let mut f = ctx.new_frame();
        let mut y = vec![0u8; W * H];
        for r in 0..H {
            for c in 0..W {
                y[r * W + c] = (((c + i * 7) % 256) ^ ((r / 16 * 9) % 256)) as u8;
            }
        }
        f.planes[0].copy_from_raw_u8(&y, W, 1);
        let mut u = vec![0u8; cw * ch];
        let mut v = vec![0u8; cw * ch];
        for r in 0..ch {
            for c in 0..cw {
                u[r * cw + c] = ((r * 3 + i * 5) % 256) as u8;
                v[r * cw + c] = ((c * 5 + r + i * 3) % 256) as u8;
            }
        }
        f.planes[1].copy_from_raw_u8(&u, cw, 1);
        f.planes[2].copy_from_raw_u8(&v, cw, 1);
        ctx.send_frame(f).expect("send");
    }
    ctx.flush();
    let mut out = std::fs::File::create(path).expect("create");
    out.write_all(&ivf_header(FRAMES as u32)).unwrap();
    let mut n = 0u64;
    loop {
        match ctx.receive_packet() {
            Ok(pkt) => {
                out.write_all(&(pkt.data.len() as u32).to_le_bytes()).unwrap();
                out.write_all(&n.to_le_bytes()).unwrap();
                out.write_all(&pkt.data).unwrap();
                n += 1;
            }
            Err(EncoderStatus::Encoded) | Err(EncoderStatus::NeedMoreData) => continue,
            Err(EncoderStatus::LimitReached) => break,
            Err(e) => panic!("{e:?}"),
        }
    }
    println!("{path}: {n} frames");
}

fn main() {
    let dir = std::env::args().nth(1).expect("out dir");
    encode(ChromaSampling::Cs444, &format!("{dir}/av1_444_8.ivf"));
    encode(ChromaSampling::Cs422, &format!("{dir}/av1_422_8.ivf"));
}
