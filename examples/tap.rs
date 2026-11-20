//! Live check of the macOS system-audio mirror:
//!   cargo run --example tap -- <target output UID> [seconds]
//! Prints frames copied and the peak level once a second.
#[cfg(target_os = "macos")]
fn main() {
    use duet::audio::tap_mirror::{Mirror, supported};
    let args: Vec<String> = std::env::args().skip(1).collect();
    let target = args.first().cloned().expect("target output UID");
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);
    println!("taps supported: {}", supported());
    let m = match Mirror::start(&[target]) {
        Ok(m) => m,
        Err(e) => return println!("start failed: {e}"),
    };
    println!("mirroring to {:?}", m.targets());
    for _ in 0..secs {
        std::thread::sleep(std::time::Duration::from_secs(1));
        for ((t, frames, peak), (calls, inb)) in m.stats().into_iter().zip(m.debug_counts()) {
            println!(
                "  {}: {calls} callbacks, {inb} input buffers, {frames} frames, peak {peak:.3}",
                &t[..t.len().min(24)]
            );
        }
    }
    drop(m);
    println!("stopped");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
