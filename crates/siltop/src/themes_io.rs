//! User theme files on disk: `~/.config/siltop/themes/*.toml`, loaded
//! once at startup, and `siltop theme import|list`.

use std::io::Write;
use std::path::{Path, PathBuf};

use siltop_ui::{BtopTheme, Theme};

/// A theme file is a few dozen lines; anything bigger is not one.
const MAX_BYTES: u64 = 64 * 1024;

/// `themes/` next to the config file.
pub fn default_dir() -> Option<PathBuf> {
    crate::config_io::default_path().map(|config| config.with_file_name("themes"))
}

/// A file error in the CLI's plain words: `no such file: X`, `X is a
/// folder`, else `X: <reason>` without the `(os error N)` tail.
fn file_error(path: &Path, error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => format!("no such file: {}", path.display()),
        std::io::ErrorKind::IsADirectory => format!("{} is a folder", path.display()),
        _ => format!("{}: {}", path.display(), crate::io_reason(error)),
    }
}

fn read_capped(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| file_error(path, &e))?;
    if meta.is_dir() {
        return Err(format!("{} is a folder", path.display()));
    }
    if meta.len() > MAX_BYTES {
        return Err(format!("{}: too large for a theme file", path.display()));
    }
    std::fs::read_to_string(path).map_err(|e| file_error(path, &e))
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Every `*.toml` in `dir`, sorted by file name, parsed; a bad file is
/// reported and skipped, never fatal.
pub fn load_dir(dir: &Path) -> (Vec<Theme>, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    let mut themes = Vec::new();
    let mut problems = Vec::new();
    for path in paths {
        match read_capped(&path).and_then(|text| Theme::from_toml(&text, &stem(&path))) {
            Ok(theme) => themes.push(theme),
            Err(why) => problems.push(format!("theme {}: {why}", path.display())),
        }
    }
    (themes, problems)
}

/// Load and install the user's themes; call once, before parsing `--theme`.
pub fn install() {
    if let Some(dir) = default_dir() {
        let (themes, _problems) = load_dir(&dir);
        siltop_ui::install_user_themes(themes);
    }
}

/// A user theme never shadows a built-in, so an imported `nord` would
/// never be the one `--theme nord` picks: it becomes `nord-btop`.
fn unshadowed(name: String) -> String {
    let taken = Theme::names()
        .into_iter()
        .chain(Theme::pack())
        .any(|builtin| builtin == name);
    if taken { format!("{name}-btop") } else { name }
}

/// Convert a btop `.theme` file into `dir/<name>.toml`, renamed if a
/// built-in has its name; returns the path.
///
/// # Errors
/// Unreadable input, a file that is not a btop theme, or a failed write.
pub fn import(source: &Path, dir: &Path) -> Result<PathBuf, String> {
    let text = read_capped(source)?;
    let btop = BtopTheme::parse(&text);
    // A colour btop's format cannot hold is named with its line, rather
    // than the file being called "not a theme" for it.
    let bad_colour = || {
        BtopTheme::bad_colour(&text).map(|(line, key, value)| {
            format!(
                "{} line {line}: theme[{key}] has a bad colour {value:?}; expected #rrggbb",
                source.display()
            )
        })
    };
    if btop.is_empty() {
        return Err(bad_colour().unwrap_or_else(|| {
            format!(
                "{}: no theme[...] lines; is this a btop .theme file?",
                source.display()
            )
        }));
    }
    let name = stem(source).to_ascii_lowercase().replace([' ', '_'], "-");
    if name.is_empty() {
        return Err(String::from("the theme file needs a name"));
    }
    let theme = btop
        .to_theme(&unshadowed(name))
        .map_err(|why| bad_colour().unwrap_or(why))?;
    let dest = dir.join(format!("{}.toml", theme.name));
    crate::config_io::atomic_write(&dest, &theme.to_toml(btop.background()))
        .map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(dest)
}

