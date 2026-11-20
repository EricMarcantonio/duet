//! Try noise control directly on connected AirPods (macOS):
//!   cargo run --example noise                 # list connected headphones + current mode
//!   cargo run --example noise -- <ADDR> <off|transparency|adaptive|anc>
//! Sets the mode from the main thread, spins the main run loop, then reads it back.
#[cfg(target_os = "macos")]
fn main() {
    use duet::audio::{BluetoothBackend, ListeningMode, bluetooth::MacBluetooth};
    let bt = MacBluetooth;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let show = |addr: &str| match bt.headphone_info(addr) {
        Ok(Some(i)) => println!("  {addr}: mode {:?}, supports {:?}, battery {:?}", i.listening_mode, i.supported_modes, i.battery),
        Ok(None) => println!("  {addr}: no extras"),
        Err(e) => println!("  {addr}: error {e}"),
    };
    if args.len() < 2 {
        for d in bt.paired_devices().unwrap_or_default().into_iter().filter(|d| d.connected && d.is_audio) {
            println!("{} ({})", d.name, d.address);
            show(&d.address);
        }
        return;
    }
    let mode = match args[1].as_str() {
        "off" => ListeningMode::Off,
        "transparency" => ListeningMode::Transparency,
        "adaptive" => ListeningMode::Adaptive,
        _ => ListeningMode::NoiseCancellation,
    };
    print!("before:");
    show(&args[0]);
    println!("set {mode:?}: {:?}", bt.set_listening_mode(&args[0], mode));
    // Let IOBluetooth deliver the request: spin the main run loop for 2 s.
    unsafe extern "C" {
        fn CFRunLoopRunInMode(mode: *const std::ffi::c_void, seconds: f64, return_after: u8) -> i32;
        static kCFRunLoopDefaultMode: *const std::ffi::c_void;
    }
    // SAFETY: CoreFoundation call on the main thread with a valid mode constant.
    unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 2.0, 0) };
    print!("after: ");
    show(&args[0]);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macOS only");
}
