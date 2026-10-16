//! The config file on disk: load once, then save the settings that changed
//! one second after the last change, atomically, keeping the user's
//! comments, formatting and unknown keys. A file that does not parse is
//! backed up and never written; a read-only file is never written.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use siltop_core::{Config, Surface};
use toml_edit::DocumentMut;

/// Quiet period after the last change before the file is written.
pub const SAVE_DEBOUNCE: Duration = Duration::from_secs(1);
pub const CONFIG_ERROR: &str = "config error, using defaults (not saved)";
pub const CONFIG_READ_ONLY: &str = "config is read-only (not saved)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Writable,
    /// The file (or its directory) refuses writes.
    ReadOnly,
    /// The file did not parse; it is left exactly as it was.
    Broken,
    /// No config location (no `HOME`): settings live for the session only.
    Disabled,
}

#[derive(Debug)]
pub struct ConfigStore {
    path: PathBuf,
    doc: DocumentMut,
    /// The settings as they stand on disk.
    saved: Config,
    due: Option<Instant>,
    mode: Mode,
    warned_read_only: bool,
}

/// A setting the command line shadows for this run: what the file says and
/// what the flag said.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shadow<T> {
    file: T,
    cli: T,
}

impl<T: PartialEq + Clone> Shadow<T> {
    /// Only a flag that actually changed the value leaves a shadow.
    fn between(file: &T, session: &T) -> Option<Self> {
        (file != session).then(|| Self {
            file: file.clone(),
            cli: session.clone(),
        })
    }

    /// Still the flag's value: write the file's back. Changed in-session:
    /// the user chose it, so it persists.
    fn unshadow(shadow: Option<&Self>, current: &mut T) {
        if let Some(shadow) = shadow
            && *current == shadow.cli
        {
            current.clone_from(&shadow.file);
        }
    }
}

/// `--interval`, `--surface` and `--theme` hold for one run only: the
/// config written back never contains them unless the user changed that
/// setting again during the session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionOverrides {
    interval: Option<Shadow<Duration>>,
    surface: Option<Shadow<Option<Surface>>>,
    theme: Option<Shadow<String>>,
    /// `SILTOP_KEYS`, or a profile alias spelled canonically.
    keymap: Option<Shadow<String>>,
}

impl SessionOverrides {
    /// The shadows `session` (the file's config with flags applied) casts
    /// over `file`.
    #[must_use]
    pub fn between(file: &Config, session: &Config) -> Self {
        Self {
            interval: Shadow::between(&file.interval, &session.interval),
            surface: Shadow::between(&file.surface, &session.surface),
            theme: Shadow::between(&file.theme, &session.theme),
            keymap: Shadow::between(&file.keymap, &session.keymap),
        }
    }

    /// `current` as it should be persisted.
    #[must_use]
    pub fn persistable(&self, mut current: Config) -> Config {
        Shadow::unshadow(self.interval.as_ref(), &mut current.interval);
        Shadow::unshadow(self.surface.as_ref(), &mut current.surface);
        Shadow::unshadow(self.theme.as_ref(), &mut current.theme);
        Shadow::unshadow(self.keymap.as_ref(), &mut current.keymap);
        current
    }
}

/// `$XDG_CONFIG_HOME/siltop/config.toml`, else `~/.config/...`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("siltop").join("config.toml"))
}

/// `config.toml` → `config.toml.<suffix>`, next to the file.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

/// Writes through a symlink to its target, so a dotfiles link survives.
fn resolve(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_read_only(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    )
}

/// Writes `text` to `path.tmp`, syncs it, then renames it over `path`, so
/// a crash leaves either the old file or the new one, never half of each.
pub fn atomic_write(path: &Path, text: &str) -> io::Result<()> {
    write_via_tmp(path, text, false)
}

/// [`atomic_write`] for a file only its owner may read (the agent's
/// state): always left 0600, even when an older copy was wider,
/// because a home directory is often readable by every local account.
pub fn atomic_write_private(path: &Path, text: &str) -> io::Result<()> {
    write_via_tmp(path, text, true)
}

