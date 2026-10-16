//! A calendar day in UTC, for the history store's day files and queries.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// A calendar day in UTC, stored as days since 1970-01-01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day(i32);

impl Day {
    /// `None` for a day that does not exist (2026-02-29, month 13, ...).
    #[must_use]
    pub fn from_ymd(year: i32, month: u32, day: u32) -> Option<Self> {
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        // Howard Hinnant's days_from_civil, with March as the first month.
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let mp = (month + 9) % 12;
        let doy = (153 * mp + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as i32;
        Some(Self(era * 146_097 + doe - 719_468))
    }

    /// The day `days` after 1970-01-01 (negative is before).
    #[must_use]
    pub const fn from_days(days: i32) -> Self {
        Self(days)
    }

    /// Strict `YYYY-MM-DD`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let shape_ok = bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
        if !shape_ok {
            return None;
        }
        let year = text[0..4].parse().ok()?;
        let month = text[5..7].parse().ok()?;
        let day = text[8..10].parse().ok()?;
        Self::from_ymd(year, month, day)
    }

    /// The UTC day the system clock reads now.
    #[must_use]
    pub fn today() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        Self(i32::try_from(secs / 86_400).unwrap_or(i32::MAX))
    }

    #[must_use]
    pub const fn days_since_epoch(self) -> i32 {
        self.0
    }

    /// `(year, month, day)`, the inverse of [`Day::from_ymd`].
    #[must_use]
    pub const fn ymd(self) -> (i32, u32, u32) {
        let z = self.0 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
        (year, month, day)
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = self.ymd();
        write!(f, "{year:04}-{month:02}-{day:02}")
    }
}

const fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn day_epoch_and_known_dates() {
        let table: &[(&str, i32)] = &[
            ("1970-01-01", 0),
            ("1970-01-02", 1),
            ("1969-12-31", -1),
            ("2000-03-01", 11_017),
            ("2026-09-23", 20_719),
        ];
        for (text, days) in table {
            let day = Day::parse(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(day.days_since_epoch(), *days, "{text}");
            assert_eq!(day.to_string(), *text);
        }
    }

    #[test]
    fn day_rejects_malformed_and_impossible_dates() {
        for text in [
            "2026-02-29",
            "2100-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-04-31",
            "2026-09-00",
            "2026-9-23",
            "26-09-23",
            "2026/09/23",
            "2026-09-23T00:00",
            "+026-09-23",
            "２026-09-23",
            "",
        ] {
            assert_eq!(Day::parse(text), None, "{text:?}");
        }
        for text in ["2024-02-29", "2000-02-29", "2026-12-31"] {
            assert!(Day::parse(text).is_some(), "{text}");
        }
    }

    /// Property sweep: every day across 1900..2200 round-trips and is
    /// exactly one after the previous.
    #[test]
    fn day_round_trips_every_day_for_three_centuries() {
        let start = Day::from_ymd(1900, 1, 1).unwrap();
        let end = Day::from_ymd(2200, 1, 1).unwrap();
        let mut previous = start.days_since_epoch() - 1;
        for n in start.days_since_epoch()..end.days_since_epoch() {
            let (y, m, d) = Day(n).ymd();
            let back = Day::from_ymd(y, m, d).unwrap();
            assert_eq!(back.days_since_epoch(), n);
            assert_eq!(back.days_since_epoch(), previous + 1);
            assert_eq!(Day::parse(&back.to_string()), Some(back));
            previous = n;
        }
    }

    #[test]
    fn today_is_after_this_code_was_written() {
        assert!(Day::today() >= Day::from_ymd(2026, 9, 23).unwrap());
    }
}
