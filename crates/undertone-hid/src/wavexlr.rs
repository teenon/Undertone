//! Wave XLR device support.
//!
//! Talks to an Elgato Wave XLR (VID:PID `0x0FD9:0x007D`) over a
//! vendor-specific UAC memory-block protocol on a hidden audio entity
//! (ID `0x33`). See the project handoff `protocol.md` for the
//! reverse-engineering reference.
//!
//! The device is driven entirely through EP0 control transfers — we do
//! **not** claim the audio-control interface, because `snd_usb_audio`
//! owns it. The kernel allows user-space control transfers on EP0
//! without driver detach.
//!
//! Access requires a udev rule granting the user RW access to the
//! device node, e.g.:
//!
//! ```text
//! SUBSYSTEM=="usb", ATTR{idVendor}=="0fd9", ATTR{idProduct}=="007d", MODE="0660", TAG+="uaccess"
//! ```
//!
//! Without it, `detect()` succeeds but `into_handle()` returns
//! [`HidError::PermissionDenied`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusb::{DeviceHandle, GlobalContext};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::device::ELGATO_VID;
use crate::device_trait::{Device, DeviceEvent, DeviceModel, DeviceState, Rgb};
use crate::error::{HidError, HidResult};

/// Wave XLR USB Product ID on Elgato's VID `0x0FD9`.
pub const WAVE_XLR_PID: u16 = 0x007D;

// --- Control-transfer parameters (see protocol.md) -----------------------

const BM_REQUEST_TYPE_GET: u8 = 0xA1;
const BM_REQUEST_TYPE_SET: u8 = 0x22;
const B_REQUEST_GET_MEM: u8 = 0x85;
const B_REQUEST_SET_MEM: u8 = 0x05;
const W_VALUE: u16 = 0x0000;
const W_INDEX: u16 = 0x3300;
const STATE_BLOB_LEN: usize = 34;
const USB_TIMEOUT: Duration = Duration::from_millis(500);

// --- State-blob byte offsets (partial; see protocol.md) ------------------

const OFFSET_MIC_GAIN_LO: usize = 0;
const OFFSET_MIC_GAIN_HI: usize = 1;
const OFFSET_HEADER_TAG: usize = 3;
const HEADER_TAG_VALUE: u8 = 0xEC;
const OFFSET_MUTE_FLAG: usize = 4;
const OFFSET_LED_ZONES: usize = 16;
const LED_ZONE_COUNT: usize = 3;
const LED_ZONE_BYTES: usize = 3;

// Placeholder scale for normalized f32 gain. Captures show raw values
// in `0x2B80..=0x2F40` during a mid-range slider drag; the full span is
// unconfirmed. A 14-bit range is a conservative guess pending
// calibration against known ALSA setpoints (see protocol.md).
const GAIN_FULL_SCALE: u16 = 0x3FFF;

const EVENT_CHANNEL_CAPACITY: usize = 64;

// Compile-time sanity checks: every documented offset must fit in the
// fixed-length blob. A protocol change that breaks this fails the build
// rather than panicking at runtime.
const _: () = {
    assert!(OFFSET_LED_ZONES + LED_ZONE_COUNT * LED_ZONE_BYTES <= STATE_BLOB_LEN);
    assert!(OFFSET_MIC_GAIN_HI < STATE_BLOB_LEN);
    assert!(OFFSET_MUTE_FLAG < STATE_BLOB_LEN);
    assert!(OFFSET_HEADER_TAG < STATE_BLOB_LEN);
};

// --- Detection -----------------------------------------------------------

/// Detection result for a connected Wave XLR, before a control handle
/// is attached. Kept as a separate type so enumeration stays cheap and
/// side-effect free.
pub struct WaveXlrDevice {
    serial: String,
}

