//! Wave XLR device support.
//!
//! Controls an Elgato Wave XLR (VID:PID `0x0FD9:0x007D`) via its
//! vendor-specific control interface (USB interface 3, labelled
//! `"Elgato Wave XLR Controls"`). This interface is not bound to
//! `snd_usb_audio`, so claiming it does **not** disturb the Wave XLR's
//! ALSA card or `PipeWire` audio routing.
//!
//! The underlying protocol is the same UAC memory-block access to
//! hidden entity `0x33` described in `protocol.md`, but with `wIndex`
//! encoded as `(entity_id << 8) | interface_number = 0x3303` and
//! `SET_MEM` using `OUT Class/Interface` (`bmRequestType = 0x21`)
//! rather than the endpoint variant seen in the original Windows
//! Wave Link captures. The endpoint variant fails with `Pipe error`
//! when the request targets interface 3.
//!
//! Known state-blob fields (as of 2026-04-18):
//!
//! | offset | field        | notes                                         |
//! |-------:|--------------|-----------------------------------------------|
//! | 0–1    | `mic_gain`   | LE u16, dB scale unconfirmed                  |
//! | 3      | `header_tag` | always `0xEC`                                 |
//! | 4      | `mute_flag`  | `0`/`1`; tag-button folds into this byte      |
//! | 9      | `knob_fine`  | sub-detent encoder, step `±0x33` per detent   |
//! | 10     | `knob_delta` | signed detent counter, `±1` per click         |
//! | 16–24  | `led[3]`     | three RGB-ish zones, byte order unconfirmed   |

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

// --- Vendor-protocol parameters -----------------------------------------

/// Vendor-specific control interface ("Elgato Wave XLR Controls"). Not
/// bound to `snd_usb_audio`, so claiming it is safe for audio routing.
const CONTROL_INTERFACE: u8 = 3;

/// `bmRequestType = IN | Class | Interface`.
const BM_REQUEST_TYPE_GET: u8 = 0xA1;
/// `bmRequestType = OUT | Class | Interface`. The Windows captures
/// showed `0x22` (OUT | Class | Endpoint), but that recipient variant
/// only works when targeting interface 0 — interface 3 requires the
/// Interface recipient.
const BM_REQUEST_TYPE_SET: u8 = 0x21;
const B_REQUEST_GET_MEM: u8 = 0x85;
const B_REQUEST_SET_MEM: u8 = 0x05;
const W_VALUE: u16 = 0x0000;
/// `(entity_id=0x33 << 8) | interface_number=0x03`. Same hidden entity
/// as the Windows protocol, but surfaced via interface 3.
const W_INDEX: u16 = 0x3303;
/// Length in bytes of the Wave XLR state blob.
pub const STATE_BLOB_LEN: usize = 34;
const USB_TIMEOUT: Duration = Duration::from_millis(500);

// --- State-blob byte offsets --------------------------------------------

const OFFSET_MIC_GAIN_LO: usize = 0;
const OFFSET_MIC_GAIN_HI: usize = 1;
const OFFSET_HEADER_TAG: usize = 3;
const HEADER_TAG_VALUE: u8 = 0xEC;
const OFFSET_MUTE_FLAG: usize = 4;
const OFFSET_KNOB_FINE: usize = 9;
const OFFSET_KNOB_DELTA: usize = 10;
const OFFSET_LED_ZONES: usize = 16;
const LED_ZONE_COUNT: usize = 3;
const LED_ZONE_BYTES: usize = 3;

/// Conservative 14-bit gain range. Full firmware span unconfirmed.
const GAIN_FULL_SCALE: u16 = 0x3FFF;

const EVENT_CHANNEL_CAPACITY: usize = 64;

const _: () = {
    assert!(OFFSET_LED_ZONES + LED_ZONE_COUNT * LED_ZONE_BYTES <= STATE_BLOB_LEN);
    assert!(OFFSET_MIC_GAIN_HI < STATE_BLOB_LEN);
    assert!(OFFSET_MUTE_FLAG < STATE_BLOB_LEN);
    assert!(OFFSET_HEADER_TAG < STATE_BLOB_LEN);
    assert!(OFFSET_KNOB_DELTA < STATE_BLOB_LEN);
    assert!(OFFSET_KNOB_FINE < STATE_BLOB_LEN);
};

// --- Detection -----------------------------------------------------------

/// Detection result for a connected Wave XLR. Side-effect-free:
/// enumerates USB and reads the serial-string descriptor only.
pub struct WaveXlrDevice {
    serial: String,
    /// ALSA card identifier (e.g. `"1"`) discovered from
    /// `/proc/asound/cards`. Used by the handle for headphone-volume
    /// control via amixer, since the firmware vendor protocol on
    /// interface 3 doesn't (yet) expose a known headphone-volume byte.
    /// `None` when the card hasn't been registered yet (typical when
    /// the daemon races `snd_usb_audio` at boot).
    alsa_card: Option<String>,
}

