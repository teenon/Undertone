//! Undertone HID — hardware integration for Elgato audio devices.
//!
//! Provides a [`Device`] trait abstraction over supported Elgato
//! products (Wave:1, Wave:3, Wave XLR, XLR Dock). Each model ships as
//! a sibling module implementing the trait.
//!
//! **Note**: Elgato's Wave series uses a vendor-specific USB interface
//! (not standard HID), so device modules talk to the hardware via
//! `rusb` control transfers. ALSA remains available as a fallback for
//! simple mic volume/mute control on devices where the vendor protocol
//! is not yet reversed.

pub mod alsa_fallback;
pub mod device;
pub mod device_trait;
pub mod error;
pub mod wavexlr;

pub use device::{Wave3Device, Wave3Handle, is_wave3_connected, scan_devices};
pub use device_trait::{Device, DeviceEvent, DeviceModel, DeviceState, Rgb};
pub use error::{HidError, HidResult};
pub use wavexlr::{WAVE_XLR_PID, WaveXlrDevice, WaveXlrHandle};