impl WaveXlrDevice {
    /// Attempt to detect a connected Wave XLR.
    ///
    /// Returns `Ok(None)` if no device is present. Errors only surface
    /// when USB enumeration itself fails.
    ///
    /// # Errors
    /// Returns `HidError::UsbError` if the descriptor read fails at an
    /// I/O level. Absence of the device is not an error.
    pub fn detect() -> HidResult<Option<Self>> {
        let Ok(devices) = rusb::devices() else {
            debug!("Failed to enumerate USB devices");
            return Ok(None);
        };

        for device in devices.iter() {
            let Ok(desc) = device.device_descriptor() else {
                continue;
            };
            if desc.vendor_id() != ELGATO_VID || desc.product_id() != WAVE_XLR_PID {
                continue;
            }

            let serial = Self::read_serial(&device).unwrap_or_else(|| "unknown".to_string());
            info!(
                serial = %serial,
                bus = device.bus_number(),
                address = device.address(),
                "Wave XLR detected via USB"
            );
            return Ok(Some(Self { serial }));
        }

        debug!("No Wave XLR device found");
        Ok(None)
    }

    fn read_serial<T: rusb::UsbContext>(device: &rusb::Device<T>) -> Option<String> {
        let desc = device.device_descriptor().ok()?;
        desc.serial_number_string_index()?;
        let handle = device.open().ok()?;
        handle.read_serial_number_string_ascii(&desc).ok()
    }

    #[must_use]
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// Promote a detected device into a cloneable [`Device`] handle by
    /// opening a USB control channel.
    ///
    /// # Errors
    /// Returns [`HidError::DeviceNotFound`] if the device has been
    /// unplugged between detect and open, [`HidError::PermissionDenied`]
    /// if the udev rule is missing, or [`HidError::UsbError`] for any
    /// other rusb failure.
    pub fn into_handle(self) -> HidResult<Arc<WaveXlrHandle>> {
        let handle = open_handle()?.ok_or(HidError::DeviceNotFound)?;
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Ok(Arc::new(WaveXlrHandle {
            serial: self.serial,
            usb: Mutex::new(handle),
            last_blob: Mutex::new(None),
            event_tx,
        }))
    }
}

fn open_handle() -> HidResult<Option<DeviceHandle<GlobalContext>>> {
    let devices = rusb::devices().map_err(|e| HidError::UsbError(e.to_string()))?;

    for device in devices.iter() {
        let Ok(desc) = device.device_descriptor() else {
            continue;
        };
        if desc.vendor_id() == ELGATO_VID && desc.product_id() == WAVE_XLR_PID {
            let handle = device.open().map_err(|e| match e {
                rusb::Error::Access => HidError::PermissionDenied,
                other => HidError::UsbError(other.to_string()),
            })?;
            return Ok(Some(handle));
        }
    }
    Ok(None)
}

// --- Handle --------------------------------------------------------------

/// Reference-counted handle to a Wave XLR. Implements [`Device`].
///
/// Operations use a read-modify-write pattern on the 34-byte state
/// blob: every `SET_MEM` starts from the most recently observed blob so
/// we never clobber bytes that haven't been reverse-engineered yet.
pub struct WaveXlrHandle {
    serial: String,
    usb: Mutex<DeviceHandle<GlobalContext>>,
    last_blob: Mutex<Option<[u8; STATE_BLOB_LEN]>>,
    event_tx: broadcast::Sender<DeviceEvent>,
}

impl WaveXlrHandle {
    fn emit(&self, event: DeviceEvent) {
        let _ = self.event_tx.send(event);
    }

    fn read_blob(&self) -> HidResult<[u8; STATE_BLOB_LEN]> {
        let mut buf = [0u8; STATE_BLOB_LEN];
        let n = {
            let usb = self.usb.lock().expect("wave xlr usb mutex poisoned");
            usb.read_control(
                BM_REQUEST_TYPE_GET,
                B_REQUEST_GET_MEM,
                W_VALUE,
                W_INDEX,
                &mut buf,
                USB_TIMEOUT,
            )
            .map_err(|e| HidError::UsbError(e.to_string()))?
        };

        if n != STATE_BLOB_LEN {
            return Err(HidError::ProtocolError(format!(
                "short GET_MEM read: got {n} of {STATE_BLOB_LEN} bytes"
            )));
        }
        if buf[OFFSET_HEADER_TAG] != HEADER_TAG_VALUE {
            warn!(
                got = format!("{:#04x}", buf[OFFSET_HEADER_TAG]),
                expected = format!("{HEADER_TAG_VALUE:#04x}"),
                "unexpected header tag in Wave XLR state blob"
            );
        }

        *self.last_blob.lock().expect("wave xlr blob mutex poisoned") = Some(buf);
        Ok(buf)
    }

