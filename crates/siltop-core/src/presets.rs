//! Named looks: each preset is plain data — which panels show, which
//! surface, how processes sort, and whether zen hides the chrome. Applying
//! one writes those fields into the [`Config`]; the active preset is never
//! stored, it is recognised from the config, so a hand-tweaked toggle reads
//! as "custom" instead of a stale name.

use crate::config::{Config, ProcSort};
use crate::surface::Surface;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Preset {
    /// The chip itself: cpu, gpu, power and heat.
    Silicon,
    /// Local models: gpu, memory and power, processes by memory.
    Ml,
    /// On battery: power, cpu and heat.
    Laptop,
    /// Operations: a wide process table beside net and disk.
    Sre,
    /// Every graph on the glance surface, chrome hidden.
    Ricer,
}

/// What a preset sets. Fields mirror [`Config`] one to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetSpec {
    pub gpu: bool,
    pub power: bool,
    pub net: bool,
    pub disk: bool,
    pub fans: bool,
    pub cores: bool,
    pub surface: Option<Surface>,
    pub sort: ProcSort,
    pub zen: bool,
}

impl Preset {
    /// Menu order; `1`–`5` pick them in this order.
    pub const ALL: [Self; 5] = [
        Self::Silicon,
        Self::Ml,
        Self::Laptop,
        Self::Sre,
        Self::Ricer,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Silicon => "silicon",
            Self::Ml => "ml",
            Self::Laptop => "laptop",
            Self::Sre => "sre",
            Self::Ricer => "ricer",
        }
    }

    /// Case-insensitive inverse of [`label`](Self::label).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.label().eq_ignore_ascii_case(text.trim()))
    }

    #[must_use]
    pub const fn spec(self) -> PresetSpec {
        let base = PresetSpec {
            gpu: false,
            power: false,
            net: false,
            disk: false,
            fans: false,
            cores: false,
            surface: None,
            sort: ProcSort::Cpu,
            zen: false,
        };
        match self {
            Self::Silicon => PresetSpec {
                gpu: true,
                power: true,
                fans: true,
                cores: true,
                ..base
            },
            Self::Ml => PresetSpec {
                gpu: true,
                power: true,
                sort: ProcSort::Mem,
                ..base
            },
            Self::Laptop => PresetSpec {
                power: true,
                fans: true,
                cores: true,
                ..base
            },
            Self::Sre => PresetSpec {
                net: true,
                disk: true,
                surface: Some(Surface::Work),
                ..base
            },
            Self::Ricer => PresetSpec {
                gpu: true,
                power: true,
                net: true,
                disk: true,
                fans: true,
                cores: true,
                surface: Some(Surface::Glance),
                zen: true,
                ..base
            },
        }
    }

    /// Writes this preset's fields into `cfg`, leaving everything else; its
    /// sort column runs its natural way (memory biggest first).
    pub fn apply(self, cfg: &mut Config) {
        let s = self.spec();
        cfg.show_gpu = s.gpu;
        cfg.show_power = s.power;
        cfg.show_net = s.net;
        cfg.show_disk = s.disk;
        cfg.show_fans = s.fans;
        cfg.show_cores = s.cores;
        cfg.surface = s.surface;
        cfg.proc_sort = s.sort;
        cfg.sort_desc = s.sort.natural_descending();
        cfg.zen = s.zen;
    }

    /// The preset `cfg` currently matches exactly, or `None` for custom.
    #[must_use]
    pub fn matching(cfg: &Config) -> Option<Self> {
        let now = PresetSpec {
            gpu: cfg.show_gpu,
            power: cfg.show_power,
            net: cfg.show_net,
            disk: cfg.show_disk,
            fans: cfg.show_fans,
            cores: cfg.show_cores,
            surface: cfg.surface,
            sort: cfg.proc_sort,
            zen: cfg.zen,
        };
        Self::ALL.into_iter().find(|p| p.spec() == now)
    }
}

/// `"custom"` when no preset matches.
#[must_use]
pub fn preset_label(preset: Option<Preset>) -> &'static str {
    preset.map_or("custom", Preset::label)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_spec_is_distinct_so_matching_is_unambiguous() {
        for (i, a) in Preset::ALL.iter().enumerate() {
            for b in &Preset::ALL[i + 1..] {
                assert_ne!(a.spec(), b.spec(), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn apply_then_matching_recognises_each_preset() {
        for p in Preset::ALL {
            let mut cfg = Config::default();
            p.apply(&mut cfg);
            assert_eq!(Preset::matching(&cfg), Some(p));
        }
    }

    #[test]
    fn preset_survives_a_toml_round_trip() {
        for p in Preset::ALL {
            let mut cfg = Config::default();
            p.apply(&mut cfg);
            let text = cfg.to_toml().unwrap();
            let back = Config::from_toml(&text).unwrap().config;
            assert_eq!(Preset::matching(&back), Some(p), "{text}");
            assert_eq!(back.zen, cfg.zen);
        }
    }

    #[test]
    fn a_hand_tweak_reads_as_custom() {
        let mut cfg = Config::default();
        Preset::Sre.apply(&mut cfg);
        cfg.show_gpu = true;
        assert_eq!(Preset::matching(&cfg), None);
        assert_eq!(preset_label(Preset::matching(&cfg)), "custom");
    }

    #[test]
    fn apply_leaves_unrelated_fields_alone() {
        let mut cfg = Config {
            theme: String::from("dracula"),
            net_bits: true,
            ..Config::default()
        };
        Preset::Ricer.apply(&mut cfg);
        assert_eq!(cfg.theme, "dracula");
        assert!(cfg.net_bits);
        assert!(cfg.zen, "ricer hides the chrome");
    }

    /// Presets that promise the power box used to leave it hidden when
    /// settings had turned it off, and still read as that preset.
    #[test]
    fn presets_that_promise_power_show_it() {
        for p in [Preset::Silicon, Preset::Ml, Preset::Laptop, Preset::Ricer] {
            assert!(p.spec().power, "{p:?} promises the power box");
        }
        for p in Preset::ALL {
            let promised = p.spec().power;
            let mut cfg = Config {
                show_power: !promised,
                ..Config::default()
            };
            p.apply(&mut cfg);
            assert_eq!(cfg.show_power, promised, "{p:?}");
            cfg.show_power = !promised;
            assert_eq!(Preset::matching(&cfg), None, "{p:?} ignores power");
        }
    }

    #[test]
    fn apply_resets_a_reversed_sort_to_the_columns_natural_way() {
        let mut cfg = Config {
            sort_desc: false,
            ..Config::default()
        };
        Preset::Ml.apply(&mut cfg);
        assert_eq!(cfg.proc_sort, ProcSort::Mem);
        assert!(cfg.sort_desc, "memory biggest first");
    }

    #[test]
    fn labels_parse_back() {
        for p in Preset::ALL {
            assert_eq!(Preset::parse(p.label()), Some(p));
            assert_eq!(Preset::parse(&p.label().to_uppercase()), Some(p));
        }
        assert_eq!(Preset::parse("custom"), None);
    }

    #[test]
    fn every_preset_keeps_at_least_one_graph_panel() {
        for p in Preset::ALL {
            let s = p.spec();
            assert!(
                s.gpu || s.net || s.disk || s.fans || s.cores,
                "{p:?} shows nothing but cpu/mem"
            );
        }
    }
}