/// `siltop theme import <FILE>` / `siltop theme list`.
pub fn run(
    file: Option<&Path>,
    dir: Option<&Path>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let Some(file) = file else {
        for name in Theme::catalog() {
            let _ = writeln!(out, "{name}");
        }
        return 0;
    };
    let Some(dir) = dir else {
        let _ = writeln!(
            err,
            "siltop: no config directory: set HOME or XDG_CONFIG_HOME"
        );
        return 1;
    };
    match import(file, dir) {
        Ok(dest) => {
            let _ = writeln!(
                out,
                "imported {} → {}\nuse it with: siltop --theme {}",
                file.display(),
                dest.display(),
                stem(&dest)
            );
            0
        }
        Err(why) => {
            let _ = writeln!(err, "siltop: {why}");
            1
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const NORD: &str = include_str!("../../siltop-ui/src/theme/fixtures/btop-nord.theme");

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "siltop-themes-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn import_writes_a_theme_file_that_loads_back() {
        let root = tempdir("import");
        let source = root.join("Nord Btop.theme");
        std::fs::write(&source, NORD).unwrap();
        let themes = root.join("themes");
        let dest = import(&source, &themes).unwrap();
        assert_eq!(dest, themes.join("nord-btop.toml"));
        let (loaded, problems) = load_dir(&themes);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "nord-btop");
    }

    #[test]
    fn import_refuses_a_file_that_is_not_a_btop_theme() {
        let root = tempdir("notbtop");
        let source = root.join("notes.theme");
        std::fs::write(&source, "hello = world\n").unwrap();
        let why = import(&source, &root.join("themes")).unwrap_err();
        assert!(why.contains("btop"), "{why}");
    }

    /// Regression: a file whose only line held a bad colour was called
    /// "no theme[...] lines" although it had one.
    #[test]
    fn import_names_a_bad_colour_and_its_line() {
        let root = tempdir("badcolour");
        let source = root.join("broken.theme");
        std::fs::write(&source, "# a btop theme\ntheme[main_bg]=\"#zzzzzz\"\n").unwrap();
        let why = import(&source, &root.join("themes")).unwrap_err();
        assert_eq!(
            why,
            format!(
                "{} line 2: theme[main_bg] has a bad colour \"#zzzzzz\"; expected #rrggbb",
                source.display()
            )
        );
        // Good lines but a bad main_fg: the bad line, not "no main_fg".
        std::fs::write(
            &source,
            "theme[cpu_start]=\"#50f095\"\ntheme[main_fg]=\"#1234\"\n",
        )
        .unwrap();
        let why = import(&source, &root.join("themes")).unwrap_err();
        assert!(why.contains("line 2: theme[main_fg]"), "{why}");
    }

    /// Regression: a missing file or a folder printed raw OS errors.
    #[test]
    fn import_of_a_missing_file_or_a_folder_is_one_plain_line() {
        let root = tempdir("missing");
        let missing = root.join("nope.theme");
        assert_eq!(
            import(&missing, &root.join("themes")).unwrap_err(),
            format!("no such file: {}", missing.display())
        );
        assert_eq!(
            import(&root, &root.join("themes")).unwrap_err(),
            format!("{} is a folder", root.display())
        );
    }

    #[test]
    fn load_dir_skips_bad_files_and_non_toml() {
        let dir = tempdir("load");
        std::fs::write(dir.join("good.toml"), "cpu = \"#5fd497\"\n").unwrap();
        std::fs::write(dir.join("bad.toml"), "cpu = \"green\"\n").unwrap();
        std::fs::write(dir.join("readme.txt"), "not a theme").unwrap();
        let (themes, problems) = load_dir(&dir);
        assert_eq!(themes.iter().map(|t| t.name).collect::<Vec<_>>(), ["good"]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("bad.toml"), "{problems:?}");
    }

    #[test]
    fn load_dir_of_a_missing_directory_is_empty() {
        let (themes, problems) = load_dir(Path::new("/nonexistent/siltop/themes"));
        assert!(themes.is_empty() && problems.is_empty());
    }

    #[test]
    fn import_command_succeeds() {
        let root = tempdir("run");
        let source = root.join("custom.theme");
        std::fs::write(&source, NORD).unwrap();
        let themes = root.join("themes");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(Some(&source), Some(&themes), &mut out, &mut err);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert!(themes.join("custom.toml").exists());
        assert!(String::from_utf8(out).unwrap().contains("--theme custom\n"));
    }

    /// `nord.theme` once became `nord.toml`, which the built-in Nord
    /// shadows: the printed `--theme nord` never picked the user's file.
    #[test]
    fn importing_a_builtin_name_is_renamed_so_it_resolves() {
        let root = tempdir("shadow");
        let themes = root.join("themes");
        for (file, want) in [
            ("nord.theme", "nord-btop"),
            ("Gruvbox_Dark.theme", "gruvbox-dark-btop"),
            ("billabong.theme", "billabong-btop"),
            ("auto.theme", "auto-btop"),
        ] {
            let source = root.join(file);
            std::fs::write(&source, NORD).unwrap();
            let (mut out, mut err) = (Vec::new(), Vec::new());
            assert_eq!(run(Some(&source), Some(&themes), &mut out, &mut err), 0);
            assert!(themes.join(format!("{want}.toml")).exists(), "{file}");
            let said = String::from_utf8(out).unwrap();
            assert!(said.contains(&format!("--theme {want}\n")), "{said}");
        }
        let (loaded, _) = load_dir(&themes);
        assert!(loaded.iter().all(|t| {
            Theme::names()
                .into_iter()
                .chain(Theme::pack())
                .all(|builtin| builtin != t.name)
        }));
    }

    #[test]
    fn list_names_every_theme_untagged() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(None, None, &mut out, &mut err), 0);
        let list = String::from_utf8(out).unwrap();
        assert!(list.lines().any(|l| l == "billabong"), "{list}");
        assert!(list.lines().any(|l| l == "dracula"), "{list}");
        assert!(!list.contains('('), "{list}");
    }
}
