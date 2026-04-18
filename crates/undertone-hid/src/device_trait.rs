//! Abstraction over supported Elgato audio devices.
//!
//! Each supported product (Wave:1, Wave:3, Wave XLR, XLR Dock) provides
//! a concrete type implementing [`Device`]. The daemon holds devices as
//! `Arc<dyn Device>` so new models can be added without changing call
//! sites.

use tokio::sync::broadcast;

use crate::error::HidResult;

/// Elgato product this device represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceModel {
    Wave1,
    Wave3,
    WaveXlr,
    XlrDock,
}

impl DeviceModel {
    /// Human-readable product name for logs and UI.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Wave1 => "Elgato Wave:1",
            Self::Wave3 => "Elgato Wave:3",
            Self::WaveXlr => "Elgato Wave XLR",
            Self::XlrDock => "Elgato XLR Dock",
        }
    }

    /// USB product ID for this model on Elgato's VID `0x0FD9`.
    #[must_use]
    pub fn usb_pid(self) -> u16 {
        match self {
            Self::Wave1 => 0x006D,
            Self::Wave3 => 0x0070,
            Self::WaveXlr => 0x007D,
            Self::XlrDock => 0x0081,
        }
    }
}

/// An 8-bit RGB colour used to drive LED rings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Observable state of a device. Not every field is meaningful for
/// every model — implementations should fill in what they know and
/// leave the rest at defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceState {
    /// Microphone mute state.
    pub mic_muted: bool,
    /// Microphone gain, normalised to `0.0..=1.0`.
    pub mic_gain: f32,
    /// Headphone volume, normalised to `0.0..=1.0`.
    pub headphone_volume: f32,
}

/// An event emitted by a device's physical controls.
#[derive(Debug, Clone)]
pub enum DeviceEvent {
    /// Rotary knob moved. `+1` per clockwise detent, `-1` per counter-clockwise.
    KnobDelta(i32),
    /// Physical mute / tag button state changed.
    TagButton { pressed: bool },
    /// Cached device state changed.
    StateChanged(DeviceState),
}

/// Unified abstraction over supported Elgato audio devices.
///
/// Implementations must be `Send + Sync` so the daemon can share them
/// across the IPC and event-loop tasks. Concrete handles are expected
/// to be cheap to wrap in `Arc`.
pub trait Device: Send + Sync {
    /// Which product this device represents.
    fn model(&self) -> DeviceModel;

    /// USB serial number reported by the device.
    fn serial(&self) -> &str;

    /// Fetch the current device state. May briefly block for devices
    /// that poll over USB control transfers.
    ///
    /// # Errors
    /// Returns an error when the underlying transport fails (USB I/O,
    /// ALSA command failure, etc.).
    fn get_state(&self) -> HidResult<DeviceState>;

    /// Set the microphone mute state.
    ///
    /// # Errors
    /// Returns an error when the write fails.
    fn set_mute(&self, muted: bool) -> HidResult<()>;

    /// Set the microphone gain, normalised to `0.0..=1.0`. Values
    /// outside this range are clamped by the implementation.
    ///
    /// # Errors
    /// Returns an error when the write fails.
    fn set_gain(&self, gain: f32) -> HidResult<()>;

    /// Set the LED ring colours, one [`Rgb`] per zone.
    ///
    /// Extra zones beyond the device's capability are ignored; missing
    /// zones repeat the last colour. Devices without addressable LEDs
    /// may accept this as a no-op (default implementation).
    ///
    /// # Errors
    /// Returns an error when the write fails.
    fn set_led(&self, zones: &[Rgb]) -> HidResult<()> {
        let _ = zones;
        Ok(())
    }

    /// Subscribe to physical-control events (knob, tag button, state
    /// changes). Every subscriber receives every event via a tokio
    /// broadcast channel.
    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent>;
}
