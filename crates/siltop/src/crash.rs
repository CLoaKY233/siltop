//! A panic writes one file under the state directory. The next launch
//! shows a toast. Nothing is sent.

use std::fs;
use std::io::Write;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli;

/// `~/.local/state/siltop`, or `$XDG_STATE_HOME/siltop`.
#[must_use]
pub fn dir() -> Option<PathBuf> {
    crate::store_disk::default_root().and_then(|history| history.parent().map(Path::to_path_buf))
}

/// Write `crash-<unix>.txt`. Failure to write is ignored: we are already panicking.
pub fn write(info: &PanicHookInfo<'_>) {
    let Some(dir) = dir() else { return };
    let _ = fs::create_dir_all(&dir);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let path = dir.join(format!("crash-{stamp}.txt"));
    let mut body = format!(
        "siltop {}\nchip: {}\nmacos: {}\n\n{info}\n\n",
        cli::VERSION,
        sysctl("machdep.cpu.brand_string"),
        sysctl("kern.osversion"),
    );
    body.push_str(&std::backtrace::Backtrace::force_capture().to_string());
    body.push('\n');
    let Ok(mut file) = fs::File::create(&path) else {
        return;
    };
    let _ = file.write_all(body.as_bytes());
}

fn sysctl(key: &str) -> String {
    Command::new("/usr/sbin/sysctl")
        .args(["-n", key])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// The newest unreported crash. Every unreported crash is renamed to
/// `.seen`, so one toast covers them all and it does not come back.
pub fn take() -> Option<PathBuf> {
    take_in(&dir()?)
}

fn take_in(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("crash-") && name.to_ascii_lowercase().ends_with(".txt")
                })
        })
        .collect();
    // `crash-<unix>` sorts by time only while the stamps have equal width.
    found.sort_by_key(|path| (path.as_os_str().len(), path.clone()));
    let newest = found.last()?.with_extension("seen");
    let mut shown = false;
    for path in &found {
        shown |= fs::rename(path, path.with_extension("seen")).is_ok();
    }
    shown.then_some(newest)
}

/// The line the next launch shows.
pub const TOAST: &str = "siltop crashed last time · `siltop feedback --bug` to report";

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp(name: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!("siltop-crash-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }

    #[test]
    fn a_crash_file_is_taken_once() {
        let dir = temp("one");
        let file = dir.0.join("crash-1.txt");
        fs::write(&file, "boom\n").unwrap();
        let taken = take_in(&dir.0).unwrap();
        assert!(taken.ends_with("crash-1.seen"));
        assert!(!file.exists());
        assert!(take_in(&dir.0).is_none());
    }

    #[test]
    fn several_crashes_show_one_toast_for_the_newest() {
        let dir = temp("many");
        for stamp in ["crash-900.txt", "crash-1000.txt", "crash-950.txt"] {
            fs::write(dir.0.join(stamp), "boom\n").unwrap();
        }
        let taken = take_in(&dir.0).unwrap();
        assert!(taken.ends_with("crash-1000.seen"), "{}", taken.display());
        assert!(take_in(&dir.0).is_none());
        assert!(dir.0.join("crash-900.seen").exists());
    }
}
