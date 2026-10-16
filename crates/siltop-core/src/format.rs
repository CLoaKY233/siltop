#[must_use]
pub fn bytes_short(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    const TIB: f64 = GIB * 1024.0;
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let units = [(KIB, "K", 0), (MIB, "M", 0), (GIB, "G", 1), (TIB, "T", 1)];
    scaled(bytes as f64, 1024.0, &units).unwrap_or_else(|| format!("{bytes}B"))
}

/// Largest unit `n` reaches, stepping up once more when rounding would print
/// the base itself (`1024K`, `1000.0Mb`).
fn scaled(n: f64, base: f64, units: &[(f64, &str, usize)]) -> Option<String> {
    let mut i = units.iter().rposition(|(unit, _, _)| n >= *unit)?;
    loop {
        let &(unit, suffix, decimals) = units.get(i)?;
        let text = format!("{:.*}", decimals, n / unit);
        let rolls_over = text.parse::<f64>().is_ok_and(|v| v >= base);
        if rolls_over && i + 1 < units.len() {
            i += 1;
            continue;
        }
        return Some(format!("{text}{suffix}"));
    }
}

#[must_use]
pub fn percent_display(ratio: f32) -> String {
    let ratio = if ratio.is_nan() {
        0.0
    } else {
        ratio.clamp(0.0, 1.0)
    };
    format!("{:.0}%", (ratio * 100.0).round())
}

#[must_use]
pub fn ready_pct(ready: bool, ratio: f32) -> String {
    if ready {
        percent_display(ratio)
    } else {
        String::from("…")
    }
}

#[must_use]
pub fn bits_per_sec(bps: u64) -> String {
    const K: f64 = 1000.0;
    const M: f64 = K * 1000.0;
    const G: f64 = M * 1000.0;
    if bps < 1000 {
        return format!("{bps}b/s");
    }
    let units = [(K, "Kb", 0), (M, "Mb", 1), (G, "Gb", 1)];
    let text = scaled(bps as f64, 1000.0, &units).unwrap_or_else(|| format!("{bps}b"));
    format!("{text}/s")
}

#[must_use]
pub fn bytes_per_sec(bps: u64) -> String {
    format!("{}/s", bytes_short(bps))
}

#[must_use]
pub fn watts_display(watts: f32) -> String {
    let tenths = format!("{watts:.1}");
    if tenths.parse::<f32>().is_ok_and(|w| w >= 10.0) {
        format!("{watts:.0}W")
    } else {
        format!("{tenths}W")
    }
}

/// How network rates are printed. Bytes by default (what Finder and Activity
/// Monitor show); bits for people who think in link speeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetUnit {
    #[default]
    Bytes,
    Bits,
}

impl NetUnit {
    #[must_use]
    pub const fn from_bits_flag(bits: bool) -> Self {
        if bits { Self::Bits } else { Self::Bytes }
    }

    /// Display value for a rate sampled in bits per second.
    #[must_use]
    pub fn from_bps(self, bits_per_sec: f64) -> f64 {
        match self {
            Self::Bytes => bits_per_sec / 8.0,
            Self::Bits => bits_per_sec,
        }
    }
}

/// A network rate sampled in bits per second: `12.4MB/s` or `99.2Mb/s`.
/// Zero prints as a bare `0` so callers can dim it.
#[must_use]
pub fn net_rate(bps: u64, unit: NetUnit) -> String {
    if bps == 0 {
        return String::from("0");
    }
    match unit {
        NetUnit::Bits => bits_per_sec(bps),
        NetUnit::Bytes => byte_rate(bps / 8),
    }
}

/// A byte rate in decimal units, like Activity Monitor: `640KB/s`, `1.2GB/s`.
/// Zero prints as a bare `0`.
#[must_use]
pub fn byte_rate(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        return String::from("0");
    }
    format!("{}/s", decimal_bytes(bytes_per_sec))
}