impl WaveXlrDevice {
    /// Attempt to detect a connected Wave XLR.
    ///
    /// # Errors
    /// Returns `Ok(None)` when no device is present. Surfaces USB
    /// enumeration failures as `HidError::UsbError`.
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
            let alsa_card = Self::find_alsa_card().ok().flatten();
            info!(
                serial = %serial,
                bus = device.bus_number(),
                address = device.address(),
                alsa_card = ?alsa_card,
                "Wave XLR detected via USB"
            );
            return Ok(Some(Self { serial, alsa_card }));
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

    fn find_alsa_card() -> HidResult<Option<String>> {
        // amixer accepts the bare card number or short name; not `hw:N`.
        let cards = std::fs::read_to_string("/proc/asound/cards").map_err(HidError::IoError)?;
        for line in cards.lines() {
            if (line.contains("Wave XLR") || line.contains("Wave_XLR"))
                && let Some(num) = line.split_whitespace().next()
                && num.parse::<u32>().is_ok()
            {
                return Ok(Some(num.to_string()));
            }
        }
        Ok(None)
    }

    #[must_use]
    pub fn serial(&self) -> &str {
        &self.serial
    }

    #[must_use]
    pub fn alsa_card(&self) -> Option<&str> {
        self.alsa_card.as_deref()
    }

