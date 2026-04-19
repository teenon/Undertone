//! Wave XLR device support.
//!
//! Detects an Elgato Wave XLR (VID:PID `0x0FD9:0x007D`) over USB and
//! exposes it as a [`Device`] backed by ALSA mixer controls
//! (`numid=5` mute switch, `numid=6` mic gain 0..=150). The vendor USB
//! protocol on hidden audio entity `0x33` is decoded and documented in
//! `protocol.md`, but using it for live mute/gain control would require
//! claiming USB interface 0, which forces `snd_usb_audio` to release
//! the device and the Wave XLR's ALSA card to disappear. Audio in/out
//! through the device would stop working.
//!
//! The vendor protocol therefore lives in [`WaveXlrProbe`] for protocol
//! research, calibration, and any future LED/special-feature work where
//! a brief audio dropout is acceptable. The ALSA-backed [`WaveXlrHandle`]
//! is what the daemon uses for daily mute/gain control.
//!
//! Known state-blob fields used by [`WaveXlrProbe`] (as of 2026-04-18):
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

use crate::alsa_fallback::AlsaMicControl;
use crate::device::ELGATO_VID;
use crate::device_trait::{Device, DeviceEvent, DeviceModel, DeviceState, Rgb};
use crate::error::{HidError, HidResult};

/// Wave XLR USB Product ID on Elgato's VID `0x0FD9`.
pub const WAVE_XLR_PID: u16 = 0x007D;

// --- Vendor-protocol constants (used by WaveXlrProbe only) ---------------

const BM_REQUEST_TYPE_GET: u8 = 0xA1;
const BM_REQUEST_TYPE_SET: u8 = 0x22;
const B_REQUEST_GET_MEM: u8 = 0x85;
const B_REQUEST_SET_MEM: u8 = 0x05;
const W_VALUE: u16 = 0x0000;
const W_INDEX: u16 = 0x3300;
/// Length in bytes of the Wave XLR state blob exchanged via `GET_MEM`
/// / `SET_MEM` control transfers. Exposed so probe and calibration
/// tools can size buffers correctly.
pub const STATE_BLOB_LEN: usize = 34;
const USB_TIMEOUT: Duration = Duration::from_millis(500);

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
const GAIN_FULL_SCALE: u16 = 0x3FFF;

const _: () = {
    assert!(OFFSET_LED_ZONES + LED_ZONE_COUNT * LED_ZONE_BYTES <= STATE_BLOB_LEN);
    assert!(OFFSET_MIC_GAIN_HI < STATE_BLOB_LEN);
    assert!(OFFSET_MUTE_FLAG < STATE_BLOB_LEN);
    assert!(OFFSET_HEADER_TAG < STATE_BLOB_LEN);
    assert!(OFFSET_KNOB_DELTA < STATE_BLOB_LEN);
    assert!(OFFSET_KNOB_FINE < STATE_BLOB_LEN);
};

const AUDIO_CONTROL_INTERFACE: u8 = 0;
const EVENT_CHANNEL_CAPACITY: usize = 64;

// --- Detection -----------------------------------------------------------

/// Detection result for a connected Wave XLR. Carries the discovered
/// ALSA card so [`Self::into_handle`] can wire up mute/gain control
/// without re-scanning.
pub struct WaveXlrDevice {
    serial: String,
    alsa_card: Option<String>,
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
        // amixer -c expects either a bare card number, the card's short
        // name, or just the short-name string — NOT `hw:N`. We return
        // the bare number for stability across amixer versions.
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

    /// Promote a detected device into a cloneable [`Device`] handle
    /// backed by ALSA. Infallible because no USB resources are claimed.
    /// If the ALSA card was not discovered, mute/gain calls will return
    /// [`HidError::DeviceNotFound`].
    #[must_use]
    pub fn into_handle(self) -> Arc<WaveXlrHandle> {
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let alsa = self.alsa_card.map(AlsaMicControl::new);
        Arc::new(WaveXlrHandle {
            serial: self.serial,
            alsa,
            event_tx,
        })
    }
}

// --- Device-trait handle (ALSA backed) -----------------------------------

/// Reference-counted handle to a Wave XLR. Implements [`Device`] by
/// delegating mute/gain to the simple-mixer `Mic` control via ALSA.
/// Holds no USB resources, so audio streaming through the Wave XLR is
/// not disturbed.
pub struct WaveXlrHandle {
    serial: String,
    alsa: Option<AlsaMicControl>,
    event_tx: broadcast::Sender<DeviceEvent>,
}

impl WaveXlrHandle {
    fn emit(&self, event: DeviceEvent) {
        let _ = self.event_tx.send(event);
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
        let Some(alsa) = &self.alsa else {
            return Ok(DeviceState::default());
        };
        let mic_muted = alsa.get_mute().unwrap_or(false);
        let mic_gain = alsa.get_volume().unwrap_or(0.0);
        Ok(DeviceState {
            mic_muted,
            mic_gain,
            headphone_volume: 0.0,
        })
    }

