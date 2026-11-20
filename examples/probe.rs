//! Print all CoreAudio output devices and paired Bluetooth devices.
//! `cargo run --example probe`

#[cfg(target_os = "macos")]
fn main() {
    use duet::audio::bluetooth::MacBluetooth;
    use duet::audio::coreaudio::CoreAudio;
    use duet::audio::{AudioBackend, BluetoothBackend};

    println!("Output devices:");
    match CoreAudio.output_devices() {
        Ok(ds) => {
            for d in ds {
                let vol = d
                    .volume
                    .map(|v| format!("{:.0}%", v * 100.0))
                    .unwrap_or_else(|| "-".into());
                println!(
                    "  {}{:>4}  {:<10} vol {:>4}  {:?}  uid={}",
                    if d.is_default { "*" } else { " " },
                    d.id,
                    format!("{:?}", d.transport),
                    vol,
                    d.name,
                    d.uid
                );
                for s in &d.subdevice_uids {
                    println!("          member {s}");
                }
            }
        }
        Err(e) => println!("  error: {e}"),
    }

    println!(
        "\nBluetooth (authorization: {:?}):",
        MacBluetooth.authorization()
    );
    match MacBluetooth.paired_devices() {
        Ok(ds) => {
            for d in ds {
                println!(
                    "  {}  {:<9} {:<5}  {:?}",
                    d.address,
                    if d.connected { "connected" } else { "" },
                    if d.is_audio { "audio" } else { "" },
                    d.name
                );
                if d.is_audio {
                    match MacBluetooth.headphone_info(&d.address) {
                        Ok(Some(i)) => println!("      extras: {i:?}"),
                        Ok(None) => println!("      extras: none"),
                        Err(e) => println!("      extras error: {e}"),
                    }
                }
            }
        }
        Err(e) => println!("  error: {e}"),
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("probe is macOS-only");
}
