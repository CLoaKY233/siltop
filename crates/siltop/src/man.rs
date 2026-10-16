//! `siltop man`: a roff manual page generated from the CLI table and the
//! keymap, so it can never drift from what the binary accepts.

use std::fmt::Write as _;

use crate::cli::{self, COMMANDS, FLAGS, Section};
use crate::keymap::{self, Profile};

/// Escapes text for roff: backslashes, hyphens (so they are not
/// hyphenation points), and a leading `.` or `'` that would read as a request.
fn esc(text: &str) -> String {
    let body = text.replace('\\', r"\e").replace('-', r"\-");
    if body.starts_with('.') || body.starts_with('\'') {
        format!(r"\&{body}")
    } else {
        body
    }
}

/// `.TP` paragraph: a bold tag, then its description.
fn item(out: &mut String, tag: &str, body: &str) {
    let _ = write!(out, ".TP\n\\fB{}\\fR\n{}\n", esc(tag), esc(body));
}

/// The whole page.
#[must_use]
pub fn page() -> String {
    let mut out = String::new();
    let date = cli::BUILD_DATE.unwrap_or("");
    let _ = writeln!(
        out,
        ".TH SILTOP 1 \"{date}\" \"siltop {}\" \"User Commands\"",
        cli::VERSION
    );
    out.push_str(".SH NAME\nsiltop \\- Apple Silicon system monitor\n");
    out.push_str(".SH SYNOPSIS\n.B siltop\n[\\fIOPTIONS\\fR]\n");
    for command in COMMANDS {
        let _ = write!(out, ".br\n.B siltop {}\n", esc(command.name));
        if !command.args.is_empty() {
            let _ = writeln!(out, "\\fI{}\\fR", esc(command.args));
        }
    }
    out.push_str(
        ".SH DESCRIPTION\n\
         A live dashboard of CPU (per core), GPU, memory, power, temperatures, \
         fans, network, disks and processes on Apple Silicon Macs. Hardware a \
         Mac does not have is hidden rather than shown as zero. Everything runs \
         offline: no network, no telemetry, no sudo.\n",
    );
    let options = |out: &mut String, section: Section| {
        let _ = writeln!(out, ".SH {}", section.title().to_uppercase());
        for spec in FLAGS.iter().filter(|spec| spec.section == section) {
            item(out, spec.synopsis().trim_start(), spec.help);
        }
    };
    for global in Section::ALL.into_iter().filter(|s| *s != Section::Command) {
        options(&mut out, global);
    }
    out.push_str(".SH COMMANDS\n");
    for command in COMMANDS {
        let head = format!("{} {}", command.name, command.args);
        item(&mut out, head.trim_end(), command.help);
        for (word, arg, help) in command.subs {
            let head = format!("{} {word} {arg}", command.name);
            item(&mut out, head.trim_end(), help);
        }
    }
    options(&mut out, Section::Command);
    out.push_str(
        ".SH KEYS\nThe default key profile. Press \\fB?\\fR in the dashboard for the \
                  full list, or pick the htop or btop profile in settings.\n",
    );
    for group in keymap::help_groups(Profile::Siltop) {
        let _ = writeln!(out, ".SS {}", esc(group.title));
        for (keys, help) in &group.rows {
            item(&mut out, keys, help);
        }
    }
    out.push_str(
        ".SH FILES\n\
         .TP\n\\fI$XDG_CONFIG_HOME/siltop/config.toml\\fR\n\
         Preferences, saved as you change them (default \\fI~/.config\\fR).\n\
         .TP\n\\fI$XDG_STATE_HOME/siltop/history\\fR\n\
         Long history, up to 30 days (default \\fI~/.local/state\\fR).\n\
         .SH ENVIRONMENT\n\
         .TP\n\\fBSILTOP_KEYS\\fR\n\
         Start with the siltop, htop or btop key profile.\n\
         .SH PRIVACY\n\
         siltop reads this Mac locally and opens no connection of its own. \
         If update checks are on, it runs \\fB/usr/bin/curl\\fR at most once a day \
         to ask the GitHub API for the latest release number. The only thing sent \
         is the User-Agent \\fBsiltop/<version>\\fR. \
         Only \\fBserve\\fR listens, on loopback unless \\fB\\-\\-addr\\fR names another \
         address, and \\fBfleet\\fR reaches other Macs through your own \\fBssh\\fR.\n\
         .SH UNINSTALL\n\
         \\fBbrew uninstall siltop\\fR, or \\fBrm ~/.local/bin/siltop\\fR. \
         Run \\fBsiltop reset\\fR first to delete config, state and \
         history.\n\
         .SH EXIT STATUS\n",
    );
    for (code, what) in cli::EXIT_CODES {
        item(&mut out, &code.to_string(), what);
    }
    out.push_str(
        ".SH SEE ALSO\n\
         .BR top (1),\n\
         .BR powermetrics (1)\n",
    );
    fill(&out)
}

