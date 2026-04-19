//! Undertone Effects — mic effect chain modeling.
//!
//! Owns the data model for the mic-processing effect chain (noise
//! suppression, gate, compressor, parametric EQ) plus the serializer
//! that emits a `pipewire-module-filter-chain` config the daemon can
//! load. The actual `PipeWire` side (loading the module, setting node
//! params) lives in `undertone-pipewire`; this crate is pure data +
//! string formatting and ships no I/O.

pub mod chain;
pub mod effects;
pub mod presets;

pub use chain::{MicChain, MicChainSnapshot};
pub use effects::{EffectInstance, EffectKind, ParamDescriptor};
pub use presets::PresetName;
