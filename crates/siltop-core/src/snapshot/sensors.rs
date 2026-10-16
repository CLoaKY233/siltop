use super::ClusterKind;

/// The macOS thermal state. Declared mildest first, so `Ord` is severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Thermal {
    #[default]
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl Thermal {
    /// Every state, mildest first.
    pub const ALL: [Self; 4] = [Self::Nominal, Self::Fair, Self::Serious, Self::Critical];

    #[must_use]
    pub const fn is_nominal(self) -> bool {
        matches!(self, Self::Nominal)
    }

    /// The one spelling everywhere: JSON, alert rules, history events.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Fair => "fair",
            Self::Serious => "serious",
            Self::Critical => "critical",
        }
    }

    /// The word a panel shows; nominal is never worth a word.
    #[must_use]
    pub const fn word(self) -> Option<&'static str> {
        if self.is_nominal() {
            None
        } else {
            Some(self.name())
        }
    }

    /// 0 nominal, 1 fair, 2 serious, 3 critical: the number history,
    /// Prometheus and the status line store.
    #[must_use]
    pub const fn level(self) -> u8 {
        self as u8
    }

    /// The state for a stored [`Self::level`]; anything above 3 is critical.
    #[must_use]
    pub const fn from_level(level: u8) -> Self {
        match level {
            0 => Self::Nominal,
            1 => Self::Fair,
            2 => Self::Serious,
            _ => Self::Critical,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanMetric {
    pub name: String,
    pub rpm: u16,
    pub max_rpm: u16,
}

impl FanMetric {
    #[must_use]
    pub fn ratio(&self) -> f32 {
        if self.max_rpm == 0 {
            0.0
        } else {
            f32::from(self.rpm) / f32::from(self.max_rpm)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TempReading {
    pub name: String,
    pub celsius: f32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SensorsSnapshot {
    pub cpu_c: Option<f32>,
    pub gpu_c: Option<f32>,
    pub hotspot_c: Option<f32>,
    pub e_c: Option<f32>,
    pub p_c: Option<f32>,
    pub s_c: Option<f32>,
    pub readings: Vec<TempReading>,
}

impl SensorsSnapshot {
    #[must_use]
    pub fn is_present(&self) -> bool {
        self.cpu_c.is_some()
            || self.gpu_c.is_some()
            || self.e_c.is_some()
            || self.p_c.is_some()
            || self.s_c.is_some()
            || !self.readings.is_empty()
    }

    #[must_use]
    pub fn zone_temp(&self, kind: ClusterKind) -> Option<f32> {
        match kind {
            ClusterKind::Efficiency => self.e_c,
            ClusterKind::Performance => self.p_c,
            ClusterKind::Super => self.s_c,
        }
    }

    #[must_use]
    pub fn best_cpu_c(&self) -> Option<f32> {
        self.cpu_c
            .or_else(|| mean_opts(&[self.e_c, self.p_c, self.s_c]))
            .or(self.hotspot_c)
    }

    #[must_use]
    pub fn named_gpu_c(&self) -> Option<f32> {
        self.gpu_c.or_else(|| {
            self.readings.iter().find_map(|r| {
                r.name
                    .as_bytes()
                    .windows(3)
                    .any(|w| w.eq_ignore_ascii_case(b"gpu"))
                    .then_some(r.celsius)
            })
        })
    }

    pub fn set_zone_temp(&mut self, kind: ClusterKind, temp: f32) {
        match kind {
            ClusterKind::Efficiency => self.e_c = Some(temp),
            ClusterKind::Performance => self.p_c = Some(temp),
            ClusterKind::Super => self.s_c = Some(temp),
        }
    }
}

fn mean_opts(values: &[Option<f32>]) -> Option<f32> {
    let mut sum = 0.0_f32;
    let mut n = 0_u32;
    for value in values.iter().flatten() {
        sum += *value;
        n += 1;
    }
    if n == 0 { None } else { Some(sum / n as f32) }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FanSnapshot {
    pub fans: Vec<FanMetric>,
}

impl FanSnapshot {
    #[must_use]
    pub fn is_present(&self) -> bool {
        self.fans.iter().any(|f| f.rpm > 0 || f.max_rpm > 0)
    }

    #[must_use]
    pub fn peak_ratio(&self) -> f32 {
        self.fans
            .iter()
            .map(FanMetric::ratio)
            .fold(0.0_f32, f32::max)
    }

    /// Fans exist but none is turning: show "fans idle", not `0 rpm`.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        !self.fans.is_empty() && self.fans.iter().all(|f| f.rpm == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanless_is_absent() {
        assert!(!FanSnapshot::default().is_present());
        let fans = FanSnapshot {
            fans: vec![FanMetric {
                name: String::from("Fan 1"),
                rpm: 1800,
                max_rpm: 6000,
            }],
        };
        assert!(fans.is_present());
        assert!(!fans.is_idle());
        assert!((fans.peak_ratio() - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn stopped_fans_are_idle_not_absent() {
        let fans = FanSnapshot {
            fans: vec![FanMetric {
                name: String::from("Fan 1"),
                rpm: 0,
                max_rpm: 5349,
            }],
        };
        assert!(fans.is_present());
        assert!(fans.is_idle());
        assert!(!FanSnapshot::default().is_idle());
    }

    #[test]
    fn zone_temp_and_best_cpu() {
        let mut sensors = SensorsSnapshot {
            e_c: Some(36.0),
            p_c: Some(48.0),
            ..SensorsSnapshot::default()
        };
        assert_eq!(sensors.zone_temp(ClusterKind::Efficiency), Some(36.0));
        assert_eq!(sensors.zone_temp(ClusterKind::Super), None);
        let best = sensors.best_cpu_c().unwrap_or(0.0);
        assert!((best - 42.0).abs() < f32::EPSILON);
        sensors.cpu_c = Some(40.0);
        assert_eq!(sensors.best_cpu_c(), Some(40.0));
    }

    #[test]
    fn named_gpu_c_uses_reading_when_field_absent() {
        let mut sensors = SensorsSnapshot {
            gpu_c: None,
            readings: vec![TempReading {
                name: String::from("GPU die"),
                celsius: 44.0,
            }],
            ..SensorsSnapshot::default()
        };
        assert_eq!(sensors.named_gpu_c(), Some(44.0));
        sensors.gpu_c = Some(51.0);
        assert_eq!(sensors.named_gpu_c(), Some(51.0));
    }

    #[test]
    fn thermal_word_is_none_when_nominal() {
        assert_eq!(Thermal::Nominal.word(), None);
        assert_eq!(Thermal::Fair.word(), Some("fair"));
    }

    #[test]
    fn thermal_order_level_and_name_agree() {
        for (i, t) in Thermal::ALL.into_iter().enumerate() {
            assert_eq!(usize::from(t.level()), i);
            assert_eq!(Thermal::from_level(t.level()), t);
        }
        assert!(Thermal::ALL.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(Thermal::from_level(9), Thermal::Critical);
        assert_eq!(Thermal::Nominal.name(), "nominal");
    }
}