    fn write_blob(&self, blob: &[u8; STATE_BLOB_LEN]) -> HidResult<()> {
        let n = {
            let usb = self.usb.lock().expect("wave xlr usb mutex poisoned");
            usb.write_control(
                BM_REQUEST_TYPE_SET,
                B_REQUEST_SET_MEM,
                W_VALUE,
                W_INDEX,
                blob,
                USB_TIMEOUT,
            )
            .map_err(|e| HidError::UsbError(e.to_string()))?
        };

        if n != STATE_BLOB_LEN {
            return Err(HidError::ProtocolError(format!(
                "short SET_MEM write: wrote {n} of {STATE_BLOB_LEN} bytes"
            )));
        }

        *self.last_blob.lock().expect("wave xlr blob mutex poisoned") = Some(*blob);
        Ok(())
    }

    /// Fetch-or-reuse the last observed blob as the base for a
    /// read-modify-write cycle.
    fn current_or_fetch_blob(&self) -> HidResult<[u8; STATE_BLOB_LEN]> {
        if let Some(blob) = *self.last_blob.lock().expect("wave xlr blob mutex poisoned") {
            return Ok(blob);
        }
        self.read_blob()
    }
}

impl Device for WaveXlrHandle {
    fn model(&self) -> DeviceModel {
        DeviceModel::WaveXlr
    }

    fn serial(&self) -> &str {
        &self.serial
    }

    fn get_state(&self) -> HidResult<DeviceState> {
        let blob = self.read_blob()?;
        Ok(parse_state(&blob))
    }

    fn set_mute(&self, muted: bool) -> HidResult<()> {
        let mut blob = self.current_or_fetch_blob()?;
        blob[OFFSET_MUTE_FLAG] = u8::from(muted);
        self.write_blob(&blob)?;
        self.emit(DeviceEvent::StateChanged(parse_state(&blob)));
        Ok(())
    }

    fn set_gain(&self, gain: f32) -> HidResult<()> {
        let mut blob = self.current_or_fetch_blob()?;
        let gain_u16 = gain_to_u16(gain);
        let [lo, hi] = gain_u16.to_le_bytes();
        blob[OFFSET_MIC_GAIN_LO] = lo;
        blob[OFFSET_MIC_GAIN_HI] = hi;
        self.write_blob(&blob)?;
        self.emit(DeviceEvent::StateChanged(parse_state(&blob)));
        Ok(())
    }

    fn set_led(&self, zones: &[Rgb]) -> HidResult<()> {
        let mut blob = self.current_or_fetch_blob()?;
        // Missing zones repeat the last supplied colour; no zones at
        // all leaves the blob unchanged.
        let Some(&fallback) = zones.last() else {
            return Ok(());
        };
        for i in 0..LED_ZONE_COUNT {
            let color = zones.get(i).copied().unwrap_or(fallback);
            let off = OFFSET_LED_ZONES + i * LED_ZONE_BYTES;
            // Byte order unconfirmed (see protocol.md open item).
            // RGB is the placeholder; revisit once a pure-red capture
            // disambiguates against BGR/GRB.
            blob[off] = color.r;
            blob[off + 1] = color.g;
            blob[off + 2] = color.b;
        }
        self.write_blob(&blob)?;
        self.emit(DeviceEvent::StateChanged(parse_state(&blob)));
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.event_tx.subscribe()
    }
}