/// Storage size in decimal units, as Finder shows it: `494.4GB`, `1.0TB`.
#[must_use]
pub fn storage(bytes: u64) -> String {
    decimal_bytes(bytes)
}

fn decimal_bytes(bytes: u64) -> String {
    const K: f64 = 1000.0;
    const M: f64 = K * K;
    const G: f64 = M * K;
    const T: f64 = G * K;
    if bytes < 1000 {
        return format!("{bytes}B");
    }
    let units = [(K, "KB", 0), (M, "MB", 1), (G, "GB", 1), (T, "TB", 1)];
    scaled(bytes as f64, 1000.0, &units).unwrap_or_else(|| format!("{bytes}B"))
}

/// Degrees with no decimals: `78°`.
#[must_use]
pub fn celsius(c: f32) -> String {
    format!("{:.0}°", c.round())
}

/// Right-aligns `text` in `width` columns so a token keeps its place while
/// its digits change (`  5%` → ` 42%` → `100%`).
#[must_use]
pub fn fixed(text: &str, width: usize) -> String {
    format!("{text:>width$}")
}

/// Unit family an axis counts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    /// 1 000 per step (rates, °C, rpm, percent).
    Decimal,
    /// 1 024 per step (RAM).
    Binary,
}

impl Base {
    const fn step(self) -> f64 {
        match self {
            Self::Decimal => 1000.0,
            Self::Binary => 1024.0,
        }
    }
}

