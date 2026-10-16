//! Which SMC temperature keys mean what, per chip.
//!
//! Keys are only labelled here; the SMC is still enumerated at start-up and
//! only keys that exist are read. `Mac17,8` (M5 Pro) was mapped on the test
//! machine. The M1–M4 rows come from Stats `Modules/Sensors/values.swift`
//! and are unverified. Chips with no row fall back to classification by key
//! prefix, which never guesses at `Tf*` or `Tm*`.

use crate::zones::TempZone;
use siltop_core::ClusterKind::{Efficiency, Performance, Super};

/// `("Tp00", zone)` matches one key; `("Tg*", zone)` matches a prefix.
type Rule = (&'static str, TempZone);

pub(crate) struct Chip {
    rules: &'static [Rule],
}

const E: TempZone = TempZone::Cluster(Efficiency);
const P: TempZone = TempZone::Cluster(Performance);
const S: TempZone = TempZone::Cluster(Super);
const GPU: TempZone = TempZone::Gpu;

const COMMON: &[Rule] = &[
    ("TB0T", TempZone::Battery),
    ("TB1T", TempZone::Battery),
    ("TB2T", TempZone::Battery),
    ("TH0*", TempZone::Nand),
];

/// M5 Pro, Mac17,8. Verified on the test machine.
static MAC17_8: Chip = Chip {
    rules: M5_PRO_RULES,
};

const M5_PRO_RULES: &[Rule] = &[
    ("Tp00", S),
    ("Tp04", S),
    ("Tp08", S),
    ("Tp0C", S),
    ("Tp0G", S),
    ("Tp0K", S),
    ("Tp0O", P),
    ("Tp0R", P),
    ("Tp0U", P),
    ("Tp0X", P),
    ("Tp0a", P),
    ("Tp0d", P),
    ("Tp0g", P),
    ("Tp0j", P),
    ("Tp0m", P),
    ("Tp0p", P),
    ("Tp0u", P),
    ("Tp0y", P),
    ("Tg*", GPU),
    // Hardware maxima over the TPD*/TRD*/TUD* die sensor banks.
    ("TPDX", TempZone::Hotspot),
    ("TRDX", TempZone::Hotspot),
    ("TUDX", TempZone::Hotspot),
];

/// Other M5 chips: the M5 Pro map, unverified.
static M5: Chip = Chip {
    rules: M5_PRO_RULES,
};

/// M4 family, from Stats; unverified.
static M4: Chip = Chip {
    rules: &[
        ("Te05", E),
        ("Te0S", E),
        ("Te09", E),
        ("Te0H", E),
        ("Tp01", P),
        ("Tp05", P),
        ("Tp09", P),
        ("Tp0D", P),
        ("Tp0V", P),
        ("Tp0Y", P),
        ("Tp0b", P),
        ("Tp0e", P),
        ("Tg*", GPU),
    ],
};

/// M3 family, from Stats; unverified.
static M3: Chip = Chip {
    rules: &[
        ("Te05", E),
        ("Te0L", E),
        ("Te0P", E),
        ("Te0S", E),
        ("Tf04", P),
        ("Tf09", P),
        ("Tf0A", P),
        ("Tf0B", P),
        ("Tf0D", P),
        ("Tf0E", P),
        ("Tf44", P),
        ("Tf49", P),
        ("Tf4A", P),
        ("Tf4B", P),
        ("Tf4D", P),
        ("Tf4E", P),
        ("Tf14", GPU),
        ("Tf18", GPU),
        ("Tf19", GPU),
        ("Tf1A", GPU),
        ("Tf24", GPU),
        ("Tf28", GPU),
        ("Tf29", GPU),
        ("Tf2A", GPU),
    ],
};

/// M2 family, from Stats; unverified.
static M2: Chip = Chip {
    rules: &[
        ("Tp1h", E),
        ("Tp1t", E),
        ("Tp1p", E),
        ("Tp1l", E),
        ("Tp01", P),
        ("Tp05", P),
        ("Tp09", P),
        ("Tp0D", P),
        ("Tp0X", P),
        ("Tp0b", P),
        ("Tp0f", P),
        ("Tp0j", P),
        ("Tg0f", GPU),
        ("Tg0j", GPU),
    ],
};

/// M1 family, from Stats; unverified.
static M1: Chip = Chip {
    rules: &[
        ("Tp09", E),
        ("Tp0T", E),
        ("Tp01", P),
        ("Tp05", P),
        ("Tp0D", P),
        ("Tp0H", P),
        ("Tp0L", P),
        ("Tp0P", P),
        ("Tp0X", P),
        ("Tp0b", P),
        ("Tg05", GPU),
        ("Tg0D", GPU),
        ("Tg0L", GPU),
        ("Tg0T", GPU),
    ],
};

/// The table for this machine: an exact `hw.model` first, then the brand
/// string's chip generation.
pub(crate) fn chip_for(model: &str, brand: &str) -> Option<&'static Chip> {
    if model == "Mac17,8" {
        return Some(&MAC17_8);
    }
    let generation = brand.strip_prefix("Apple ")?.split_whitespace().next()?;
    match generation {
        "M5" => Some(&M5),
        "M4" => Some(&M4),
        "M3" => Some(&M3),
        "M2" => Some(&M2),
        "M1" => Some(&M1),
        _ => None,
    }
}