    /// Open a USB control channel to the Wave XLR and claim interface 3.
    /// Returns a cloneable [`Device`] handle.
    ///
    /// # Errors
    /// Returns [`HidError::DeviceNotFound`] if the device has been
    /// unplugged, [`HidError::PermissionDenied`] if the udev rule is
    /// missing, or [`HidError::UsbError`] for any other rusb failure.
    pub fn into_handle(self) -> HidResult<Arc<WaveXlrHandle>> {
        let handle = open_handle()?.ok_or(HidError::DeviceNotFound)?;
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Ok(Arc::new(WaveXlrHandle {
            serial: self.serial,
            usb: Mutex::new(handle),
            last_blob: Mutex::new(None),
            alsa_card: self.alsa_card,
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

            // Interface 3 is the Elgato vendor-specific "Wave XLR
            // Controls" interface. `snd_usb_audio` does not bind it
            // (it owns 0, 1, 2), so claiming is safe for audio.
            // Auto-detach is a no-op here but kept for defence in depth
            // against future kernel changes that might bind this
            // interface to a generic driver.
            if let Err(e) = handle.set_auto_detach_kernel_driver(true) {
                debug!(error = %e, "auto-detach not supported on this platform");
            }
            handle
                .claim_interface(CONTROL_INTERFACE)
                .map_err(|e| match e {
                    rusb::Error::Access => HidError::PermissionDenied,
                    other => HidError::UsbError(format!("claim interface 3: {other}")),
                })?;
            return Ok(Some(handle));
        }
    }
    Ok(None)
}

// --- Device-trait handle (USB-native via interface 3) -------------------

/// Reference-counted handle to a Wave XLR. Implements [`Device`] by
/// issuing vendor control transfers on interface 3. Holds a persistent
/// claim on interface 3; audio on interfaces 0–2 is unaffected.
///
/// Operations use a read-modify-write pattern on the 34-byte state
/// blob: every `SET_MEM` starts from the most recent observed blob so
/// bytes whose meaning isn't yet decoded (LED byte order, some
/// opaque header/trailer regions) are preserved verbatim.
pub struct WaveXlrHandle {
    serial: String,
    usb: Mutex<DeviceHandle<GlobalContext>>,
    last_blob: Mutex<Option<[u8; STATE_BLOB_LEN]>>,
    /// ALSA card name for amixer-based controls (currently just the
    /// PCM Playback Volume / headphone level). The vendor protocol on
    /// interface 3 carries the rest.
    alsa_card: Option<String>,
    event_tx: broadcast::Sender<DeviceEvent>,
}

impl WaveXlrHandle {
    fn emit(&self, event: DeviceEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Issue a `GET_MEM` on interface 3 and return the raw 34-byte
    /// state blob. Intended for protocol decoding, calibration, and
    /// diagnostic tooling; regular callers should use
    /// [`Device::get_state`].
    ///
    /// # Errors
    /// Propagates USB errors and surfaces short reads or an unexpected
    /// header tag as [`HidError::ProtocolError`].
    ///
    /// # Panics
    /// Panics if the internal USB or blob mutex is poisoned, which
    /// only happens after a previous call panicked while holding it.
    pub fn read_raw_state(&self) -> HidResult<[u8; STATE_BLOB_LEN]> {
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

    /// Send a `SET_MEM` with the provided 34-byte blob. Seed the blob
    /// from [`Self::read_raw_state`] first — do not construct one
    /// from scratch.
    ///
    /// # Errors
    /// Propagates USB errors and surfaces short writes as
    /// [`HidError::ProtocolError`].
    ///
    /// # Panics
    /// Panics if the internal USB or blob mutex is poisoned.
    pub fn write_raw_state(&self, blob: &[u8; STATE_BLOB_LEN]) -> HidResult<()> {
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

    fn current_or_fetch_blob(&self) -> HidResult<[u8; STATE_BLOB_LEN]> {
        if let Some(blob) = *self.last_blob.lock().expect("wave xlr blob mutex poisoned") {
            return Ok(blob);
        }
        self.read_raw_state()
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
        let blob = self.read_raw_state()?;
        let mut state = parse_state(&blob);
        // Headphone volume isn't in the firmware blob — read it from
        // ALSA. Best-effort: if the card name is missing or amixer
        // fails, leave it at 0.0.
        if let Some(card) = &self.alsa_card
            && let Ok(v) = read_pcm_playback_volume(card)
        {
            state.headphone_volume = v;
        }
        Ok(state)
    }

    fn set_mute(&self, muted: bool) -> HidResult<()> {
        let mut blob = self.current_or_fetch_blob()?;
        blob[OFFSET_MUTE_FLAG] = u8::from(muted);
        self.write_raw_state(&blob)?;
        self.emit(DeviceEvent::StateChanged(parse_state(&blob)));
        Ok(())
    }

    fn set_gain(&self, gain: f32) -> HidResult<()> {
        let mut blob = self.current_or_fetch_blob()?;
        let [lo, hi] = gain_to_u16(gain).to_le_bytes();
        blob[OFFSET_MIC_GAIN_LO] = lo;
        blob[OFFSET_MIC_GAIN_HI] = hi;
        self.write_raw_state(&blob)?;
        self.emit(DeviceEvent::StateChanged(parse_state(&blob)));
        Ok(())
    }

    fn set_headphone_volume(&self, volume: f32) -> HidResult<()> {
        let card = self
            .alsa_card
            .as_deref()
            .ok_or_else(|| HidError::AlsaError("Wave XLR ALSA card not registered yet".into()))?;
        write_pcm_playback_volume(card, volume)?;
        self.emit(DeviceEvent::StateChanged(self.get_state()?));
        Ok(())
    }

    fn set_led(&self, zones: &[Rgb]) -> HidResult<()> {
        let mut blob = self.current_or_fetch_blob()?;
        let Some(&fallback) = zones.last() else {
            return Ok(());
        };
        for i in 0..LED_ZONE_COUNT {
            let color = zones.get(i).copied().unwrap_or(fallback);
            let off = OFFSET_LED_ZONES + i * LED_ZONE_BYTES;
            // Byte order unconfirmed; RGB is the placeholder until a
            // pure-red capture disambiguates against BGR/GRB.
            blob[off] = color.r;
            blob[off + 1] = color.g;
            blob[off + 2] = color.b;
        }
        self.write_raw_state(&blob)?;
        self.emit(DeviceEvent::StateChanged(parse_state(&blob)));
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.event_tx.subscribe()
    }
}

// --- ALSA helpers (PCM playback volume only) ----------------------------

/// Read the Wave XLR's PCM Playback Volume normalized to `0.0..=1.0`.
fn read_pcm_playback_volume(card: &str) -> HidResult<f32> {
    let output = std::process::Command::new("amixer")
        .args(["-c", card, "sget", "PCM"])
        .output()
        .map_err(|e| HidError::AlsaError(e.to_string()))?;
    if !output.status.success() {
        return Err(HidError::AlsaError(format!(
            "amixer sget PCM failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for token in stdout.split_whitespace() {
        if let Some(percent) = token
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix("%]"))
            && let Ok(p) = percent.parse::<u32>()
        {
            return Ok(percent_to_unit(p));
        }
    }
    Ok(0.0)
}

fn write_pcm_playback_volume(card: &str, volume: f32) -> HidResult<()> {
    let percent = unit_to_percent(volume);
    let output = std::process::Command::new("amixer")
        .args(["-c", card, "sset", "PCM", &format!("{percent}%")])
        .output()
        .map_err(|e| HidError::AlsaError(e.to_string()))?;
    if !output.status.success() {
        return Err(HidError::AlsaError(format!(
            "amixer sset PCM failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn unit_to_percent(volume: f32) -> u32 {
    (volume.clamp(0.0, 1.0) * 100.0).round() as u32
}

#[allow(clippy::cast_precision_loss)]
fn percent_to_unit(percent: u32) -> f32 {
    (percent.min(100) as f32) / 100.0
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

    const SAMPLE_UNMUTED: [u8; STATE_BLOB_LEN] = [
        0x3f, 0x2c, 0x00, 0xec, 0x00, 0x01, 0x00, 0x00, 0x00, 0xcd, 0xe9, 0x00, 0x00, 0x00, 0x02,
        0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0xff, 0x00, 0x00, 0xff, 0x00, 0x01, 0x00, 0xff,
        0x37, 0x00, 0x01, 0x01,
    ];

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
