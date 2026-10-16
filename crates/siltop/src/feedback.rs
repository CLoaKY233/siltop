//! `siltop feedback` prints a new-issue URL on GitHub and opens it in
//! a browser, except over ssh: the feature-request form, or with `--bug` the
//! bug report. `--copy` puts `doctor` output on the clipboard and only
//! prints the URL.
//! The app itself makes no request. The query names the issue form and
//! prefills only the version, the chip, the macOS build and how this copy
//! was installed, each into the form field with that id.

use std::io::{self, Write};
use std::process::Command;

use crate::cli::{self, EXIT_ERROR};
use crate::update::InstallKind;

const NEW_ISSUE: &str = "https://github.com/CLoaKY233/siltop/issues/new";

/// The four facts the form is allowed to see, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub version: String,
    pub chip: String,
    pub macos: String,
    pub install: InstallKind,
    pub bug: bool,
}

impl Facts {
    #[must_use]
    pub fn url(&self) -> String {
        let template = if self.bug { "bug.yml" } else { "feature.yml" };
        format!(
            "{NEW_ISSUE}?template={template}&version={}&chip={}&macos={}&install={}",
            encode(&self.version),
            encode(&self.chip),
            encode(&self.macos),
            encode(self.install.label()),
        )
    }
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("%{byte:02X}"));
            }
        }
    }
    out
}

/// Chip and macOS build from sysctl. Empty when the call is unavailable.
#[must_use]
pub fn this_mac() -> (String, String) {
    (sysctl("machdep.cpu.brand_string"), sysctl("kern.osversion"))
}

fn sysctl(key: &str) -> String {
    Command::new("/usr/sbin/sysctl")
        .args(["-n", key])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map_or(String::new(), |out| {
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        })
}

/// An ssh session has no browser of its own: `open` would launch one on
/// the Mac's screen, not in front of the person typing.
#[must_use]
pub fn in_ssh(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|name| var(name).is_some_and(|value| !value.is_empty()))
}

/// `/usr/bin/open`. Only [`command`] calls it; tests pass their own opener.
fn open_url(url: &str) -> io::Result<()> {
    let status = Command::new("/usr/bin/open").arg(url).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("open exited with {status}")))
    }
}

/// `doctor` output on the clipboard, for pasting into the form.
pub fn copy_doctor() -> io::Result<()> {
    let mut text = Vec::new();
    crate::doctor::run(&mut text).map_err(|err| io::Error::other(err.to_string()))?;
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(&text)?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("pbcopy failed"))
    }
}

/// The side effects [`run`] may have, so a test never opens a browser or
/// touches the clipboard.
pub struct Hooks<'a> {
    pub open: &'a mut dyn FnMut(&str) -> io::Result<()>,
    pub copy: &'a mut dyn FnMut() -> io::Result<()>,
    pub ssh: bool,
}

/// Print `url` always. `copy` copies `doctor` output and opens nothing;
/// otherwise the browser opens unless this is an ssh session. A failed
/// copy or open is a note, not an error: the URL is already printed.
pub fn run(
    copy: bool,
    url: &str,
    hooks: &mut Hooks<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    if copy {
        match (hooks.copy)() {
            Ok(()) => {
                let _ = writeln!(out, "siltop: doctor output is on the clipboard");
            }
            Err(error) => {
                let _ = writeln!(
                    err,
                    "siltop: could not copy doctor output ({error}); paste `siltop doctor` into the form instead"
                );
            }
        }
    }
    match writeln!(out, "{url}").and_then(|()| out.flush()) {
        // The reader went away (`| true`): nothing more to say, and no
        // browser for a link nobody saw printed.
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return 0,
        Err(_) => return EXIT_ERROR,
        Ok(()) => {}
    }
    if copy || hooks.ssh {
        return 0;
    }
    if let Err(error) = (hooks.open)(url) {
        let _ = writeln!(
            err,
            "siltop: could not open a browser ({error}); use the link above"
        );
    }
    0
}

pub fn command(bug: bool, copy: bool, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let install =
        std::env::current_exe().map_or(InstallKind::Other, |exe| crate::update::install_kind(&exe));
    let (chip, macos) = this_mac();
    let facts = Facts {
        version: cli::VERSION.to_owned(),
        chip,
        macos,
        install,
        bug,
    };
    let mut hooks = Hooks {
        open: &mut open_url,
        copy: &mut copy_doctor,
        ssh: in_ssh(|name| std::env::var_os(name)),
    };
    run(copy, &facts.url(), &mut hooks, out, err)
}