/// Longest source line `mandoc -T lint` accepts without a style warning.
const MAX_LINE: usize = 80;

/// Breaks text lines longer than [`MAX_LINE`] at spaces. roff joins them
/// again, so the rendered page is the same; requests are left alone.
fn fill(page: &str) -> String {
    let mut out = String::with_capacity(page.len() + page.len() / 16);
    for line in page.lines() {
        if line.len() <= MAX_LINE || line.starts_with('.') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let mut current = String::new();
        for word in line.split(' ') {
            if !current.is_empty() && current.len() + 1 + word.len() > MAX_LINE {
                out.push_str(&current);
                out.push('\n');
                current.clear();
            }
            if current.is_empty() {
                // A broken line must not start with a request character.
                if word.starts_with('.') || word.starts_with('\'') {
                    current.push_str(r"\&");
                }
            } else {
                current.push(' ');
            }
            current.push_str(word);
        }
        out.push_str(&current);
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// The page with roff escapes undone, for plain-text searching.
    fn plain() -> String {
        page().replace(r"\-", "-").replace(r"\e", "\\")
    }

    #[test]
    fn no_source_line_is_longer_than_80_bytes() {
        for line in page().lines() {
            assert!(line.len() <= MAX_LINE, "{} bytes: {line}", line.len());
        }
    }

    #[test]
    fn man_page_contains_every_key_from_the_keymap() {
        let text = plain();
        let groups = keymap::help_groups(Profile::Siltop);
        assert!(!groups.is_empty());
        for group in groups {
            assert!(text.contains(group.title), "missing group {}", group.title);
            for (keys, help) in &group.rows {
                assert!(text.contains(keys.as_str()), "missing keys {keys}");
                assert!(text.contains(help), "missing help {help}");
            }
        }
    }

    #[test]
    fn man_page_lists_every_exit_status() {
        let text = plain();
        let section = text.split(".SH EXIT STATUS").nth(1).unwrap();
        for (code, what) in cli::EXIT_CODES {
            assert!(section.contains(&format!("\\fB{code}\\fR")), "{code}");
            assert!(section.contains(what), "{what}");
        }
    }

    #[test]
    fn man_page_contains_every_flag_and_command() {
        let text = plain();
        for spec in FLAGS {
            assert!(text.contains(spec.long), "{}", spec.long);
            assert!(text.contains(spec.help), "{}", spec.help);
        }
        for command in COMMANDS {
            assert!(text.contains(command.name), "{}", command.name);
            for (word, ..) in command.subs {
                assert!(text.contains(&format!("{} {word}", command.name)));
            }
        }
    }

    #[test]
    fn man_page_is_well_formed_roff() {
        let page = page();
        assert!(page.starts_with(".TH SILTOP 1 "));
        for section in [
            "NAME",
            "SYNOPSIS",
            "DESCRIPTION",
            "COMMANDS",
            "KEYS",
            "FILES",
            "PRIVACY",
        ] {
            assert!(page.contains(&format!(".SH {section}\n")), "{section}");
        }
        for line in page.lines() {
            // A text line starting with `'` or an unknown `.` request would
            // be eaten by troff.
            assert!(!line.starts_with('\''), "{line}");
            if let Some(request) = line.strip_prefix('.') {
                let name = request.split_whitespace().next().unwrap_or("");
                assert!(
                    ["TH", "SH", "SS", "TP", "B", "BR", "br"].contains(&name),
                    "unknown request {line:?}"
                );
            }
        }
        assert!(!page.contains('\x1b'));
    }

    #[test]
    fn privacy_names_the_commands_that_touch_the_network() {
        // Regression: PRIVACY said siltop "never opens a network
        // connection" while `serve` listens and `fleet` runs ssh.
        let page = page();
        let privacy = page
            .split(".SH PRIVACY\n")
            .nth(1)
            .and_then(|rest| rest.split(".SH ").next())
            .unwrap();
        assert!(!privacy.contains("never opens"), "{privacy}");
        for word in ["serve", "loopback", r"\-\-addr", "fleet", "ssh"] {
            assert!(privacy.contains(word), "{word} in {privacy}");
        }
    }

    #[test]
    fn esc_guards_hyphens_backslashes_and_leading_dots() {
        assert_eq!(esc("--json"), r"\-\-json");
        assert_eq!(esc(r"a\b"), r"a\eb");
        assert_eq!(esc(".hidden"), r"\&.hidden");
        assert_eq!(esc("'quoted"), r"\&'quoted");
    }
}
