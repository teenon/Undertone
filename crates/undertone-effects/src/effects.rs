//! Effect kinds, parameter descriptors, and per-instance state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One of the four effect slots in the mic chain. The order in which
/// instances appear in [`crate::chain::MicChain`] matches the signal
/// flow: `NoiseSuppression` → `Gate` → `Compressor` → `Equalizer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    NoiseSuppression,
    Gate,
    Compressor,
    Equalizer,
}

impl EffectKind {
    /// Stable identifier used as the `PipeWire` filter-graph node name
    /// (e.g. `ns`, `gate`, `comp`, `eq`). Must stay short and
    /// `[a-z0-9_]+` — `pw-cli set-param` looks nodes up by this name.
    #[must_use]
    pub fn node_id(self) -> &'static str {
        match self {
            Self::NoiseSuppression => "ns",
            Self::Gate => "gate",
            Self::Compressor => "comp",
            Self::Equalizer => "eq",
        }
    }

    /// Human-readable label for UI rendering.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NoiseSuppression => "Noise Suppression",
            Self::Gate => "Noise Gate",
            Self::Compressor => "Compressor",
            Self::Equalizer => "Equalizer",
        }
    }

    /// All four effect kinds in signal-flow order.
    #[must_use]
    pub fn all() -> &'static [EffectKind] {
        &[
            Self::NoiseSuppression,
            Self::Gate,
            Self::Compressor,
            Self::Equalizer,
        ]
    }
}

/// Description of a single tunable parameter on an effect. Drives the
/// generic slider rendering in the UI — no per-effect special cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamDescriptor {
    /// Internal control-port name as exposed by the LV2/LADSPA plugin.
    /// Used as the lookup key in `pw-cli set-param Props`.
    pub name: &'static str,
    /// Human-readable label for the slider.
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub step: f32,
    pub unit: &'static str,
}

/// Live state of one effect instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectInstance {
    pub kind: EffectKind,
    pub bypassed: bool,
    /// Current parameter values keyed by `ParamDescriptor::name`.
    /// `BTreeMap` for deterministic JSON serialization (snapshot
    /// equality across polls).
    pub params: BTreeMap<String, f32>,
}

impl EffectInstance {
    /// Build an instance with the descriptor defaults and `bypassed=true`.
    #[must_use]
    pub fn default_for(kind: EffectKind) -> Self {
        let params = descriptors_for(kind)
            .iter()
            .map(|d| (d.name.to_string(), d.default))
            .collect();
        Self {
            kind,
            // Effects start bypassed so the daemon never silently colours
            // the user's mic on first run; user opts each one in.
            bypassed: true,
            params,
        }
    }
}

/// Static parameter table for a given effect. `()` matches `&[]`.
#[must_use]
pub fn descriptors_for(kind: EffectKind) -> &'static [ParamDescriptor] {
    match kind {
        EffectKind::NoiseSuppression => NOISE_SUPPRESSION_PARAMS,
        EffectKind::Gate => GATE_PARAMS,
        EffectKind::Compressor => COMPRESSOR_PARAMS,
        EffectKind::Equalizer => EQUALIZER_PARAMS,
    }
}

// --- Parameter tables ---------------------------------------------------
//
// Control names match the LV2/LADSPA plugins listed in `chain.rs`:
//   - rnnoise (LADSPA, `noise_suppressor_mono`)
//   - LSP `gate_mono`
//   - LSP `compressor_mono`
//   - LSP `para_equalizer_x16_mono` (we wire only 4 bands)
//
// Bypass is a separate boolean on the instance, NOT a parameter — for
// LSP plugins it's mapped to the `bp` port at write time; for RNNoise
// (no bypass port) we collapse VAD threshold to 0 when bypassed.

const NOISE_SUPPRESSION_PARAMS: &[ParamDescriptor] = &[ParamDescriptor {
    name: "VAD Threshold (%)",
    label: "Threshold",
    min: 0.0,
    max: 100.0,
    default: 50.0,
    step: 1.0,
    unit: "%",
}];

const GATE_PARAMS: &[ParamDescriptor] = &[
    ParamDescriptor {
        name: "th",
        label: "Threshold",
        min: -72.0,
        max: 0.0,
        default: -36.0,
        step: 0.5,
        unit: "dB",
    },
    ParamDescriptor {
        name: "at",
        label: "Attack",
        min: 0.1,
        max: 100.0,
        default: 1.5,
        step: 0.1,
        unit: "ms",
    },
    ParamDescriptor {
        name: "rt",
        label: "Release",
        min: 1.0,
        max: 1000.0,
        default: 50.0,
        step: 1.0,
        unit: "ms",
    },
    ParamDescriptor {
        name: "rng",
        label: "Range",
        min: -60.0,
        max: 0.0,
        default: -24.0,
        step: 0.5,
        unit: "dB",
    },
];