/// What `key` measures on `chip`, or by prefix when there is no table.
/// `None` means the key is not shown.
pub(crate) fn classify(chip: Option<&Chip>, key: &str) -> Option<TempZone> {
    match chip {
        Some(chip) => lookup(chip.rules, key).or_else(|| lookup(COMMON, key)),
        None => by_prefix(key),
    }
}

fn lookup(rules: &[Rule], key: &str) -> Option<TempZone> {
    rules
        .iter()
        .find(|(pattern, _)| *pattern == key)
        .or_else(|| {
            rules.iter().find(|(pattern, _)| {
                pattern
                    .strip_suffix('*')
                    .is_some_and(|prefix| key.starts_with(prefix))
            })
        })
        .map(|(_, zone)| *zone)
}

fn by_prefix(key: &str) -> Option<TempZone> {
    if let Some(zone) = lookup(COMMON, key) {
        return Some(zone);
    }
    let prefix = key.get(..2)?;
    match prefix {
        "Te" => Some(E),
        "Tp" | "TC" => Some(TempZone::Cpu),
        "Tg" => Some(GPU),
        _ if key == "TG0P" || key == "TG0D" => Some(GPU),
        _ => None,
    }
}

/// SMC power keys (`flt`, watts) whose sum is CPU package power, per exact
/// `hw.model`. Rails differ between machines, so an unlisted model gets no
/// CPU watts rather than a guess.
struct CpuRails {
    model: &'static str,
    keys: &'static [&'static str],
}

/// Mac17,8 (M5 Pro): Super cluster `PP0b`+`PP4b`, first Performance cluster
/// `PP1b`+`PP7b`, second `PR0b`+`PR7b`. Calibrated against `IOReport` "CPU
/// Energy" at 0.90–1.00× over publication spans of 2.2–94 s.
static CPU_RAILS: &[CpuRails] = &[CpuRails {
    model: "Mac17,8",
    keys: &["PP0b", "PP4b", "PP1b", "PP7b", "PR0b", "PR7b"],
}];

