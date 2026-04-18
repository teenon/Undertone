//! Device detection and concrete implementations.
//!
//! This module owns the USB enumeration + ALSA card-discovery logic
//! and provides [`scan_devices`] as the main entry point. Individual
//! device implementations ([`Wave3Handle`], future `WaveXlrHandle`,
//! etc.) implement [`crate::device_trait::Device`] and are returned as
//! `Arc<dyn Device>` so callers can treat them uniformly.

use std::sync::Arc;

use tokio::sync::broadcast;
use tracing::{debug, info};

use crate::alsa_fallback::AlsaMicControl;
use crate::device_trait::{Device, DeviceEvent, DeviceModel, DeviceState, Rgb};
use crate::error::{HidError, HidResult};

/// Elgato USB Vendor ID.
pub const ELGATO_VID: u16 = 0x0FD9;
/// Wave:3 USB Product ID (kept for backwards compatibility).
pub const WAVE3_PID: u16 = 0x0070;
/// Vendor-specific control interface number on all Wave series devices.
pub const CONTROL_INTERFACE: u8 = 3;

/// Default capacity for per-device broadcast channels. Chosen to be
/// large enough to absorb UI reconnects without dropping events, small
/// enough that a stale subscriber doesn't wedge memory.
const EVENT_CHANNEL_CAPACITY: usize = 64;

// =====================================================================
// Wave:3 detection (unchanged public API)
// =====================================================================

/// Represents a detected Wave:3 device before any hardware-control
/// handle is attached. Kept as a pure detection result for
/// backwards-compatible callers.
pub struct Wave3Device {
    serial: String,
    alsa_card: Option<String>,
}

impl Wave3Device {
    /// Attempt to detect a connected Wave:3 device.
    ///
    /// # Errors
    /// Returns an error if ALSA card enumeration fails on an I/O level.
    /// Absence of a device returns `Ok(None)`.
    pub fn detect() -> HidResult<Option<Self>> {
        if let Some(device) = Self::detect_usb()? {
            return Ok(Some(device));
        }

        if let Some(card) = Self::find_alsa_card()? {
            info!(card = %card, "Wave:3 detected via ALSA");
            let serial = Self::get_serial_from_alsa(&card).unwrap_or_else(|| "unknown".to_string());
            return Ok(Some(Self { serial, alsa_card: Some(card) }));
        }

        debug!("No Wave:3 device found");
        Ok(None)
    }

    fn detect_usb() -> HidResult<Option<Self>> {
        let devices = match rusb::devices() {
            Ok(d) => d,
            Err(e) => {
                debug!(error = %e, "Failed to enumerate USB devices");
                return Ok(None);
            }
        };

        for device in devices.iter() {
            let Ok(desc) = device.device_descriptor() else {
                continue;
            };

            if desc.vendor_id() == ELGATO_VID && desc.product_id() == WAVE3_PID {
                let serial = Self::get_usb_serial(&device).unwrap_or_else(|| "unknown".to_string());
                let alsa_card = Self::find_alsa_card().ok().flatten();

                info!(
                    serial = %serial,
                    bus = device.bus_number(),
                    address = device.address(),
                    "Wave:3 detected via USB"
                );

                return Ok(Some(Self { serial, alsa_card }));
            }
        }

        Ok(None)
    }

    fn get_usb_serial<T: rusb::UsbContext>(device: &rusb::Device<T>) -> Option<String> {
        let desc = device.device_descriptor().ok()?;
        let handle = device.open().ok()?;

        if desc.serial_number_string_index().is_some() {
            handle.read_serial_number_string_ascii(&desc).ok()
        } else {
            None
        }
    }

    #[must_use]
    pub fn serial(&self) -> &str {
        &self.serial
    }

    #[must_use]
    pub fn alsa_card(&self) -> Option<&str> {
        self.alsa_card.as_deref()
    }

    fn find_alsa_card() -> HidResult<Option<String>> {
        let cards = std::fs::read_to_string("/proc/asound/cards").map_err(HidError::IoError)?;

        for line in cards.lines() {
            if (line.contains("Wave:3") || line.contains("Wave 3"))
                && let Some(num) = line.split_whitespace().next()
                && let Ok(card_num) = num.parse::<u32>()
            {
                return Ok(Some(format!("hw:{card_num}")));
            }
        }

        Ok(None)
    }

    fn get_serial_from_alsa(_card: &str) -> Option<String> {
        None
    }

    /// Promote a detected device into a cloneable handle that
    /// implements [`Device`].
    #[must_use]
    pub fn into_handle(self) -> Arc<Wave3Handle> {
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let alsa = self.alsa_card.as_deref().map(|c| AlsaMicControl::new(c.to_string()));
        Arc::new(Wave3Handle {
            serial: self.serial,
            alsa,
            event_tx,
        })
    }
}