// --- Parsing helpers -----------------------------------------------------

fn parse_state(blob: &[u8; STATE_BLOB_LEN]) -> DeviceState {
    let gain_u16 = u16::from_le_bytes([blob[OFFSET_MIC_GAIN_LO], blob[OFFSET_MIC_GAIN_HI]]);
    DeviceState {
        mic_muted: blob[OFFSET_MUTE_FLAG] != 0,
        mic_gain: gain_from_u16(gain_u16),
        headphone_volume: 0.0,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn gain_to_u16(gain: f32) -> u16 {
    let clamped = gain.clamp(0.0, 1.0);
    let scaled = (f32::from(GAIN_FULL_SCALE) * clamped).round();
    // clamped ∈ [0, 1], scaled ∈ [0, GAIN_FULL_SCALE]; the cast is safe.
    (scaled as u16).min(GAIN_FULL_SCALE)
}

fn gain_from_u16(raw: u16) -> f32 {
    let clamped = raw.min(GAIN_FULL_SCALE);
    f32::from(clamped) / f32::from(GAIN_FULL_SCALE)
}

// --- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Frame 9761 from `06-gain-change.pcapng`: gain=0x2C3F, mute=0, LED=00 00 FF × 3
    const SAMPLE_UNMUTED: [u8; STATE_BLOB_LEN] = [
        0x3f, 0x2c, 0x00, 0xec, 0x00, 0x01, 0x00, 0x00, 0x00, 0xcd, 0xe9, 0x00, 0x00, 0x00, 0x02,
        0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0xff, 0x00, 0x00, 0xff, 0x00, 0x01, 0x00, 0xff,
        0x37, 0x00, 0x01, 0x01,
    ];

    // Frame 4415 from `08-mute-toggle.pcapng`: gain=0x2B80, mute=1, LED=00 AA FF × 3
    const SAMPLE_MUTED: [u8; STATE_BLOB_LEN] = [
        0x80, 0x2b, 0x00, 0xec, 0x01, 0x01, 0x00, 0x00, 0x00, 0xcd, 0xe9, 0x00, 0x00, 0x00, 0x02,
        0xff, 0x00, 0x00, 0xaa, 0xff, 0x00, 0xaa, 0xff, 0x00, 0xaa, 0xff, 0x00, 0x01, 0x00, 0xff,
        0x37, 0x00, 0x01, 0x01,
    ];

    #[test]
    fn parse_unmuted_sample() {
        let state = parse_state(&SAMPLE_UNMUTED);
        assert!(!state.mic_muted);
        let expected = gain_from_u16(0x2C3F);
        assert!((state.mic_gain - expected).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_muted_sample() {
        let state = parse_state(&SAMPLE_MUTED);
        assert!(state.mic_muted);
    }

    #[test]
    fn gain_round_trip_is_stable() {
        for &raw in &[0x0000_u16, 0x0001, 0x1FFF, 0x2C3F, 0x3FFE, 0x3FFF] {
            let roundtripped = gain_to_u16(gain_from_u16(raw));
            assert_eq!(raw.min(GAIN_FULL_SCALE), roundtripped);
        }
    }

    #[test]
    fn gain_clamps_out_of_range() {
        assert_eq!(gain_to_u16(-0.5), 0);
        assert_eq!(gain_to_u16(0.0), 0);
        assert_eq!(gain_to_u16(1.0), GAIN_FULL_SCALE);
        assert_eq!(gain_to_u16(5.0), GAIN_FULL_SCALE);
    }

    #[test]
    fn device_model_pid_matches_const() {
        assert_eq!(DeviceModel::WaveXlr.usb_pid(), WAVE_XLR_PID);
    }

    #[test]
    fn header_tag_matches_samples() {
        assert_eq!(SAMPLE_UNMUTED[OFFSET_HEADER_TAG], HEADER_TAG_VALUE);
        assert_eq!(SAMPLE_MUTED[OFFSET_HEADER_TAG], HEADER_TAG_VALUE);
    }
}