/// The query string holds only the template, version, chip, macos and install.
#[cfg(test)]
#[must_use]
fn query_keys(url: &str) -> Vec<String> {
    let Some(query) = url.split_once('?').map(|(_, q)| q) else {
        return Vec::new();
    };
    query
        .split('&')
        .filter_map(|pair| pair.split_once('=').map(|(key, _)| key.to_owned()))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_url_carries_only_the_four_fields() {
        let facts = Facts {
            version: String::from("1.1.0"),
            chip: String::from("Apple M5 Pro"),
            macos: String::from("26A428"),
            install: InstallKind::Download,
            bug: false,
        };
        let url = facts.url();
        assert!(url.starts_with("https://github.com/CLoaKY233/siltop/issues/new?"));
        assert_eq!(
            query_keys(&url),
            ["template", "version", "chip", "macos", "install"]
        );
        assert!(url.contains("template=feature.yml"));
        assert!(url.contains("version=1.1.0"));
        assert!(url.contains("chip=Apple%20M5%20Pro"));
        assert!(url.contains("macos=26A428"));
        assert!(url.contains("install=download"));
        assert!(!url.contains("serial"));
        assert!(!url.contains("config"));
    }

    #[test]
    fn a_bug_report_picks_the_bug_form_and_nothing_more() {
        let facts = Facts {
            version: String::from("1.1.0"),
            chip: String::from("Apple M5 Pro"),
            macos: String::from("26A428"),
            install: InstallKind::Brew,
            bug: true,
        };
        let keys = query_keys(&facts.url());
        assert_eq!(keys, ["template", "version", "chip", "macos", "install"]);
        assert!(facts.url().contains("install=brew"));
        assert!(facts.url().contains("template=bug.yml"));
    }

    const URL: &str = "https://github.com/CLoaKY233/siltop/issues/new?template=bug.yml&version=1.1.0&chip=x&macos=y&install=other";

    /// Runs [`run`] with recording hooks: (exit, stdout, stderr, opened, copied).
    fn drive(copy: bool, ssh: bool, copy_ok: bool) -> (u8, String, String, Vec<String>, usize) {
        let mut opened = Vec::new();
        let mut copied = 0;
        let mut open = |url: &str| {
            opened.push(url.to_owned());
            Ok(())
        };
        let mut clipboard = || {
            copied += 1;
            if copy_ok {
                Ok(())
            } else {
                Err(io::Error::other("pbcopy failed"))
            }
        };
        let mut hooks = Hooks {
            open: &mut open,
            copy: &mut clipboard,
            ssh,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(copy, URL, &mut hooks, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            opened,
            copied,
        )
    }

    #[test]
    fn printing_the_url_does_not_require_a_browser() {
        let (code, out, _, opened, _) = drive(false, false, true);
        assert_eq!(code, 0);
        assert!(out.contains("install=other"));
        assert_eq!(query_keys(out.trim()).len(), 5);
        assert_eq!(opened, [URL]);
    }

    #[test]
    fn copy_copies_and_prints_but_never_opens() {
        let (code, out, _, opened, copied) = drive(true, false, true);
        assert_eq!(code, 0);
        assert_eq!(copied, 1);
        assert!(opened.is_empty());
        assert!(out.contains("clipboard") && out.contains(URL));
    }

    #[test]
    fn a_failed_copy_still_prints_the_url_and_exits_zero() {
        let (code, out, err, opened, _) = drive(true, false, false);
        assert_eq!(code, 0);
        assert!(out.contains(URL));
        assert!(err.contains("siltop doctor"), "{err}");
        assert!(opened.is_empty());
    }

    /// Regression: `siltop feedback | true` exited 1.
    #[test]
    fn a_closed_stdout_is_a_normal_end_and_opens_nothing() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut opened = 0;
        let mut open = |_: &str| {
            opened += 1;
            Ok(())
        };
        let mut copy = || Ok(());
        let mut hooks = Hooks {
            open: &mut open,
            copy: &mut copy,
            ssh: true,
        };
        let mut err = Vec::new();
        assert_eq!(run(false, URL, &mut hooks, &mut Closed, &mut err), 0);
        assert!(err.is_empty());
        assert_eq!(opened, 0);
    }

    #[test]
    fn over_ssh_the_url_is_printed_and_nothing_opens() {
        let (code, out, _, opened, _) = drive(false, true, true);
        assert_eq!(code, 0);
        assert!(out.contains(URL));
        assert!(opened.is_empty());
        assert!(in_ssh(
            |name| (name == "SSH_TTY").then(|| "/dev/ttys001".into())
        ));
        assert!(in_ssh(
            |name| (name == "SSH_CONNECTION").then(|| "1 2 3 4".into())
        ));
        assert!(!in_ssh(|name| (name == "SSH_TTY").then(|| "".into())));
        assert!(!in_ssh(|_| None));
    }
}
