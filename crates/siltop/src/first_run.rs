//! The very first launch: a quiet welcome toast, then one line in the
//! footer's verdict slot naming the keys worth knowing, spelled the way the
//! active key profile binds them. It goes away
//! after a few seconds or on the first key, and leaves a config file behind
//! so it never shows again.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use siltop_core::{Verdict, VerdictKind};

use crate::keymap::{Action, Mode, Profile, bindings};

pub const WELCOME: &str = "welcome to siltop";
/// The default profile's hint; [`hint`] builds it for any profile.
#[cfg(test)]
pub const HINT: &str = "m menu · ? keys · T themes";

/// The hint for `profile`: each key it names is that profile's own first
/// binding, and a feature the profile does not bind is left out, so the
/// hint never points at a key that does something else.
#[must_use]
pub fn hint(profile: Profile) -> String {
    [
        (Action::Menu, "menu"),
        (Action::Help, "keys"),
        (Action::CycleTheme, "themes"),
    ]
    .into_iter()
    .filter_map(|(action, word)| {
        bindings(Mode::Normal, profile)
            .find(|binding| binding.action == action)
            .and_then(|binding| binding.keys.first())
            .map(|key| format!("{} {word}", key.label()))
    })
    .collect::<Vec<_>>()
    .join(" · ")
}
/// The welcome toast owns the footer slot first...
pub const WELCOME_TTL: Duration = Duration::from_millis(2500);
/// ...then the key hint shows for this long.
pub const HINT_TTL: Duration = Duration::from_secs(8);

/// What a sealed config file starts with.
const HEADER: &str = "\
# siltop preferences. Changes you make in the app are saved here.
# `siltop --print-config` shows every key with its current value.
";

#[derive(Debug, Clone, Default)]
pub struct FirstRun {
    /// When the hint stops showing; `None` once it is gone (or never was).
    until: Option<Instant>,
    hint: Option<Verdict>,
}

impl FirstRun {
    /// A first run is a live session whose config file does not exist yet.
    /// `--demo` is a showcase, not this user's first day, so it never counts.
    #[must_use]
    pub fn detect(config: Option<&Path>, demo: bool, now: Instant) -> Self {
        let first = !demo && config.is_some_and(|path| !path.exists());
        if !first {
            return Self::default();
        }
        Self {
            until: Some(now + WELCOME_TTL + HINT_TTL),
            hint: Some(Verdict {
                kind: VerdictKind::Normal,
                headline: hint(Profile::default()),
                evidence: Vec::new(),
                pid: None,
            }),
        }
    }

    /// Re-spells the hint for the key profile the session actually uses.
    pub fn use_profile(&mut self, profile: Profile) {
        if let Some(verdict) = self.hint.as_mut() {
            verdict.headline = hint(profile);
        }
    }

    /// Whether this session started as a first run (even after the hint went).
    #[must_use]
    pub fn detected(&self) -> bool {
        self.hint.is_some()
    }

    /// The hint while it is due, in place of the verdict.
    #[must_use]
    pub fn shown(&self, now: Instant) -> Option<&Verdict> {
        self.until
            .filter(|until| now < *until)
            .and(self.hint.as_ref())
    }

    /// When the loop must wake to take the hint down.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.until
    }

    /// A key was pressed or the time ran out: hide the hint for good.
    /// Returns whether anything visible changed.
    pub fn dismiss(&mut self) -> bool {
        self.until.take().is_some()
    }

    /// Drops the hint once its time is up; true when that happened now.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.until.is_some_and(|until| now >= until) {
            self.until = None;
            return true;
        }
        false
    }
}