/// The CPU rail keys for this exact model, if mapped.
pub(crate) fn cpu_rails(model: &str) -> Option<&'static [&'static str]> {
    CPU_RAILS
        .iter()
        .find(|rails| rails.model == model)
        .map(|rails| rails.keys)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Every T key on the Mac17,8 test machine (macOS 27.0, `#KEY` = 3626).
    const MAC17_8_T_KEYS: &str = "TAOL TB0T TB1T TB2T TCHP TCMb TD00 TD01 TD02 TD03 TD04 TD10 \
        TD11 TD12 TD13 TD14 TD20 TD21 TD22 TD23 TD24 TDBP TDCR TDEC TDEL TDER TDTP TDVx TDeL \
        TDeR TF2S TG0B TG0C TG0H TG0V TG1B TG2B TH0a TH0b TH0x TMVR TN00 TN01 TN02 TN03 TPD0 \
        TPD1 TPD2 TPD3 TPD4 TPD5 TPD6 TPD7 TPD8 TPD9 TPDX TPDa TPDb TPDc TPDd TPDe TPDf TPH3 \
        TPMP TPSP TR0Z TR1d TR2d TRD0 TRD1 TRD2 TRD3 TRD4 TRD5 TRD6 TRD7 TRD8 TRD9 TRDX TRDa \
        TRDb TRDc TRDd TRDe TRDf TS0P TSG1 TTPD TUD0 TUD1 TUD2 TUD3 TUD4 TUD5 TUD6 TUD7 TUD8 \
        TUD9 TUDX TUDa TUDb TUDc TUDd TUDe TUDf TV11 TV12 TV13 TV14 TVA0 TVAi TVCi TVD0 TVDA \
        TVDG TVDM TVDP TVDc TVDi TVGi TVMD TVMN TVMR TVMX TVMi TVMj TVMr TVNN TVPi TVS0 TVS1 \
        TVS2 TVV0 TVVi TVmS TVms TW0P Ta00 Ta04 Ta08 Ta0K Ta0O Ta0R TaLP TaLT TaLW TaRF TaRT \
        TaRW TaTP TfC0 TfC1 TfC3 TfC5 TfCB TfCD TfCE TfCF Tg08 Tg0C Tg0O Tg0R Tg0U Tg0X Tg0a \
        Tg0d Tg0g Tg0j Tg12 Tg16 Tg1I Tg1M Tg1Q Tg1U Tg1Y Tg1c Tg1k Tg1o Tg1x Tg29 Tg2D Tg2P \
        Tg2T Tg2X Tg2b Tg2f Tg2j Tg2n Tg2r Tg3B Tg3F Tg3R Tg3V Tg3Z Tg3d Tg3h Tg3l Tg3t Tg3x \
        Tg43 Tm00 Tm04 Tm08 Tm0C Tm0G Tm0K Tm0O Tm0R Tm0U Tm0X Tm0a Tm0d Tm0g Tm0j Tm0m Tm0p \
        Tm0u Tm0y Tm1E Tm1I Tm1M Tm1Q Tm1U Tm1Y Tm1c Tm1g Tm1k Tm1o Tm1s Tm1x Tm21 Tm25 Tm29 \
        Tm2D Tm2H Tm2L Tm2P Tm2T Tm2j Tm2n Tp00 Tp04 Tp08 Tp0C Tp0G Tp0K Tp0O Tp0R Tp0U Tp0X \
        Tp0a Tp0d Tp0g Tp0j Tp0m Tp0p Tp0u Tp0y Tp1E Tp1I Tp1Q Tp1U Tp1g Ts00 Ts04 Ts08 Ts0C \
        Ts0G Ts0K Ts0O Ts0P Ts0R Ts0U Ts0X Ts0a Ts1P Tz11 Tz12 Tz13 Tz14 Tz15 Tz16 Tz17 Tz18 \
        Tz1j";

    fn count(chip: Option<&Chip>, zone: TempZone) -> Vec<&'static str> {
        MAC17_8_T_KEYS
            .split_whitespace()
            .filter(|k| classify(chip, k) == Some(zone))
            .collect()
    }

    #[test]
    fn m5_pro_zones_from_its_key_list() {
        let chip = chip_for("Mac17,8", "Apple M5 Pro");
        assert!(chip.is_some_and(|c| std::ptr::eq(c, &raw const MAC17_8)));
        assert_eq!(count(chip, S).len(), 6);
        assert_eq!(count(chip, P).len(), 12);
        assert_eq!(count(chip, E).len(), 0);
        assert_eq!(count(chip, GPU).len(), 42);
        assert_eq!(count(chip, TempZone::Hotspot), vec!["TPDX", "TRDX", "TUDX"]);
        assert_eq!(count(chip, TempZone::Battery), vec!["TB0T", "TB1T", "TB2T"]);
        assert_eq!(count(chip, TempZone::Nand), vec!["TH0a", "TH0b", "TH0x"]);
        assert_eq!(count(chip, TempZone::Cpu).len(), 0);
        for junk in [
            "TR0Z", "TfC0", "Tm00", "TG0B", "TVGi", "Tz11", "Tp1E", "TCMb",
        ] {
            assert_eq!(classify(chip, junk), None, "{junk}");
        }
    }

    #[test]
    fn prefix_fallback_never_guesses_tf_or_tm() {
        assert_eq!(classify(None, "Tp0O"), Some(TempZone::Cpu));
        assert_eq!(classify(None, "Te05"), Some(E));
        assert_eq!(classify(None, "Tg0j"), Some(GPU));
        assert_eq!(classify(None, "TG0P"), Some(GPU));
        assert_eq!(classify(None, "TC0P"), Some(TempZone::Cpu));
        assert_eq!(classify(None, "TB1T"), Some(TempZone::Battery));
        assert_eq!(classify(None, "TH0x"), Some(TempZone::Nand));
        for junk in ["TfC0", "Tf04", "Tm0p", "TR0Z", "TG0B", "TVGi", "Ts0P"] {
            assert_eq!(classify(None, junk), None, "{junk}");
        }
    }

    #[test]
    fn chips_resolve_by_model_then_generation() {
        let is = |chip: Option<&'static Chip>, want: &'static Chip| {
            chip.is_some_and(|c| std::ptr::eq(c, want))
        };
        assert!(is(chip_for("Mac17,8", ""), &MAC17_8));
        assert!(is(chip_for("Mac17,2", "Apple M5 Max"), &M5));
        assert!(is(chip_for("Mac16,1", "Apple M4 Max"), &M4));
        assert!(is(chip_for("MacBookPro18,1", "Apple M1 Pro"), &M1));
        assert!(chip_for("iMac20,1", "Intel(R) Core(TM) i9").is_none());
        assert!(chip_for("Mac99,1", "Apple M9").is_none());
    }

    #[test]
    fn m3_maps_tf_through_its_table() {
        let chip = chip_for("Mac15,3", "Apple M3");
        assert_eq!(classify(chip, "Tf04"), Some(P));
        assert_eq!(classify(chip, "Tf14"), Some(GPU));
        assert_eq!(classify(chip, "TB0T"), Some(TempZone::Battery));
        assert_eq!(classify(chip, "TfC0"), None);
    }
}