/// Next ceiling at or above `value` whose label reads 1, 2 or 5 × 10^j in the
/// unit `base` picks (`2MB/s`, `500K`), so axis marks never print `3.8Mb`.
#[must_use]
pub fn nice_ceiling_in(value: f64, base: Base) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    let step = base.step();
    let mut unit = 1.0;
    while value / unit >= step {
        unit *= step;
    }
    let mantissa = value / unit;
    let decade = 10f64.powf(mantissa.log10().floor());
    let n = mantissa / decade;
    let nice = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .find(|m| n <= *m * (1.0 + 1e-9))
        .unwrap_or(10.0);
    let ceiling = nice * decade;
    // `1000K` reads worse than `1M`: roll into the next unit.
    let ceiling = if ceiling >= step { step } else { ceiling };
    ceiling * unit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_scale() {
        assert_eq!(bytes_short(512), "512B");
        assert_eq!(bytes_short(2048), "2K");
        assert_eq!(bytes_short(12 * 1024 * 1024), "12M");
        assert_eq!(bytes_short(36 * 1024 * 1024 * 1024), "36.0G");
    }

    #[test]
    fn percent_rounds() {
        assert_eq!(percent_display(0.184), "18%");
        assert_eq!(percent_display(1.2), "100%");
        assert_eq!(percent_display(-0.1), "0%");
    }

    #[test]
    fn watts_precision() {
        assert_eq!(watts_display(8.24), "8.2W");
        assert_eq!(watts_display(18.7), "19W");
    }

    #[test]
    fn throughput_scale() {
        assert_eq!(bits_per_sec(800), "800b/s");
        assert_eq!(bits_per_sec(12_400_000), "12.4Mb/s");
        assert_eq!(bytes_per_sec(800), "800B/s");
        assert_eq!(bytes_per_sec(12 * 1024 * 1024), "12M/s");
    }

    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;

    #[test]
    fn bytes_short_table() {
        let cases: &[(u64, &str)] = &[
            (0, "0B"),
            (1, "1B"),
            (1023, "1023B"),
            (KIB, "1K"),
            (KIB + 511, "1K"),
            (KIB + 512, "2K"),
            (10 * KIB, "10K"),
            (999 * KIB, "999K"),
            (MIB, "1M"),
            (512 * MIB, "512M"),
            (GIB, "1.0G"),
            (GIB + GIB / 2, "1.5G"),
            (24 * GIB, "24.0G"),
            (TIB, "1.0T"),
            (3 * TIB, "3.0T"),
        ];
        for (bytes, want) in cases {
            assert_eq!(bytes_short(*bytes), *want, "{bytes} bytes");
        }
    }

    #[test]
    fn bytes_short_rounds_up_into_next_unit() {
        // Regression: 1023.6 KiB printed "1024K", one digit wider than any
        // other K value, instead of rolling into "1M".
        assert_eq!(bytes_short(MIB - 1), "1M");
        assert_eq!(bytes_short(MIB - KIB / 2), "1M");
        assert_eq!(bytes_short(GIB - 1), "1.0G");
        assert_eq!(bytes_short(TIB - 1), "1.0T");
        assert_eq!(bytes_short(1023 * KIB), "1023K");
    }

    #[test]
    fn bytes_short_never_prints_the_base() {
        for exp in 1..=4_u32 {
            let unit = 1024_u64.pow(exp);
            for delta in [1_u64, 2, 7, 100, unit / 4096 + 1, unit / 2048] {
                let text = bytes_short(unit.saturating_sub(delta));
                let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
                let whole: u64 = digits.parse().unwrap_or(0);
                assert!(whole < 1024, "{text} for {unit}-{delta}");
            }
        }
    }

    #[test]
    fn bytes_short_is_monotone_in_magnitude() {
        // Walking up by ~3% steps: the unit never steps back down.
        let rank = |s: &str| match s.chars().last() {
            Some('B') => 0,
            Some('K') => 1,
            Some('M') => 2,
            Some('G') => 3,
            Some('T') => 4,
            _ => 99,
        };
        let mut n: u64 = 1;
        let mut last = 0;
        while n < 200 * TIB {
            let text = bytes_short(n);
            let r = rank(&text);
            assert!(r >= last, "{n} -> {text}");
            assert!(text.len() <= 7, "{n} -> {text} is too wide");
            last = r;
            n = n + n / 33 + 1;
        }
    }

    #[test]
    fn bytes_short_extremes() {
        assert_eq!(bytes_short(u64::MAX), "16777216.0T");
        assert!(bytes_short(u64::MAX).ends_with('T'));
    }

    #[test]
    fn bits_per_sec_table() {
        let cases: &[(u64, &str)] = &[
            (0, "0b/s"),
            (999, "999b/s"),
            (1000, "1Kb/s"),
            (1499, "1Kb/s"),
            (1500, "2Kb/s"),
            (999_000, "999Kb/s"),
            (1_000_000, "1.0Mb/s"),
            (1_050_000, "1.1Mb/s"),
            (940_000_000, "940.0Mb/s"),
            (1_000_000_000, "1.0Gb/s"),
            (10_000_000_000, "10.0Gb/s"),
        ];
        for (bps, want) in cases {
            assert_eq!(bits_per_sec(*bps), *want, "{bps} bps");
        }
    }

    #[test]
    fn bits_per_sec_rounds_up_into_next_unit() {
        // Regression: 999.95 Mb printed "1000.0Mb"; 999.6 Kb printed "1000Kb".
        assert_eq!(bits_per_sec(999_950_000), "1.0Gb/s");
        assert_eq!(bits_per_sec(999_600), "1.0Mb/s");
        assert_eq!(bits_per_sec(999_949_999), "999.9Mb/s");
        for bps in (990_000_u64..1_000_000).step_by(37) {
            assert!(!bits_per_sec(bps).starts_with("1000"), "{bps}");
        }
    }

    #[test]
    fn bytes_per_sec_wraps_bytes_short() {
        for n in [0, 1, 1023, KIB, MIB - 1, 5 * GIB, 2 * TIB] {
            assert_eq!(bytes_per_sec(n), format!("{}/s", bytes_short(n)));
        }
    }

    #[test]
    fn percent_display_table() {
        let cases: &[(f32, &str)] = &[
            (0.0, "0%"),
            (0.004, "0%"),
            (0.005, "1%"),
            (0.5, "50%"),
            (0.994, "99%"),
            (0.996, "100%"),
            (1.0, "100%"),
            (f32::INFINITY, "100%"),
            (f32::NEG_INFINITY, "0%"),
        ];
        for (ratio, want) in cases {
            assert_eq!(percent_display(*ratio), *want, "{ratio}");
        }
    }

    #[test]
    fn percent_display_nan_is_not_printed() {
        // Regression: a 0/0 ratio printed "NaN%" because clamp keeps NaN.
        assert_eq!(percent_display(f32::NAN), "0%");
        assert_eq!(ready_pct(true, f32::NAN), "0%");
    }

    #[test]
    fn percent_display_sweep_stays_in_range() {
        for i in -200..=1200 {
            let ratio = i as f32 / 1000.0;
            let text = percent_display(ratio);
            let n: u32 = text.trim_end_matches('%').parse().unwrap_or(999);
            assert!(n <= 100, "{ratio} -> {text}");
        }
    }

    #[test]
    fn ready_pct_waits_with_an_ellipsis() {
        assert_eq!(ready_pct(false, 0.5), "…");
        assert_eq!(ready_pct(true, 0.5), "50%");
    }

    #[test]
    fn watts_rounding_boundary() {
        // Regression: 9.96 W printed "10.0W", breaking the <10 → one decimal law.
        assert_eq!(watts_display(9.96), "10W");
        assert_eq!(watts_display(9.94), "9.9W");
        assert_eq!(watts_display(0.0), "0.0W");
        assert_eq!(watts_display(10.0), "10W");
        assert_eq!(watts_display(123.4), "123W");
    }

    #[test]
    fn watts_sweep_never_prints_ten_point_zero() {
        for i in 0..20_000 {
            let w = i as f32 / 1000.0;
            let text = watts_display(w);
            assert!(!text.starts_with("10.0"), "{w} -> {text}");
            if text.contains('.') {
                assert!(w < 10.0, "{w} -> {text}");
            }
        }
    }

    #[test]
    fn every_rate_ends_in_per_second() {
        // Regression (D55): `Mb` printed without `/s` next to `0B/s`.
        let mut n: u64 = 1;
        while n < 50_000_000_000 {
            for text in [
                bits_per_sec(n),
                bytes_per_sec(n),
                byte_rate(n),
                net_rate(n * 8, NetUnit::Bytes),
                net_rate(n, NetUnit::Bits),
            ] {
                assert!(text.ends_with("/s"), "{n} -> {text}");
            }
            n = n * 3 / 2 + 1;
        }
    }

    #[test]
    fn zero_rates_are_a_bare_zero() {
        assert_eq!(byte_rate(0), "0");
        assert_eq!(net_rate(0, NetUnit::Bytes), "0");
        assert_eq!(net_rate(0, NetUnit::Bits), "0");
    }

    #[test]
    fn net_rate_units_table() {
        let cases: &[(u64, NetUnit, &str)] = &[
            (8, NetUnit::Bytes, "1B/s"),
            (7, NetUnit::Bytes, "0"),
            (8_000, NetUnit::Bytes, "1KB/s"),
            (12_400_000, NetUnit::Bytes, "1.6MB/s"),
            (1_000_000_000, NetUnit::Bytes, "125.0MB/s"),
            (8_000_000_000, NetUnit::Bytes, "1.0GB/s"),
            (12_400_000, NetUnit::Bits, "12.4Mb/s"),
            (800, NetUnit::Bits, "800b/s"),
        ];
        for (bps, unit, want) in cases {
            assert_eq!(net_rate(*bps, *unit), *want, "{bps} {unit:?}");
        }
    }

    #[test]
    fn net_unit_defaults_to_bytes() {
        assert_eq!(NetUnit::default(), NetUnit::Bytes);
        assert_eq!(NetUnit::from_bits_flag(false), NetUnit::Bytes);
        assert_eq!(NetUnit::from_bits_flag(true), NetUnit::Bits);
        assert!((NetUnit::Bytes.from_bps(8.0) - 1.0).abs() < f64::EPSILON);
        assert!((NetUnit::Bits.from_bps(8.0) - 8.0).abs() < f64::EPSILON);
    }

    #[test]
    fn storage_is_decimal_like_finder() {
        let cases: &[(u64, &str)] = &[
            (999, "999B"),
            (1_000, "1KB"),
            (999_499, "999KB"),
            (999_500, "1.0MB"),
            (494_384_795_648, "494.4GB"),
            (999_950_000_000, "1.0TB"),
            (2_000_000_000_000, "2.0TB"),
        ];
        for (bytes, want) in cases {
            assert_eq!(storage(*bytes), *want, "{bytes}");
        }
    }

    #[test]
    fn decimal_units_never_print_the_base() {
        for exp in 1..=4_u32 {
            let unit = 1000_u64.pow(exp);
            for delta in [1_u64, 2, 49, unit / 2000 + 1] {
                let text = storage(unit.saturating_sub(delta));
                assert!(!text.starts_with("1000"), "{text}");
            }
        }
    }

    #[test]
    fn celsius_rounds_whole_degrees() {
        assert_eq!(celsius(71.4), "71°");
        assert_eq!(celsius(71.5), "72°");
        assert_eq!(celsius(0.0), "0°");
        assert_eq!(celsius(104.9), "105°");
    }

    #[test]
    fn fixed_width_keeps_tokens_still() {
        for ratio in [0.0_f32, 0.05, 0.42, 1.0] {
            assert_eq!(fixed(&percent_display(ratio), 4).chars().count(), 4);
        }
        assert_eq!(fixed("5%", 4), "  5%");
        assert_eq!(fixed("12345", 3), "12345", "never truncates");
    }

    #[test]
    fn nice_ceiling_table() {
        let cases: &[(f64, Base, f64)] = &[
            (0.0, Base::Decimal, 1.0),
            (-4.0, Base::Decimal, 1.0),
            (f64::NAN, Base::Decimal, 1.0),
            (1.0, Base::Decimal, 1.0),
            (1.2, Base::Decimal, 2.0),
            (3.8, Base::Decimal, 5.0),
            (7.0, Base::Decimal, 10.0),
            (999.0, Base::Decimal, 1000.0),
            (1_500.0, Base::Decimal, 2_000.0),
            (1_900_000.0, Base::Decimal, 2_000_000.0),
            (3_800_000.0, Base::Decimal, 5_000_000.0),
            (1_000.0, Base::Binary, 1_000.0),
            (1_100.0, Base::Binary, 2_048.0),
            (3.0 * 1024.0 * 1024.0, Base::Binary, 5.0 * 1024.0 * 1024.0),
        ];
        for (v, base, want) in cases {
            let got = nice_ceiling_in(*v, *base);
            assert!((got - want).abs() < 1e-6, "{v} {base:?}: {got} != {want}");
        }
    }

    #[test]
    fn nice_ceiling_sweep_reads_one_two_five_per_unit() {
        for base in [Base::Decimal, Base::Binary] {
            let step = base.step();
            let mut v = 0.5_f64;
            while v < 1e13 {
                let c = nice_ceiling_in(v, base);
                assert!(c >= v * (1.0 - 1e-9), "{v} -> {c}");
                assert!(c <= v * 10.0 + 1.0, "{v} -> {c} overshoots");
                let mut unit = 1.0;
                while c / unit >= step {
                    unit *= step;
                }
                let m = c / unit;
                let decade = 10f64.powf(m.log10().floor());
                let lead = m / decade;
                assert!(
                    [1.0, 2.0, 2.5, 5.0].iter().any(|n| (lead - n).abs() < 1e-6),
                    "{v} {base:?} -> {c} ({lead})"
                );
                v *= 1.29;
            }
        }
    }
}
