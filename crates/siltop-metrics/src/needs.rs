//! What the visible UI needs from the costly background samplers.
//!
//! The app computes a [`Needs`] from what is on screen and hands it to the
//! worker over its command channel; the `IOReport` thread stops sampling
//! what nobody is looking at. Held readings stand in while a source is
//! parked (see [`crate::Sampler::set_needs`]).

use std::ops::{BitOr, BitOrAssign};

/// A small bit set of the sources the visible views read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Needs(u8);

impl Needs {
    /// Nothing on screen reads clocks or watts (a full-screen page).
    pub const NONE: Self = Self(0);
    /// CPU residency: cluster and core MHz, and frequency-scaled CPU usage.
    pub const CPU_CLOCKS: Self = Self(1);
    /// GPU residency (GPUPH): GPU MHz and frequency-scaled GPU usage.
    pub const GPU_CLOCKS: Self = Self(1 << 1);
    /// Energy rails: CPU, GPU, ANE and DRAM watts.
    pub const POWER: Self = Self(1 << 2);
    /// Everything: the default until the UI says otherwise.
    pub const ALL: Self = Self(0b111);

    /// Every bit of `other` is set here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Any bit of `other` is set here.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Bits `self` has that `before` did not: the sources just re-enabled.
    #[must_use]
    pub const fn added_since(self, before: Self) -> Self {
        Self(self.0 & !before.0)
    }

    /// Every bit of either: `|` for constants.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) const fn bits(self) -> u8 {
        self.0
    }

    /// Unknown bits are dropped.
    pub(crate) const fn from_bits(bits: u8) -> Self {
        Self(bits & Self::ALL.0)
    }
}

impl Default for Needs {
    fn default() -> Self {
        Self::ALL
    }
}

impl BitOr for Needs {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for Needs {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_needs_everything() {
        assert_eq!(Needs::default(), Needs::ALL);
        assert!(Needs::ALL.contains(Needs::CPU_CLOCKS | Needs::GPU_CLOCKS | Needs::POWER));
    }

    #[test]
    fn none_contains_nothing_but_itself() {
        assert!(!Needs::NONE.intersects(Needs::ALL));
        assert!(Needs::NONE.contains(Needs::NONE));
    }

    #[test]
    fn intersects_needs_one_shared_bit() {
        let pmgr = Needs::CPU_CLOCKS | Needs::POWER;
        assert!(Needs::POWER.intersects(pmgr));
        assert!(!Needs::GPU_CLOCKS.intersects(pmgr));
    }

    #[test]
    fn added_since_reports_only_new_bits() {
        let before = Needs::POWER;
        let now = Needs::POWER | Needs::CPU_CLOCKS;
        assert_eq!(now.added_since(before), Needs::CPU_CLOCKS);
        assert_eq!(before.added_since(now), Needs::NONE);
    }

    #[test]
    fn bits_round_trip_and_drop_unknown() {
        for needs in [Needs::NONE, Needs::CPU_CLOCKS, Needs::POWER, Needs::ALL] {
            assert_eq!(Needs::from_bits(needs.bits()), needs);
        }
        assert_eq!(Needs::from_bits(0xff), Needs::ALL);
    }

    #[test]
    fn bitor_assign_accumulates() {
        let mut needs = Needs::NONE;
        needs |= Needs::GPU_CLOCKS;
        needs |= Needs::POWER;
        assert!(needs.contains(Needs::GPU_CLOCKS | Needs::POWER));
        assert!(!needs.contains(Needs::CPU_CLOCKS));
    }
}
