#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Surface {
    #[default]
    Work,
    Glance,
}

impl Surface {
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Work => Self::Glance,
            Self::Glance => Self::Work,
        }
    }
}

/// Work needs room for both columns: 24 process + 36 metrics.
pub const WORK_MIN_COLS: u16 = 60;
pub const WORK_MIN_ROWS: u16 = 16;

#[must_use]
pub const fn auto_surface(cols: u16, rows: u16) -> Surface {
    if cols < WORK_MIN_COLS || rows < WORK_MIN_ROWS {
        Surface::Glance
    } else {
        Surface::Work
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_terminal_is_glance() {
        assert_eq!(auto_surface(30, 40), Surface::Glance);
        assert_eq!(auto_surface(80, 10), Surface::Glance);
        assert_eq!(auto_surface(80, 15), Surface::Glance);
    }

    #[test]
    fn typical_terminal_is_work() {
        assert_eq!(auto_surface(80, 24), Surface::Work);
        assert_eq!(auto_surface(120, 30), Surface::Work);
        assert_eq!(auto_surface(60, 16), Surface::Work);
    }

    #[test]
    fn between_old_gate_and_two_columns_is_glance() {
        // 40..59 used to pick Work and panic in the layout clamp.
        for cols in 40..60 {
            assert_eq!(auto_surface(cols, 24), Surface::Glance, "cols {cols}");
        }
    }

    #[test]
    fn other_flips() {
        assert_eq!(Surface::Work.other(), Surface::Glance);
        assert_eq!(Surface::Glance.other(), Surface::Work);
    }

    #[test]
    fn auto_surface_is_monotone_in_both_axes() {
        // Growing the terminal never demotes Work back to Glance.
        for cols in 0..=200_u16 {
            for rows in 0..=60_u16 {
                if auto_surface(cols, rows) == Surface::Work {
                    assert_eq!(auto_surface(cols + 1, rows), Surface::Work);
                    assert_eq!(auto_surface(cols, rows + 1), Surface::Work);
                }
            }
        }
    }

    #[test]
    fn auto_surface_gate_is_exactly_the_minimums() {
        assert_eq!(auto_surface(WORK_MIN_COLS, WORK_MIN_ROWS), Surface::Work);
        assert_eq!(
            auto_surface(WORK_MIN_COLS - 1, WORK_MIN_ROWS),
            Surface::Glance
        );
        assert_eq!(
            auto_surface(WORK_MIN_COLS, WORK_MIN_ROWS - 1),
            Surface::Glance
        );
        assert_eq!(auto_surface(0, 0), Surface::Glance);
        assert_eq!(auto_surface(u16::MAX, u16::MAX), Surface::Work);
        assert_eq!(auto_surface(u16::MAX, 0), Surface::Glance);
    }

    #[test]
    fn other_is_an_involution_and_default_is_work() {
        for s in [Surface::Work, Surface::Glance] {
            assert_eq!(s.other().other(), s);
            assert_ne!(s.other(), s);
        }
        assert_eq!(Surface::default(), Surface::Work);
    }
}
