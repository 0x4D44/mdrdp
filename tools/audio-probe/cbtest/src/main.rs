use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

fn main() {
    let host = cpal::default_host();
    let dev = host
        .default_output_device()
        .expect("no default output device");
    let def = dev.default_output_config().unwrap();
    println!("default config: {def:?}");
    let cfg = def.config();
    println!("stream config: {cfg:?}");

    let calls = Arc::new(AtomicU64::new(0));
    let gaps = Arc::new(AtomicU64::new(0));
    let frames = Arc::new(AtomicUsize::new(0));
    let (c, g, f) = (calls.clone(), gaps.clone(), frames.clone());

    // Empty ring: every slot is silence, mirroring AudioRing::pop_into with nothing queued.
    let stream = dev
        .build_output_stream(
            &cfg,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                c.fetch_add(1, Ordering::Relaxed);
                f.store(out.len(), Ordering::Relaxed);
                let mut had_gap = false;
                for s in out.iter_mut() {
                    *s = 0.0;
                    had_gap = true;
                }
                if had_gap {
                    g.fetch_add(1, Ordering::Relaxed);
                }
            },
            move |e| eprintln!("err: {e}"),
            None,
        )
        .expect("build_output_stream");

    let before_play = calls.load(Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let after_300ms_no_play = calls.load(Ordering::Relaxed);
    stream.play().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(5));
    let n = calls.load(Ordering::Relaxed);
    println!("calls immediately after build: {before_play}");
    println!("calls after 300ms WITHOUT calling play(): {after_300ms_no_play}");
    println!(
        "calls after a further 5s: {n}  -> {:.1}/sec",
        (n - after_300ms_no_play) as f64 / 5.0
    );
    println!(
        "underrun events counted over 5.3s idle: {}",
        gaps.load(Ordering::Relaxed)
    );
    println!("samples per callback: {}", frames.load(Ordering::Relaxed));
    println!(
        "extrapolated to a 10 min idle session: {:.0}",
        (n as f64 / 5.3) * 600.0
    );
}