/// Leaves a (comment-only) config file at `path` so the next launch is not
/// a first run. An existing file is never touched.
pub fn seal(path: &Path) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    crate::config_io::atomic_write(path, HEADER)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config_io::tests::TempDir;

    #[test]
    fn first_run_hint_shows_once_and_never_after_config_exists() {
        let dir = TempDir::new("first-run-once");
        let path = dir.file();
        let now = Instant::now();
        let first = FirstRun::detect(Some(&path), false, now);
        assert!(first.detected());
        assert_eq!(first.shown(now).map(|v| v.headline.as_str()), Some(HINT));
        seal(&path).unwrap();
        let second = FirstRun::detect(Some(&path), false, now);
        assert!(!second.detected());
        assert!(second.shown(now).is_none());
        assert_eq!(second.deadline(), None, "no wake-ups on later launches");
    }

    #[test]
    fn sealed_file_loads_as_default_preferences() {
        let dir = TempDir::new("first-run-seal");
        let path = dir.file();
        seal(&path).unwrap();
        let loaded = siltop_core::Config::load(&path);
        assert_eq!(loaded, siltop_core::Config::default());
    }

    #[test]
    fn seal_never_overwrites_an_existing_config() {
        let dir = TempDir::new("first-run-keep");
        let path = dir.file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "interval_ms = 2000\n").unwrap();
        seal(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "interval_ms = 2000\n"
        );
    }

    #[test]
    fn hint_hides_on_the_first_key_and_stays_hidden() {
        let now = Instant::now();
        let mut run = FirstRun::detect(Some(Path::new("/nonexistent/p/config.toml")), false, now);
        assert!(run.dismiss(), "the first key changes the footer");
        assert!(run.shown(now).is_none());
        assert!(!run.dismiss(), "later keys are no-ops");
        assert!(run.detected(), "still sealed at exit");
    }

    #[test]
    fn hint_expires_after_welcome_plus_hint_time() {
        let now = Instant::now();
        let mut run = FirstRun::detect(Some(Path::new("/nonexistent/p/config.toml")), false, now);
        let end = now + WELCOME_TTL + HINT_TTL;
        assert_eq!(run.deadline(), Some(end));
        let before = end.checked_sub(Duration::from_millis(1)).unwrap();
        assert!(run.shown(before).is_some());
        assert!(!run.expire(before));
        assert!(run.expire(end));
        assert!(run.shown(end).is_none());
        assert_eq!(run.deadline(), None);
    }

    #[test]
    fn demo_and_pathless_sessions_are_never_first_runs() {
        let now = Instant::now();
        let missing = Path::new("/nonexistent/p/config.toml");
        assert!(!FirstRun::detect(Some(missing), true, now).detected());
        assert!(!FirstRun::detect(None, false, now).detected());
    }

    #[test]
    fn hint_names_each_profiles_own_keys() {
        use crate::keymap::resolve;
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        assert_eq!(hint(Profile::Siltop), HINT);
        // htop's T sorts; its hint must not promise themes on T.
        let htop = hint(Profile::Htop);
        assert!(!htop.contains("T "), "{htop}");
        assert_eq!(
            resolve(KeyEvent::from(KeyCode::F(1)), Mode::Normal, Profile::Htop),
            Some(Action::Help)
        );
        assert_eq!(htop, "m menu · F1 keys");
        assert_eq!(hint(Profile::Btop), "m menu · ? keys · T themes");
        let now = Instant::now();
        let mut run = FirstRun::detect(Some(Path::new("/nonexistent/p/config.toml")), false, now);
        run.use_profile(Profile::Htop);
        assert_eq!(
            run.shown(now).map(|v| v.headline.as_str()),
            Some(htop.as_str())
        );
    }

    #[test]
    fn hint_names_the_real_keys() {
        use crate::keymap::resolve;
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        let key = |c| {
            resolve(
                KeyEvent::from(KeyCode::Char(c)),
                Mode::Normal,
                Profile::Siltop,
            )
        };
        assert_eq!(key('m'), Some(Action::Menu));
        assert_eq!(key('?'), Some(Action::Help));
        assert_eq!(key('T'), Some(Action::CycleTheme));
        for c in ['m', '?', 'T'] {
            assert!(
                HINT.starts_with(&format!("{c} ")) || HINT.contains(&format!(" {c} ")),
                "{c}"
            );
        }
    }

    #[test]
    fn hint_fits_the_footer_at_a_normal_width() {
        // The verdict slot only shows a headline that fits whole; keep the
        // hint short enough for the gap between chips and a laptop's longer
        // battery status at 120 columns.
        for profile in Profile::ALL {
            let text = hint(profile);
            assert!(text.chars().count() <= 28, "{profile:?}: {text}");
        }
    }
}