fn write_via_tmp(path: &Path, text: &str, private: bool) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let tmp = sibling(path, "tmp");
    let written = (|| {
        let mut file = fs::File::create(&tmp)?;
        if private {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
        } else if let Ok(meta) = fs::metadata(path) {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        io::Write::write_all(&mut file, text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

impl ConfigStore {
    /// A store that never touches the disk.
    pub fn disabled() -> (Self, Config) {
        let store = Self {
            path: PathBuf::new(),
            doc: DocumentMut::new(),
            saved: Config::default(),
            due: None,
            mode: Mode::Disabled,
            warned_read_only: false,
        };
        (store, Config::default())
    }

    /// Loads `path`. The warning, if any, is for the status line.
    pub fn open(path: &Path) -> (Self, Config, Option<String>) {
        let path = resolve(path);
        let mut store = Self {
            path,
            doc: DocumentMut::new(),
            saved: Config::default(),
            due: None,
            mode: Mode::Writable,
            warned_read_only: false,
        };
        let text = match fs::read(&store.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return (store, Config::default(), None);
            }
            Err(err) => {
                store.mode = Mode::ReadOnly;
                return (
                    store,
                    Config::default(),
                    Some(format!("config unreadable, using defaults: {err}")),
                );
            }
        };
        if fs::metadata(&store.path).is_ok_and(|meta| meta.permissions().readonly()) {
            store.mode = Mode::ReadOnly;
        }
        let parsed = String::from_utf8(text)
            .map_err(|err| err.to_string())
            .and_then(|text| Config::from_toml(&text).map_err(|err| err.to_string()));
        let Ok(loaded) = parsed else {
            store.mode = Mode::Broken;
            let _ = fs::copy(&store.path, sibling(&store.path, "bak"));
            return (store, Config::default(), Some(String::from(CONFIG_ERROR)));
        };
        store.doc = loaded.doc;
        store.saved = loaded.config.clone();
        let warning = loaded.warnings.first().map(|first| {
            let more = loaded.warnings.len() - 1;
            if more == 0 {
                format!("config: {first}")
            } else {
                format!("config: {first} (+{more} more)")
            }
        });
        (store, loaded.config, warning)
    }

    /// A setting changed: (re)start the debounce.
    pub fn mark(&mut self, now: Instant) {
        if matches!(self.mode, Mode::Writable | Mode::ReadOnly) {
            self.due = Some(now + SAVE_DEBOUNCE);
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.due
    }

    /// Saves when the debounce has run out.
    pub fn poll(&mut self, now: Instant, config: &Config) -> Option<String> {
        if self.due.is_some_and(|at| now >= at) {
            self.flush(config)
        } else {
            None
        }
    }

    /// Saves now if anything changed since the last save. Changes are laid
    /// over a fresh read of the file, so an edit made by hand while the app
    /// runs is kept.
    pub fn flush(&mut self, config: &Config) -> Option<String> {
        self.due = None;
        match self.mode {
            Mode::Writable => {}
            Mode::ReadOnly => return self.read_only_once(config),
            Mode::Broken | Mode::Disabled => return None,
        }
        if *config == self.saved {
            return None;
        }
        if fs::metadata(&self.path).is_ok_and(|meta| meta.permissions().readonly()) {
            self.mode = Mode::ReadOnly;
            return self.read_only_once(config);
        }
        let fresh = match fs::read_to_string(&self.path) {
            Ok(text) => text.parse::<DocumentMut>().ok(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Some(DocumentMut::new()),
            Err(_) => Some(self.doc.clone()),
        };
        let Some(mut doc) = fresh else {
            self.mode = Mode::Broken;
            let _ = fs::copy(&self.path, sibling(&self.path, "bak"));
            return Some(String::from(CONFIG_ERROR));
        };
        // A comment-only file (the sealed first-run header) keeps its
        // comments as the document's trailing text, which would print after
        // every table written now. Keep them at the top instead.
        let header = if doc.is_empty() {
            let text = doc.trailing().as_str().unwrap_or_default().to_owned();
            doc.set_trailing("");
            text
        } else {
            String::new()
        };
        config.write_changes(&self.saved, &mut doc);
        let text = format!("{header}{doc}");
        match atomic_write(&self.path, &text) {
            Ok(()) => {
                self.doc = text.parse().unwrap_or(doc);
                self.saved = config.clone();
                None
            }
            Err(err) if is_read_only(&err) => {
                self.mode = Mode::ReadOnly;
                self.read_only_once(config)
            }
            Err(err) => Some(format!("could not save settings: {err}")),
        }
    }

    fn read_only_once(&mut self, config: &Config) -> Option<String> {
        if self.warned_read_only || *config == self.saved {
            return None;
        }
        self.warned_read_only = true;
        Some(String::from(CONFIG_READ_ONLY))
    }

    #[cfg(test)]
    pub fn is_broken(&self) -> bool {
        self.mode == Mode::Broken
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub mod tests {
    use super::*;
    use crate::app::running_as_root;
    use siltop_core::{ProcSort, Surface};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const CORRUPT: &[u8] = include_bytes!("../../siltop-core/src/config/fixtures/corrupt.toml");
    const LEGACY: &str = "interval_ms = 1000\nsurface = \"work\"\nshow_gpu = true\nshow_net = false\nshow_disk = true\nshow_fans = true\nshow_cores = true\nshow_threads = false\nshow_tree = false\nsort = \"cpu\"\nproc_ratio = 55\n";

    /// A fresh directory per test, removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(name: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "siltop-cfgio-{}-{name}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub fn file(&self) -> PathBuf {
            self.0.join("siltop").join("config.toml")
        }

        fn write(&self, bytes: &[u8]) -> PathBuf {
            let path = self.file();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.0.join("siltop"), fs::Permissions::from_mode(0o755));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn toggled(cfg: &Config) -> Config {
        Config {
            show_gpu: !cfg.show_gpu,
            ..cfg.clone()
        }
    }

    #[test]
    fn missing_file_loads_defaults_and_is_not_created_without_changes() {
        let tmp = TempDir::new("missing");
        let (mut store, cfg, warning) = ConfigStore::open(&tmp.file());
        assert_eq!(cfg, Config::default());
        assert!(warning.is_none());
        assert!(store.flush(&cfg).is_none());
        assert!(!tmp.file().exists(), "nothing changed, nothing written");
    }

    #[test]
    fn first_change_creates_dir_and_writes_only_that_key() {
        let tmp = TempDir::new("first");
        let (mut store, cfg, _) = ConfigStore::open(&tmp.file());
        let next = Config {
            show_disk: true,
            ..cfg
        };
        assert!(store.flush(&next).is_none());
        let text = fs::read_to_string(tmp.file()).unwrap();
        assert!(text.contains("disk = true"), "{text}");
        assert!(text.contains("version = 1"));
        assert!(!text.contains("interval_ms"), "{text}");
        assert!(!text.contains("gpu"), "{text}");
        assert_eq!(ConfigStore::open(&tmp.file()).1, next);
    }

    #[test]
    fn atomic_write_leaves_no_tmp_and_replaces_whole_file() {
        let tmp = TempDir::new("atomic");
        let path = tmp.write(b"old = 1\n");
        atomic_write(&path, "new = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new = 2\n");
        assert!(!sibling(&path, "tmp").exists());
        let entries = fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(entries, 1, "only config.toml remains");
    }

    #[test]
    fn atomic_write_keeps_file_mode() {
        let tmp = TempDir::new("mode");
        let path = tmp.write(b"x = 1\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        atomic_write(&path, "x = 2\n").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn atomic_write_private_leaves_the_file_readable_by_its_owner_only() {
        // Regression: agent.json was created 0644, so any local account
        // could read it.
        let tmp = TempDir::new("private");
        let fresh = tmp.file().with_file_name("agent.json");
        atomic_write_private(&fresh, "k = 1\n").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&fresh), 0o600);
        let wide = tmp.write(b"x = 1\n");
        fs::set_permissions(&wide, fs::Permissions::from_mode(0o644)).unwrap();
        atomic_write_private(&wide, "x = 2\n").unwrap();
        assert_eq!(mode(&wide), 0o600, "an old 0644 copy is narrowed");
        assert_eq!(fs::read_to_string(&wide).unwrap(), "x = 2\n");
    }

    #[test]
    fn debounce_waits_one_second_after_the_last_change() {
        let tmp = TempDir::new("debounce");
        let (mut store, cfg, _) = ConfigStore::open(&tmp.file());
        let t0 = Instant::now();
        let next = toggled(&cfg);
        store.mark(t0);
        assert_eq!(store.deadline(), Some(t0 + SAVE_DEBOUNCE));
        assert!(store.poll(t0 + Duration::from_millis(999), &next).is_none());
        assert!(!tmp.file().exists(), "not before the quiet second");
        store.poll(t0 + SAVE_DEBOUNCE, &next);
        assert!(tmp.file().exists());
        assert_eq!(store.deadline(), None);
    }

    #[test]
    fn debounce_restarts_on_each_change() {
        let tmp = TempDir::new("restart");
        let (mut store, cfg, _) = ConfigStore::open(&tmp.file());
        let t0 = Instant::now();
        let mut next = cfg;
        for step in 0..20_u32 {
            let now = t0 + Duration::from_millis(u64::from(step) * 300);
            next.show_gpu = !next.show_gpu;
            store.mark(now);
            store.poll(now, &next);
            assert!(!tmp.file().exists(), "burst of changes at step {step}");
        }
        let last = t0 + Duration::from_millis(19 * 300);
        assert_eq!(store.deadline(), Some(last + SAVE_DEBOUNCE));
        store.poll(last + SAVE_DEBOUNCE, &next);
        assert_eq!(
            ConfigStore::open(&tmp.file()).1,
            next,
            "one write, final value"
        );
    }

    #[test]
    fn poll_without_mark_never_writes() {
        let tmp = TempDir::new("nomark");
        let (mut store, cfg, _) = ConfigStore::open(&tmp.file());
        let far = Instant::now() + Duration::from_secs(3600);
        assert!(store.poll(far, &toggled(&cfg)).is_none());
        assert!(!tmp.file().exists());
    }

    #[test]
    fn flush_saves_pending_change_immediately() {
        let tmp = TempDir::new("flush");
        let (mut store, cfg, _) = ConfigStore::open(&tmp.file());
        store.mark(Instant::now());
        let next = toggled(&cfg);
        assert!(store.flush(&next).is_none());
        assert_eq!(store.deadline(), None);
        assert_eq!(ConfigStore::open(&tmp.file()).1, next);
    }

    /// Regression: `siltop onboarding` over the sealed first-run file
    /// moved its header comment below the tables it wrote.
    #[test]
    fn a_comment_only_file_keeps_its_header_on_top() {
        let tmp = TempDir::new("header-top");
        let path = tmp.file();
        crate::first_run::seal(&path).unwrap();
        let header = fs::read_to_string(&path).unwrap();
        let (mut store, cfg, _) = ConfigStore::open(&path);
        store.flush(&toggled(&cfg));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(&header), "{text}");
        assert_eq!(text.matches("# siltop preferences").count(), 1, "{text}");
        assert_eq!(ConfigStore::open(&path).1, toggled(&cfg));
    }

    #[test]
    fn comment_and_future_key_survive_a_toggle_on_disk() {
        let tmp = TempDir::new("comments");
        let original = "# tuned by hand\nfuture_key = 42\n[proc]\nratio = 50 # wide\n";
        let path = tmp.write(original.as_bytes());
        let (mut store, cfg, warning) = ConfigStore::open(&path);
        assert_eq!(
            warning.as_deref(),
            Some("config: ignored unknown key future_key")
        );
        store.flush(&toggled(&cfg));
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# tuned by hand\nfuture_key = 42\n"),
            "{text}"
        );
        assert!(text.contains("ratio = 50 # wide"));
        assert!(text.contains("gpu = false"));
    }

    #[test]
    fn captured_legacy_file_migrates_only_the_changed_key() {
        let tmp = TempDir::new("legacy");
        let path = tmp.write(LEGACY.as_bytes());
        let (mut store, cfg, _) = ConfigStore::open(&path);
        assert_eq!(cfg.interval, Duration::from_secs(1));
        assert_eq!(cfg.surface, Some(Surface::Work));
        let next = Config {
            proc_sort: ProcSort::Mem,
            ..cfg.clone()
        };
        store.flush(&next);
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("sort = \"cpu\""), "{text}");
        assert!(text.contains("column = \"mem\""), "{text}");
        assert!(text.starts_with("interval_ms = 1000\nsurface = \"work\"\n"));
        assert_eq!(ConfigStore::open(&path).1, next);
    }

    #[test]
    fn corrupt_file_is_backed_up_and_never_written() {
        let tmp = TempDir::new("corrupt");
        let path = tmp.write(CORRUPT);
        let (mut store, cfg, warning) = ConfigStore::open(&path);
        assert_eq!(warning.as_deref(), Some(CONFIG_ERROR));
        assert_eq!(cfg, Config::default());
        assert!(store.is_broken());
        assert_eq!(fs::read(sibling(&path, "bak")).unwrap(), CORRUPT);
        store.mark(Instant::now());
        assert_eq!(store.deadline(), None, "a broken file never schedules");
        assert!(store.flush(&toggled(&cfg)).is_none());
        assert_eq!(fs::read(&path).unwrap(), CORRUPT, "left exactly alone");
    }

    #[test]
    fn toml_syntax_error_is_broken_too() {
        let tmp = TempDir::new("syntax");
        let path = tmp.write(b"show_gpu = maybe\n");
        let (mut store, _, warning) = ConfigStore::open(&path);
        assert_eq!(warning.as_deref(), Some(CONFIG_ERROR));
        store.flush(&toggled(&Config::default()));
        assert_eq!(fs::read(&path).unwrap(), b"show_gpu = maybe\n");
        assert!(sibling(&path, "bak").exists());
    }

    #[test]
    fn read_only_file_is_never_written_and_warns_once() {
        if running_as_root() {
            return;
        }
        let tmp = TempDir::new("ro");
        let path = tmp.write(LEGACY.as_bytes());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        let (mut store, cfg, warning) = ConfigStore::open(&path);
        assert!(warning.is_none());
        assert_eq!(cfg.interval, Duration::from_secs(1), "still read");
        let next = toggled(&cfg);
        assert_eq!(store.flush(&next).as_deref(), Some(CONFIG_READ_ONLY));
        assert!(store.flush(&next).is_none(), "only once");
        assert_eq!(fs::read_to_string(&path).unwrap(), LEGACY);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn file_made_read_only_mid_session_is_not_replaced() {
        if running_as_root() {
            return;
        }
        let tmp = TempDir::new("ro-late");
        let path = tmp.write(b"theme = \"nord\"\n");
        let (mut store, cfg, _) = ConfigStore::open(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(
            store.flush(&toggled(&cfg)).as_deref(),
            Some(CONFIG_READ_ONLY)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"nord\"\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn read_only_directory_fails_soft_without_tmp_litter() {
        if running_as_root() {
            return;
        }
        let tmp = TempDir::new("ro-dir");
        let dir = tmp.file().parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        let (mut store, cfg, _) = ConfigStore::open(&tmp.file());
        let next = toggled(&cfg);
        assert_eq!(store.flush(&next).as_deref(), Some(CONFIG_READ_ONLY));
        assert!(store.flush(&next).is_none());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn hand_edit_during_session_is_kept() {
        let tmp = TempDir::new("external");
        let path = tmp.write(b"theme = \"nord\"\n");
        let (mut store, cfg, _) = ConfigStore::open(&path);
        fs::write(
            &path,
            "theme = \"nord\"\nkeymap = \"vim\" # added by hand\n",
        )
        .unwrap();
        store.flush(&toggled(&cfg));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("keymap = \"vim\" # added by hand"), "{text}");
        assert!(text.contains("gpu = false"));
    }

    #[test]
    fn file_corrupted_during_session_is_not_overwritten() {
        let tmp = TempDir::new("external-bad");
        let path = tmp.write(b"theme = \"nord\"\n");
        let (mut store, cfg, _) = ConfigStore::open(&path);
        fs::write(&path, "theme = \n").unwrap();
        assert_eq!(store.flush(&toggled(&cfg)).as_deref(), Some(CONFIG_ERROR));
        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \n");
        assert!(store.is_broken());
    }

    #[test]
    fn symlinked_config_is_written_through() {
        let tmp = TempDir::new("symlink");
        let target = tmp.0.join("dotfiles-config.toml");
        fs::write(&target, "# dotfiles\n").unwrap();
        let link = tmp.file();
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let (mut store, cfg, _) = ConfigStore::open(&link);
        store.flush(&toggled(&cfg));
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let text = fs::read_to_string(&target).unwrap();
        assert!(
            text.contains("# dotfiles") && text.contains("gpu = false"),
            "{text}"
        );
    }

    #[test]
    fn invalid_utf8_is_treated_as_corrupt() {
        let tmp = TempDir::new("utf8");
        let path = tmp.write(&[0xff, 0xfe, b'a', b'=', b'1']);
        let (store, _, warning) = ConfigStore::open(&path);
        assert!(store.is_broken());
        assert_eq!(warning.as_deref(), Some(CONFIG_ERROR));
    }

    #[test]
    fn ignored_values_are_summarised_in_one_warning() {
        let tmp = TempDir::new("warn");
        let path = tmp.write(b"show_fans = 3\nsurface = \"sideways\"\ninterval_ms = 7\n");
        let (_, cfg, warning) = ConfigStore::open(&path);
        assert_eq!(cfg, Config::default());
        let warning = warning.unwrap();
        assert!(warning.starts_with("config: ignored"), "{warning}");
        assert!(warning.ends_with("(+2 more)"), "{warning}");
    }

    #[test]
    fn disabled_store_never_schedules_or_writes() {
        let (mut store, cfg) = ConfigStore::disabled();
        store.mark(Instant::now());
        assert_eq!(store.deadline(), None);
        assert!(store.flush(&toggled(&cfg)).is_none());
    }

    #[test]
    fn repeated_saves_do_not_duplicate_keys() {
        let tmp = TempDir::new("repeat");
        let (mut store, mut cfg, _) = ConfigStore::open(&tmp.file());
        for ratio in 35..=72 {
            cfg.proc_ratio = ratio;
            store.flush(&cfg);
        }
        let text = fs::read_to_string(tmp.file()).unwrap();
        assert_eq!(text.matches("ratio").count(), 1, "{text}");
        assert_eq!(text.matches("version").count(), 1, "{text}");
        assert_eq!(ConfigStore::open(&tmp.file()).1.proc_ratio, 72);
    }

    #[test]
    fn sibling_names() {
        let path = Path::new("/x/y/config.toml");
        assert_eq!(sibling(path, "bak"), Path::new("/x/y/config.toml.bak"));
        assert_eq!(sibling(path, "tmp"), Path::new("/x/y/config.toml.tmp"));
    }

    fn file_config() -> Config {
        Config {
            interval: Duration::from_millis(1000),
            surface: None,
            theme: String::from("auto"),
            ..Config::default()
        }
    }

    fn cli_config() -> Config {
        Config {
            interval: Duration::from_millis(250),
            surface: Some(Surface::Glance),
            theme: String::from("paper"),
            ..file_config()
        }
    }

    #[test]
    fn rv_cli_overrides_never_persist() {
        let overrides = SessionOverrides::between(&file_config(), &cli_config());
        assert_eq!(overrides.persistable(cli_config()), file_config());
    }

    #[test]
    fn siltop_keys_shows_in_settings_but_never_persists() {
        let file = Config {
            keymap: String::from("btop++"),
            ..Config::default()
        };
        let mut session = file.clone();
        crate::keymap::Profile::apply_session(&mut session, Some("htop"));
        assert_eq!(session.keymap, "htop");
        let overrides = SessionOverrides::between(&file, &session);
        assert_eq!(overrides.persistable(session.clone()).keymap, "btop++");
        let mut changed = session;
        changed.keymap = String::from("siltop");
        assert_eq!(
            overrides.persistable(changed).keymap,
            "siltop",
            "picked in settings: it persists"
        );
    }

    #[test]
    fn rv_cli_overrides_keep_other_session_changes() {
        let overrides = SessionOverrides::between(&file_config(), &cli_config());
        let mut current = cli_config();
        current.show_gpu = !current.show_gpu;
        current.proc_ratio = 40;
        let saved = overrides.persistable(current);
        assert_eq!(saved.show_gpu, !file_config().show_gpu);
        assert_eq!(saved.proc_ratio, 40);
        assert_eq!(saved.interval, file_config().interval);
    }

    #[test]
    fn rv_setting_changed_after_its_flag_persists() {
        let overrides = SessionOverrides::between(&file_config(), &cli_config());
        let mut current = cli_config();
        current.interval = Duration::from_millis(2000);
        current.theme = String::from("nord");
        let saved = overrides.persistable(current);
        assert_eq!(saved.interval, Duration::from_millis(2000), "user chose it");
        assert_eq!(saved.theme, "nord");
        assert_eq!(saved.surface, None, "the untouched flag still unshadows");
    }

    #[test]
    fn rv_no_flags_is_identity() {
        let none = SessionOverrides::between(&file_config(), &file_config());
        assert_eq!(none, SessionOverrides::default());
        for config in [file_config(), cli_config(), Config::default()] {
            assert_eq!(none.persistable(config.clone()), config);
        }
    }

    #[test]
    fn rv_flag_equal_to_file_leaves_no_shadow() {
        let same = Config {
            interval: file_config().interval,
            ..cli_config()
        };
        let overrides = SessionOverrides::between(&file_config(), &same);
        assert!(overrides.interval.is_none());
        let mut current = same;
        current.interval = Duration::from_millis(500);
        assert_eq!(
            overrides.persistable(current).interval,
            Duration::from_millis(500)
        );
    }

    #[test]
    fn rv_cli_overrides_are_not_written_through_the_store() {
        let tmp = TempDir::new("cli-session");
        let (mut store, file, _) = ConfigStore::open(&tmp.file());
        let session = Config {
            interval: Duration::from_millis(250),
            surface: Some(Surface::Glance),
            theme: String::from("paper"),
            ..file.clone()
        };
        let overrides = SessionOverrides::between(&file, &session);
        let mut current = session;
        current.show_fans = !current.show_fans;
        store.flush(&overrides.persistable(current));
        let text = fs::read_to_string(tmp.file()).unwrap();
        assert!(!text.contains("250"), "{text}");
        assert!(!text.contains("paper"), "{text}");
        assert!(!text.contains("glance"), "{text}");
        let (_, reloaded, _) = ConfigStore::open(&tmp.file());
        assert_eq!(reloaded.interval, file.interval);
        assert_eq!(reloaded.show_fans, !file.show_fans);
    }
}
