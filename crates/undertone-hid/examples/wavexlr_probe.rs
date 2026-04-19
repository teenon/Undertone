//! Wave XLR live protocol probe.
//!
//! Polls the Wave XLR's 34-byte state blob at ~5 Hz and prints every
//! byte that changes between polls. Use it to identify bytes that
//! respond to physical inputs — knob rotation, tag-button press — so
//! they can be mapped into `wavexlr.rs`.
//!
//!     cargo run --example wavexlr_probe -p undertone-hid
//!
//! Stop with Ctrl-C. Each change line also prints the cumulative set
//! of offsets that have moved so far, so the run summary is always
//! visible on the last printed line.
//!
//! Requires the udev rule for `0fd9:007d` to be installed so the user
//! can open the device without root. Without it, you'll see a
//! `PermissionDenied` error on startup.

use std::collections::BTreeSet;
use std::io::Write;
use std::time::{Duration, Instant};

use undertone_hid::{STATE_BLOB_LEN, WaveXlrDevice};

const POLL_INTERVAL: Duration = Duration::from_millis(200);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let device = WaveXlrDevice::detect()?
        .ok_or("No Wave XLR found — check `lsusb | grep 0fd9:007d`")?;
    println!("Detected Wave XLR, serial = {}", device.serial());

    let handle = device.into_handle()?;
    println!(
        "Opened USB control channel on interface 3. Audio routing on \
         interfaces 0–2 is unaffected."
    );
    println!(
        "Polling every {}ms. Stop with Ctrl-C.\n",
        POLL_INTERVAL.as_millis()
    );
    println!("Twist the knob, press the tag button, toggle mute in another tool.");
    println!("Bytes that move are ones we still need to decode.\n");
    println!(
        "Legend: 00,01 = gain_lo/hi  |  03 = header_tag  |  04 = mute_flag  |  \
         09 = knob_fine  |  10 = knob_delta  |  16..=24 = led_zones  |  \
         others = opaque/unknown\n"
    );

    let start = Instant::now();
    let mut prev: Option<[u8; STATE_BLOB_LEN]> = None;
    let mut ever_changed: BTreeSet<usize> = BTreeSet::new();

    loop {
        let blob = handle.read_raw_state()?;
        let elapsed = start.elapsed().as_secs_f32();

        if let Some(p) = prev {
            let diffs: Vec<usize> = (0..STATE_BLOB_LEN).filter(|&i| p[i] != blob[i]).collect();
            if !diffs.is_empty() {
                let mut line = format!("[t={elapsed:>6.2}s]");
                for &off in &diffs {
                    ever_changed.insert(off);
                    line.push_str(&format!(
                        "  byte[{off:02}] {:#04x}→{:#04x} ({})",
                        p[off],
                        blob[off],
                        offset_label(off)
                    ));
                }
                line.push_str(&format!(
                    "    | ever-changed: {}",
                    ever_changed
                        .iter()
                        .map(|o| format!("{o:02}"))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
                println!("{line}");
                let _ = std::io::stdout().flush();
            }
        } else {
            println!("[t={elapsed:>6.2}s] baseline: {}", hex_row(&blob));
        }

        prev = Some(blob);
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn hex_row(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

fn offset_label(off: usize) -> &'static str {
    match off {
        0 => "gain_lo",
        1 => "gain_hi",
        3 => "header_tag",
        4 => "mute_flag",
        9 => "knob_fine",
        10 => "knob_delta",
        16..=24 => "led_zone",
        _ => "unknown",
    }
}