const COMPRESSOR_PARAMS: &[ParamDescriptor] = &[
    ParamDescriptor {
        name: "th",
        label: "Threshold",
        min: -60.0,
        max: 0.0,
        default: -18.0,
        step: 0.5,
        unit: "dB",
    },
    ParamDescriptor {
        name: "ratio",
        label: "Ratio",
        min: 1.0,
        max: 20.0,
        default: 3.0,
        step: 0.1,
        unit: ":1",
    },
    ParamDescriptor {
        name: "at",
        label: "Attack",
        min: 0.1,
        max: 100.0,
        default: 5.0,
        step: 0.1,
        unit: "ms",
    },
    ParamDescriptor {
        name: "rt",
        label: "Release",
        min: 1.0,
        max: 1000.0,
        default: 50.0,
        step: 1.0,
        unit: "ms",
    },
    ParamDescriptor {
        name: "makeup",
        label: "Makeup",
        min: 0.0,
        max: 24.0,
        default: 0.0,
        step: 0.5,
        unit: "dB",
    },
];

/// Four-band parametric EQ: low shelf → low-mid → high-mid → high shelf.
/// LSP's `para_equalizer_x16_mono` exposes ports as `fg_<n>`, `g_<n>`,
/// `q_<n>` for band index `n`. Bands beyond 3 are left at unity gain.
const EQUALIZER_PARAMS: &[ParamDescriptor] = &[
    // Band 0
    ParamDescriptor { name: "fg_0", label: "Low Freq",   min: 20.0,  max: 500.0,  default: 80.0,   step: 1.0, unit: "Hz" },
    ParamDescriptor { name: "g_0",  label: "Low Gain",   min: -24.0, max: 24.0,   default: 0.0,    step: 0.5, unit: "dB" },
    ParamDescriptor { name: "q_0",  label: "Low Q",      min: 0.1,   max: 10.0,   default: 1.0,    step: 0.1, unit: "" },
    // Band 1
    ParamDescriptor { name: "fg_1", label: "LowMid Freq", min: 100.0, max: 1000.0, default: 250.0, step: 1.0, unit: "Hz" },
    ParamDescriptor { name: "g_1",  label: "LowMid Gain", min: -24.0, max: 24.0,   default: 0.0,   step: 0.5, unit: "dB" },
    ParamDescriptor { name: "q_1",  label: "LowMid Q",    min: 0.1,   max: 10.0,   default: 1.0,   step: 0.1, unit: "" },
    // Band 2
    ParamDescriptor { name: "fg_2", label: "HighMid Freq", min: 1000.0, max: 8000.0,  default: 2500.0, step: 10.0, unit: "Hz" },
    ParamDescriptor { name: "g_2",  label: "HighMid Gain", min: -24.0,  max: 24.0,    default: 0.0,    step: 0.5,  unit: "dB" },
    ParamDescriptor { name: "q_2",  label: "HighMid Q",    min: 0.1,    max: 10.0,    default: 1.0,    step: 0.1,  unit: "" },
    // Band 3
    ParamDescriptor { name: "fg_3", label: "High Freq",   min: 4000.0, max: 20000.0, default: 8000.0, step: 10.0, unit: "Hz" },
    ParamDescriptor { name: "g_3",  label: "High Gain",   min: -24.0,  max: 24.0,    default: 0.0,    step: 0.5,  unit: "dB" },
    ParamDescriptor { name: "q_3",  label: "High Q",      min: 0.1,    max: 10.0,    default: 1.0,    step: 0.1,  unit: "" },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_kinds_have_unique_node_ids() {
        let ids: Vec<_> = EffectKind::all().iter().map(|k| k.node_id()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "duplicate node ids");
    }

    #[test]
    fn default_instance_uses_descriptor_defaults() {
        for &kind in EffectKind::all() {
            let inst = EffectInstance::default_for(kind);
            assert!(inst.bypassed, "should start bypassed");
            for d in descriptors_for(kind) {
                assert_eq!(
                    inst.params.get(d.name).copied(),
                    Some(d.default),
                    "{:?}.{} default mismatch",
                    kind,
                    d.name
                );
            }
        }
    }

    #[test]
    fn descriptors_have_sensible_ranges() {
        for &kind in EffectKind::all() {
            for d in descriptors_for(kind) {
                assert!(d.min < d.max, "{}: min<max", d.name);
                assert!(d.default >= d.min && d.default <= d.max, "{}: default in range", d.name);
                assert!(d.step > 0.0, "{}: step>0", d.name);
            }
        }
    }
}