/// Check whether a Wave:3 device is currently connected via USB.
#[must_use]
pub fn is_wave3_connected() -> bool {
    let Ok(devices) = rusb::devices() else {
        return false;
    };

    for device in devices.iter() {
        if let Ok(desc) = device.device_descriptor()
            && desc.vendor_id() == ELGATO_VID
            && desc.product_id() == WAVE3_PID
        {
            return true;
        }
    }

    false
}

// =====================================================================
// Wave:3 handle (new — implements Device)
// =====================================================================

/// Cloneable, reference-counted handle around a detected Wave:3
/// device. Implements [`Device`] by delegating to ALSA for
/// mute/gain control; LED control is a no-op until the vendor
/// protocol for Wave:3 is reversed.
pub struct Wave3Handle {
    serial: String,
    alsa: Option<AlsaMicControl>,
    event_tx: broadcast::Sender<DeviceEvent>,
}

impl Wave3Handle {
    /// Publish an event to all subscribers. Quiet no-op when there are
    /// no subscribers.
    fn emit(&self, event: DeviceEvent) {
        let _ = self.event_tx.send(event);
    }
}

impl Device for Wave3Handle {
    fn model(&self) -> DeviceModel {
        DeviceModel::Wave3
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
        alsa.set_volume(gain.clamp(0.0, 1.0))?;
        self.emit(DeviceEvent::StateChanged(self.get_state()?));
        Ok(())
    }

    fn set_led(&self, _zones: &[Rgb]) -> HidResult<()> {
        // Wave:3 LED control not yet reversed; accept and ignore so
        // callers can be written against the trait uniformly.
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.event_tx.subscribe()
    }
}

// =====================================================================
// Multi-device scanner
// =====================================================================

/// Enumerate all connected supported Elgato devices and return them as
/// generic handles.
///
/// Additional models are added here as their device modules land
/// (e.g. `WaveXlrHandle` after Task #7).
///
/// # Errors
/// Returns an error only when an enumeration step fails at an I/O
/// level — absence of devices is not an error.
pub fn scan_devices() -> HidResult<Vec<Arc<dyn Device>>> {
    let mut devices: Vec<Arc<dyn Device>> = Vec::new();

    if let Some(wave3) = Wave3Device::detect()? {
        let handle = wave3.into_handle();
        info!(serial = handle.serial(), "Registered Wave:3 device");
        devices.push(handle);
    }

    // Task #7 will add:
    // if let Some(wavexlr) = WaveXlrDevice::detect()? {
    //     let handle = wavexlr.into_handle();
    //     info!(serial = handle.serial(), "Registered Wave XLR device");
    //     devices.push(handle);
    // }

    Ok(devices)
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_model_usb_pids_are_unique() {
        let pids = [
            DeviceModel::Wave1.usb_pid(),
            DeviceModel::Wave3.usb_pid(),
            DeviceModel::WaveXlr.usb_pid(),
            DeviceModel::XlrDock.usb_pid(),
        ];
        let mut sorted = pids;
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), pids.len(), "duplicate PIDs across models");
    }

    #[test]
    fn device_model_names_are_nonempty() {
        for model in [
            DeviceModel::Wave1,
            DeviceModel::Wave3,
            DeviceModel::WaveXlr,
            DeviceModel::XlrDock,
        ] {
            assert!(!model.name().is_empty());
        }
    }

    #[test]
    fn wave3_handle_without_alsa_returns_default_state() {
        let (tx, _) = broadcast::channel(1);
        let handle = Wave3Handle {
            serial: "test".to_string(),
            alsa: None,
            event_tx: tx,
        };
        let state = handle.get_state().unwrap();
        assert_eq!(state, DeviceState::default());
        assert_eq!(handle.model(), DeviceModel::Wave3);
        assert_eq!(handle.serial(), "test");
    }

    #[test]
    fn wave3_handle_without_alsa_errors_on_mute() {
        let (tx, _) = broadcast::channel(1);
        let handle = Wave3Handle {
            serial: "test".to_string(),
            alsa: None,
            event_tx: tx,
        };
        assert!(matches!(handle.set_mute(true), Err(HidError::DeviceNotFound)));
        assert!(matches!(handle.set_gain(0.5), Err(HidError::DeviceNotFound)));
    }

    #[test]
    fn wave3_handle_set_led_is_no_op() {
        let (tx, _) = broadcast::channel(1);
        let handle = Wave3Handle {
            serial: "test".to_string(),
            alsa: None,
            event_tx: tx,
        };
        assert!(handle.set_led(&[Rgb::new(255, 0, 0)]).is_ok());
    }
}
