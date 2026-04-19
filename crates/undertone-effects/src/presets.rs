//! Built-in chain presets. Names are user-facing.

use crate::chain::MicChain;
use crate::effects::EffectKind;

/// Stable preset identifiers shipped with the daemon.
#[derive(Debug, Clone, Copy)]
pub enum PresetName {
    /// All four effects bypassed; mic passes through unprocessed.
    Off,
    /// Light noise suppression + gentle compression. Conservative
    /// settings that work for most voice content out of the box.
    Voice,
    /// More aggressive noise suppression + tighter gate + stronger
    /// compression + presence boost. Aimed at streaming with a
    /// noisier room.
    Streaming,
    /// Soft compression, no gate, high-shelf air boost. Optimised
    /// for sung vocals where you don't want the gate cutting tails.
    Singing,
}

impl PresetName {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Voice => "Voice",
            Self::Streaming => "Streaming",
            Self::Singing => "Singing",
        }
    }

    #[must_use]
    pub fn all() -> &'static [PresetName] {
        &[Self::Off, Self::Voice, Self::Streaming, Self::Singing]
    }

    /// Build a chain with this preset applied. Effects are only
    /// un-bypassed where the preset specifically wants them on.
    #[must_use]
    pub fn build_chain(self) -> MicChain {
        let mut chain = MicChain {
            preset: Some(self.label().to_string()),
            ..MicChain::default()
        };
        match self {
            Self::Off => { /* default chain is all bypassed */ }
            Self::Voice => {
                if let Some(ns) = chain.effect_mut(EffectKind::NoiseSuppression) {
                    ns.bypassed = false;
                    ns.params.insert("VAD Threshold (%)".into(), 50.0);
                }
                if let Some(c) = chain.effect_mut(EffectKind::Compressor) {
                    c.bypassed = false;
                    c.params.insert("th".into(), -18.0);
                    c.params.insert("ratio".into(), 2.5);
                    c.params.insert("makeup".into(), 3.0);
                }
            }
            Self::Streaming => {
                if let Some(ns) = chain.effect_mut(EffectKind::NoiseSuppression) {
                    ns.bypassed = false;
                    ns.params.insert("VAD Threshold (%)".into(), 65.0);
                }
                if let Some(g) = chain.effect_mut(EffectKind::Gate) {
                    g.bypassed = false;
                    g.params.insert("th".into(), -32.0);
                    g.params.insert("rng".into(), -30.0);
                }
                if let Some(c) = chain.effect_mut(EffectKind::Compressor) {
                    c.bypassed = false;
                    c.params.insert("th".into(), -20.0);
                    c.params.insert("ratio".into(), 4.0);
                    c.params.insert("makeup".into(), 4.0);
                }
                if let Some(eq) = chain.effect_mut(EffectKind::Equalizer) {
                    eq.bypassed = false;
                    eq.params.insert("g_2".into(), 2.5);  // presence
                    eq.params.insert("g_3".into(), 2.0);  // air
                }
            }
            Self::Singing => {
                if let Some(ns) = chain.effect_mut(EffectKind::NoiseSuppression) {
                    ns.bypassed = false;
                    ns.params.insert("VAD Threshold (%)".into(), 35.0);
                }
                if let Some(c) = chain.effect_mut(EffectKind::Compressor) {
                    c.bypassed = false;
                    c.params.insert("th".into(), -22.0);
                    c.params.insert("ratio".into(), 2.0);
                    c.params.insert("at".into(), 10.0);
                    c.params.insert("rt".into(), 200.0);
                    c.params.insert("makeup".into(), 2.0);
                }
                if let Some(eq) = chain.effect_mut(EffectKind::Equalizer) {
                    eq.bypassed = false;
                    eq.params.insert("g_3".into(), 3.0); // air shelf
                }
            }
        }
        chain
    }

    /// Look a preset up by case-sensitive label.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::all().iter().copied().find(|p| p.label() == label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_preset_keeps_everything_bypassed() {
        let chain = PresetName::Off.build_chain();
        for inst in &chain.effects {
            assert!(inst.bypassed, "{:?} should be bypassed in Off preset", inst.kind);
        }
        assert_eq!(chain.preset.as_deref(), Some("Off"));
    }

    #[test]
    fn streaming_preset_enables_all_four() {
        let chain = PresetName::Streaming.build_chain();
        for inst in &chain.effects {
            assert!(!inst.bypassed, "{:?} should be active in Streaming preset", inst.kind);
        }
    }

    #[test]
    fn presets_round_trip_via_label() {
        for &p in PresetName::all() {
            assert_eq!(PresetName::from_label(p.label()).map(PresetName::label), Some(p.label()));
        }
        assert!(PresetName::from_label("not-a-preset").is_none());
    }
}
