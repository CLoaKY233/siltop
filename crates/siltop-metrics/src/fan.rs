//! Fan speeds over the shared SMC connection.

/// The speed to show: some SMCs report `F0Ac = 0` while the fan is already
/// being driven to `F0Tg`.
pub(crate) fn resolve_rpm(actual: u16, target: Option<u16>) -> u16 {
    match target {
        Some(target) if actual == 0 && target > 0 => target,
        _ => actual,
    }
}

/// More fan slots than any Mac has; `FNum` above this is garbage.
const MAX_FANS: u8 = 8;

/// Fan slots to probe: `0..FNum` when the SMC reports a count, otherwise
/// every `F<n>Ac` key in the key list. A fanless Mac (a `MacBook Air`) has
/// neither, so it gets no fans at all rather than a fan stuck at 0 rpm.
pub(crate) fn fan_indexes(fnum: Option<f64>, all_keys: &[u32]) -> Vec<u8> {
    let count = fnum.map_or(0, |n| n.clamp(0.0, f64::from(MAX_FANS)) as u8);
    if count > 0 {
        return (0..count).collect();
    }
    all_keys
        .iter()
        .map(|k| k.to_be_bytes())
        .filter(|b| b[0] == b'F' && b[1].is_ascii_digit() && b[2] == b'A' && b[3] == b'c')
        .map(|b| b[1] - b'0')
        .filter(|i| *i < MAX_FANS)
        .collect()
}

#[cfg(target_os = "macos")]
pub(crate) use macos::Fans;

#[cfg(target_os = "macos")]
mod macos {
    use super::{fan_indexes, resolve_rpm};
    use crate::smc::{Client, NATIVE_INTS, decode_numeric, decode_rpm, fourcc};
    use siltop_core::{FanMetric, FanSnapshot};

    struct FanKeys {
        index: u8,
        actual: u32,
        target: u32,
        max_rpm: u16,
    }

    /// The machine's fans, found once; `max_rpm` is read at discovery.
    pub(crate) struct Fans {
        fans: Vec<FanKeys>,
    }

    impl Fans {
        /// `all_keys` is the SMC key list, used when `FNum` is missing.
        pub(crate) fn discover(client: &mut Client, all_keys: &[u32]) -> Self {
            let fnum = client
                .read(fourcc(*b"FNum"))
                .and_then(|(info, bytes)| decode_numeric(info.data_type, &bytes, NATIVE_INTS));
            let fans = fan_indexes(fnum, all_keys)
                .into_iter()
                .filter_map(|index| {
                    let actual = fan_key(index, *b"Ac");
                    client.info(actual)?;
                    let max_rpm = read_rpm(client, fan_key(index, *b"Mx")).unwrap_or(0);
                    Some(FanKeys {
                        index,
                        actual,
                        target: fan_key(index, *b"Tg"),
                        max_rpm,
                    })
                })
                .collect();
            Self { fans }
        }

        pub(crate) fn is_empty(&self) -> bool {
            self.fans.is_empty()
        }

        /// `None` when fans exist but none could be read.
        pub(crate) fn sample(&self, client: &mut Client) -> Option<FanSnapshot> {
            let fans: Vec<FanMetric> = self
                .fans
                .iter()
                .filter_map(|fan| {
                    let actual = read_rpm(client, fan.actual)?;
                    let target = read_rpm(client, fan.target);
                    Some(FanMetric {
                        name: format!("Fan {}", fan.index + 1),
                        rpm: resolve_rpm(actual, target),
                        max_rpm: fan.max_rpm,
                    })
                })
                .collect();
            (!fans.is_empty() || self.fans.is_empty()).then_some(FanSnapshot { fans })
        }
    }

    fn read_rpm(client: &mut Client, key: u32) -> Option<u16> {
        let (info, bytes) = client.read(key)?;
        decode_rpm(info, &bytes)
    }

    fn fan_key(index: u8, suffix: [u8; 2]) -> u32 {
        let digit = b'0'.saturating_add(index);
        fourcc([b'F', digit, suffix[0], suffix[1]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&[u8; 4]]) -> Vec<u32> {
        names.iter().map(|n| u32::from_be_bytes(**n)).collect()
    }

    #[test]
    fn fanless_macbook_air_has_no_fan_slots() {
        // No FNum (or FNum = 0) and no F?Ac keys: fans absent, never 0 rpm.
        let air = keys(&[b"TC0P", b"PSTR", b"B0AC", b"FNum"]);
        assert!(fan_indexes(None, &air).is_empty());
        assert!(fan_indexes(Some(0.0), &air).is_empty());
    }

    #[test]
    fn fan_count_comes_from_fnum_on_every_fanned_shape() {
        let cases: [(&str, f64, usize); 4] = [
            ("Mac mini", 1.0, 1),
            ("MacBook Pro 14/16", 2.0, 2),
            ("Mac Studio", 1.0, 1),
            ("Mac Pro", 3.0, 3),
        ];
        for (shape, fnum, want) in cases {
            assert_eq!(fan_indexes(Some(fnum), &[]).len(), want, "{shape}");
        }
    }

    #[test]
    fn missing_fnum_falls_back_to_the_key_list() {
        let all = keys(&[b"F0Ac", b"F1Ac", b"F0Tg", b"F0Mx", b"FNum"]);
        assert_eq!(fan_indexes(None, &all), [0, 1]);
    }

    #[test]
    fn garbage_fan_keys_and_counts_are_bounded() {
        let all = keys(&[b"FzAc", b"F9Ac", b"F.Ac", b"F7Ac"]);
        assert_eq!(fan_indexes(None, &all), [7], "only digit slots below 8");
        assert_eq!(fan_indexes(Some(1e9), &[]).len(), 8, "clamped");
        assert!(fan_indexes(Some(-3.0), &[]).is_empty());
        assert!(fan_indexes(Some(f64::NAN), &[]).is_empty());
    }

    #[test]
    fn target_stands_in_for_a_zero_actual() {
        assert_eq!(resolve_rpm(0, Some(2300)), 2300);
        assert_eq!(resolve_rpm(0, Some(0)), 0);
        assert_eq!(resolve_rpm(0, None), 0);
        assert_eq!(resolve_rpm(1850, Some(2300)), 1850);
    }
}