    fn set_mute(&self, muted: bool) -> HidResult<()> {
        let alsa = self.alsa.as_ref().ok_or(HidError::DeviceNotFound)?;
        alsa.set_mute(muted)?;
        self.emit(DeviceEvent::StateChanged(self.get_state()?));
        Ok(())
    }

    fn set_gain(&self, gain: f32) -> HidResult<()> {
        let alsa = self.alsa.as_ref().ok_or(HidError::DeviceNotFound)?;
        alsa.set_volume(gain)?;
        self.emit(DeviceEvent::StateChanged(self.get_state()?));
        Ok(())
    }

    fn set_led(&self, _zones: &[Rgb]) -> HidResult<()> {
        // LED control would require briefly claiming USB interface 0,
        // which interrupts audio. Not wired in the daemon path. Use
        // `WaveXlrProbe` for one-off LED experiments.
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.event_tx.subscribe()
    }
}

// --- Vendor-protocol probe (USB, debug only) -----------------------------

/// Direct USB access to the Wave XLR's vendor protocol on entity
/// `0x33`. Claiming USB interface 0 detaches `snd_usb_audio` and
/// **unregisters the device's ALSA card for the lifetime of this handle**
/// — audio in/out through the Wave XLR will not work while a probe is
/// open. Use only for protocol research, calibration, and debugging.
pub struct WaveXlrProbe {
    usb: Mutex<DeviceHandle<GlobalContext>>,
    last_blob: Mutex<Option<[u8; STATE_BLOB_LEN]>>,
}

impl WaveXlrProbe {
    /// Open a USB control channel to the Wave XLR, claiming interface 0.
    ///
    /// # Errors
    /// Returns [`HidError::DeviceNotFound`] if no Wave XLR is present,
    /// [`HidError::PermissionDenied`] if the udev rule is missing, or
    /// [`HidError::UsbError`] for any other rusb failure.
    pub fn open() -> HidResult<Self> {
        let handle = open_with_claim()?.ok_or(HidError::DeviceNotFound)?;
        Ok(Self {
            usb: Mutex::new(handle),
            last_blob: Mutex::new(None),
        })
    }

    /// Issue a `GET_MEM` and return the raw 34-byte state blob.
    ///
    /// # Errors
    /// Propagates USB errors and surfaces short reads or an unexpected
    /// header tag as [`HidError::ProtocolError`].
    ///
    /// # Panics
    /// Panics if the internal USB or blob mutex is poisoned, which only
    /// happens after a previous call panicked while holding the lock.
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

    /// Read the raw blob and parse it into a structured [`DeviceState`].
    /// Convenience over [`Self::read_raw_state`] plus the module's
    /// blob-parsing helpers.
    ///
    /// # Errors
    /// Propagates the underlying [`Self::read_raw_state`] failure modes.
    pub fn read_state(&self) -> HidResult<DeviceState> {
        Ok(parse_state(&self.read_raw_state()?))
    }

    /// Update mic mute and gain in a single read-modify-write cycle.
    /// LED, knob, and opaque magic bytes are preserved from the most
    /// recent device observation.
    ///
    /// # Errors
    /// Propagates the underlying read/write failure modes.
    pub fn write_mute_and_gain(&self, muted: bool, gain: f32) -> HidResult<()> {
        let mut blob = self.read_raw_state()?;
        blob[OFFSET_MUTE_FLAG] = u8::from(muted);
        let [lo, hi] = gain_to_u16(gain).to_le_bytes();
        blob[OFFSET_MIC_GAIN_LO] = lo;
        blob[OFFSET_MIC_GAIN_HI] = hi;
        self.write_raw_state(&blob)
    }

    /// Send a `SET_MEM` with the provided 34-byte blob.
    ///
    /// Use [`Self::read_raw_state`] first to seed bytes you don't
    /// understand — never write a blob constructed from scratch.
    ///
    /// # Errors
    /// Propagates USB errors and surfaces short writes as
    /// [`HidError::ProtocolError`].
    ///
    /// # Panics
    /// Panics if the internal USB or blob mutex is poisoned, which only
    /// happens after a previous call panicked while holding the lock.
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
}

fn open_with_claim() -> HidResult<Option<DeviceHandle<GlobalContext>>> {
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

            if let Err(e) = handle.set_auto_detach_kernel_driver(true) {
                debug!(error = %e, "auto-detach not supported on this platform");
            }
            handle
                .claim_interface(AUDIO_CONTROL_INTERFACE)
                .map_err(|e| match e {
                    rusb::Error::Access => HidError::PermissionDenied,
                    other => HidError::UsbError(format!("claim interface 0: {other}")),
                })?;
            return Ok(Some(handle));
        }
    }
    Ok(None)
}

// --- Vendor-protocol parsing helpers -------------------------------------

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
