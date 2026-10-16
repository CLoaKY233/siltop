//! `siltop completions zsh|bash|fish`: completion scripts generated from
//! the CLI table in [`crate::cli`], so a new flag completes without anyone
//! touching this file.

use std::fmt::Write as _;

use crate::cli::{COMMANDS, CommandSpec, Complete, FLAGS, FlagSpec, Shell};

/// The completion script for `shell`.
#[must_use]
pub fn script(shell: Shell) -> String {
    match shell {
        Shell::Zsh => zsh(),
        Shell::Bash => bash(),
        Shell::Fish => fish(),
    }
}

/// The words a command's argument completes to: its sub-words or its list.
fn command_words(command: &CommandSpec) -> Vec<&'static str> {
    match command.value {
        _ if !command.subs.is_empty() => command.subs.iter().map(|(word, ..)| *word).collect(),
        Some(Complete::Words(words)) => words(),
        _ => Vec::new(),
    }
}

/// Whether the command itself takes a path (`replay <FILE>`).
fn takes_file(command: &CommandSpec) -> bool {
    matches!(command.value, Some(Complete::File))
}

/// Single-quoted for any POSIX-ish shell: `'` becomes `'\''`.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Text for inside a single-quoted zsh `_arguments` spec: brackets become
/// parentheses, and a `:` (which would end the field) and a `'` (which
/// would end the quoting) are escaped rather than dropped.
fn zsh_escape(text: &str) -> String {
    text.replace('[', "(")
        .replace(']', ")")
        .replace(':', r"\:")
        .replace('\'', r"'\''")
}

fn zsh_value(spec: &FlagSpec) -> String {
    let Some((placeholder, complete)) = spec.value else {
        return String::new();
    };
    let message = zsh_escape(&placeholder.to_lowercase());
    match complete {
        Complete::Text => format!(":{message}: "),
        Complete::File => format!(":{message}:_files"),
        Complete::Words(words) => format!(":{message}:({})", words().join(" ")),
    }
}

/// The `_arguments` specs for what follows `command` (whose own words
/// start at 1): its sub-word, then a file for a sub-word that takes one.
fn zsh_positionals(command: &CommandSpec) -> String {
    let words = command_words(command);
    if takes_file(command) {
        return String::from(" '1: :_files'");
    }
    if words.is_empty() {
        return String::new();
    }
    let mut specs = format!(" '1: :({})'", words.join(" "));
    let file_subs: Vec<&str> = command
        .subs
        .iter()
        .filter(|(_, arg, _)| !arg.is_empty())
        .map(|(word, ..)| *word)
        .collect();
    if !file_subs.is_empty() {
        let _ = write!(
            specs,
            " '2: :{{case $words[2] in ({}) _files ;; esac}}'",
            file_subs.join("|")
        );
    }
    specs
}

fn zsh() -> String {
    let mut out =
        String::from("#compdef siltop\n\n_siltop() {\n  local -a commands\n  commands=(\n");
    for command in COMMANDS {
        let _ = writeln!(
            out,
            "    {}",
            quote(&format!(
                "{}:{}",
                command.name,
                command.help.replace(':', " -")
            ))
        );
    }
    out.push_str("  )\n  local -a flags\n  flags=(\n");
    for spec in FLAGS {
        let help = zsh_escape(spec.help);
        let value = zsh_value(spec);
        let line = match spec.short {
            Some(c) => format!(
                "'(-{c} {long})'{{-{c},{long}}}'[{help}]{value}'",
                long = spec.long
            ),
            None => format!("'{}[{help}]{value}'", spec.long),
        };
        let _ = writeln!(out, "    {line}");
    }
    out.push_str("  )\n  local state\n  _arguments -s $flags '1: :->command' '*:: :->args'\n");
    out.push_str("  case $state in\n    command) _describe -t commands 'siltop command' commands ;;\n    args)\n      case $words[1] in\n");
    // Each command re-parses the flags, so `line --style <TAB>` completes.
    for command in COMMANDS {
        let _ = writeln!(
            out,
            "        {}) _arguments -s $flags{} ;;",
            command.name,
            zsh_positionals(command)
        );
    }
    out.push_str("      esac ;;\n  esac\n}\n\n_siltop \"$@\"\n");
    out
}

fn bash() -> String {
    let mut out = String::from(
        "# bash completion for siltop\n_siltop() {\n  local cur prev\n  cur=\"${COMP_WORDS[COMP_CWORD]}\"\n  prev=\"${COMP_WORDS[COMP_CWORD-1]}\"\n  case \"$prev\" in\n",
    );
    for spec in FLAGS {
        let Some((_, complete)) = spec.value else {
            continue;
        };
        let names = spec
            .short
            .map_or_else(|| spec.long.to_owned(), |c| format!("-{c}|{}", spec.long));
        let action = match complete {
            Complete::Text => String::from("return"),
            Complete::File => String::from("COMPREPLY=($(compgen -f -- \"$cur\")); return"),
            Complete::Words(words) => format!(
                "COMPREPLY=($(compgen -W \"{}\" -- \"$cur\")); return",
                words().join(" ")
            ),
        };
        let _ = writeln!(out, "    {names}) {action} ;;");
    }
    for command in COMMANDS {
        let words = command_words(command);
        if !words.is_empty() {
            let _ = writeln!(
                out,
                "    {}) COMPREPLY=($(compgen -W \"{}\" -- \"$cur\")); return ;;",
                command.name,
                words.join(" ")
            );
        }
    }
    // `theme import <FILE>` and friends: a sub-word (or `replay`) that takes a path.
    let file_subs: Vec<&str> = COMMANDS
        .iter()
        .flat_map(|command| command.subs.iter())
        .filter(|(_, arg, _)| !arg.is_empty())
        .map(|(word, ..)| *word)
        .chain(
            COMMANDS
                .iter()
                .filter(|command| takes_file(command))
                .map(|command| command.name),
        )
        .collect();
    if !file_subs.is_empty() {
        let _ = writeln!(
            out,
            "    {}) COMPREPLY=($(compgen -f -- \"$cur\")); return ;;",
            file_subs.join("|")
        );
    }
    let mut words: Vec<String> = Vec::new();
    for spec in FLAGS {
        words.extend(spec.short.map(|c| format!("-{c}")));
        words.push(spec.long.to_owned());
    }
    words.extend(COMMANDS.iter().map(|command| command.name.to_owned()));
    let _ = writeln!(
        out,
        "  esac\n  COMPREPLY=($(compgen -W \"{}\" -- \"$cur\"))\n}}\ncomplete -F _siltop siltop",
        words.join(" ")
    );
    out
}

fn fish() -> String {
    let mut out = String::from("# fish completion for siltop\ncomplete -c siltop -f\n");
    let names: Vec<&str> = COMMANDS.iter().map(|command| command.name).collect();
    let top = format!("not __fish_seen_subcommand_from {}", names.join(" "));
    for spec in FLAGS {
        let mut line = String::from("complete -c siltop");
        if let Some(c) = spec.short {
            let _ = write!(line, " -s {c}");
        }
        let _ = write!(line, " -l {}", spec.long.trim_start_matches("--"));
        match spec.value {
            None => {}
            Some((_, Complete::Text)) => line.push_str(" -x"),
            Some((_, Complete::File)) => line.push_str(" -r -F"),
            Some((_, Complete::Words(words))) => {
                let _ = write!(line, " -x -a {}", quote(&words().join(" ")));
            }
        }
        let _ = writeln!(out, "{line} -d {}", quote(spec.help));
    }
    for command in COMMANDS {
        let _ = writeln!(
            out,
            "complete -c siltop -n {} -a {} -d {}",
            quote(&top),
            command.name,
            quote(command.help)
        );
        let seen = quote(&format!("__fish_seen_subcommand_from {}", command.name));
        for (word, arg, help) in command.subs {
            let _ = writeln!(
                out,
                "complete -c siltop -n {seen} -a {word} -d {}",
                quote(help)
            );
            if !arg.is_empty() {
                let when = quote(&format!(
                    "__fish_seen_subcommand_from {}; and __fish_seen_subcommand_from {word}",
                    command.name
                ));
                let _ = writeln!(out, "complete -c siltop -n {when} -F");
            }
        }
        if command.subs.is_empty() {
            for word in command_words(command) {
                let _ = writeln!(out, "complete -c siltop -n {seen} -a {word}");
            }
        }
        if takes_file(command) {
            let _ = writeln!(out, "complete -c siltop -n {seen} -F");
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cli::{self, FlagId, Mode};

    /// A value the parser accepts for `spec`, so every table row can be fed
    /// back through it.
    fn sample(spec: &FlagSpec) -> Option<String> {
        spec.value.map(|(_, complete)| match (spec.id, complete) {
            (_, Complete::Words(words)) => words()[0].to_owned(),
            (FlagId::Interval, _) => String::from("1s"),
            (FlagId::Addr, _) => String::from("127.0.0.1:9101"),
            (FlagId::Duration | FlagId::From | FlagId::To, _) => String::from("10m"),
            (_, Complete::File) => String::from("/tmp/x.toml"),
            _ => String::from("3"),
        })
    }

    /// The shortest line that runs `command`: the words it cannot do without.
    fn command_args(command: &str) -> Vec<String> {
        let needs: &[&str] = match command {
            "completions" => &["zsh"],
            "replay" => &["t.ptrace"],
            "export" => &["--csv"],
            "fleet" => &["studio"],
            _ => &[],
        };
        std::iter::once(command)
            .chain(needs.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    /// The command a command option needs in front of it to parse.
    fn owner_args(spec: &FlagSpec) -> Vec<String> {
        spec.owner
            .map(|owner| command_args(owner.command))
            .unwrap_or_default()
    }

    #[test]
    fn every_script_names_every_flag_and_command() {
        for shell in Shell::ALL {
            let text = script(shell);
            for spec in FLAGS {
                let long = spec.long.trim_start_matches("--");
                assert!(text.contains(long), "{shell:?} misses {}", spec.long);
                if let Some(c) = spec.short {
                    let needle = match shell {
                        Shell::Fish => format!("-s {c}"),
                        _ => format!("-{c}"),
                    };
                    assert!(text.contains(&needle), "{shell:?} misses -{c}");
                }
            }
            for command in COMMANDS {
                assert!(
                    text.contains(command.name),
                    "{shell:?} misses {}",
                    command.name
                );
                for word in command_words(command) {
                    assert!(text.contains(word), "{shell:?} misses {word}");
                }
            }
        }
    }

    /// Property: every flag the completions offer is one the parser takes,
    /// with a value when the table says it needs one.
    #[test]
    fn every_table_flag_parses() {
        for spec in FLAGS {
            let mut forms = vec![spec.long.to_owned()];
            forms.extend(spec.short.map(|c| format!("-{c}")));
            for form in forms {
                let mut args = owner_args(spec);
                args.push(form.clone());
                args.extend(sample(spec));
                if spec.id == FlagId::Count {
                    args.push(String::from("--json-stream"));
                }
                if spec.id == FlagId::Verbose {
                    args.push(String::from("--version"));
                }
                let parsed = cli::parse(args.clone());
                assert!(parsed.is_ok(), "{args:?}: {parsed:?}");
                if spec.value.is_some() {
                    let mut args = owner_args(spec);
                    args.push(form.clone());
                    let missing = cli::parse(args);
                    assert_eq!(missing, Err(cli::CliError::MissingValue(spec.long)));
                }
            }
        }
    }

    /// Property: the bash word list is exactly the table, and each word
    /// parses, so a completion can never suggest an unknown argument.
    #[test]
    fn bash_word_list_round_trips_through_the_parser() {
        let text = script(Shell::Bash);
        let list = text
            .lines()
            .rev()
            .find(|line| line.contains("compgen -W") && line.contains("--help"))
            .unwrap();
        let words: Vec<&str> = list.split('"').nth(1).unwrap().split_whitespace().collect();
        let expected =
            FLAGS.len() + FLAGS.iter().filter(|f| f.short.is_some()).count() + COMMANDS.len();
        assert_eq!(words.len(), expected);
        for word in words {
            let spec = FLAGS.iter().find(|spec| spec.matches(word));
            let mut args = match spec {
                Some(spec) => {
                    let mut args = owner_args(spec);
                    args.push(word.to_owned());
                    args.extend(sample(spec));
                    args
                }
                None => command_args(word),
            };
            if word == "--count" {
                args.push(String::from("--json-stream"));
            }
            if word == "--verbose" {
                args.push(String::from("-V"));
            }
            assert!(cli::parse(args.clone()).is_ok(), "{args:?}");
        }
    }

    /// Every command in the table parses from its shortest line, and every
    /// sub-word it lists parses too.
    #[test]
    fn every_table_command_and_sub_word_parses() {
        for command in COMMANDS {
            let args = command_args(command.name);
            assert!(cli::parse(args.clone()).is_ok(), "{args:?}");
            for (word, arg, _) in command.subs {
                let mut args = vec![command.name.to_owned(), (*word).to_owned()];
                if !arg.is_empty() {
                    args.push(String::from("/tmp/x"));
                }
                assert!(cli::parse(args.clone()).is_ok(), "{args:?}");
            }
        }
    }

    #[test]
    fn completions_subcommand_parses_every_shell_and_rejects_others() {
        for shell in Shell::ALL {
            let cli = cli::parse(["completions", shell.name()]).unwrap();
            assert_eq!(cli.mode, Mode::Completions(shell));
        }
        assert_eq!(
            cli::parse(["completions"]),
            Err(cli::CliError::NeedsFlag(
                "completions",
                "a shell: zsh, bash or fish"
            ))
        );
        let err = cli::parse(["completions", "powershell"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "completions \"powershell\": expected one of zsh, bash, fish"
        );
    }

    #[test]
    fn replay_completes_files_in_every_shell() {
        assert!(script(Shell::Zsh).contains("replay) _arguments -s $flags '1: :_files' ;;"));
        assert!(script(Shell::Bash).contains("|replay) COMPREPLY=($(compgen -f"));
        assert!(
            script(Shell::Fish)
                .contains("complete -c siltop -n '__fish_seen_subcommand_from replay' -F")
        );
    }

    #[test]
    fn scripts_have_their_shell_headers_and_no_escapes() {
        assert!(script(Shell::Zsh).starts_with("#compdef siltop\n"));
        assert!(script(Shell::Bash).ends_with("complete -F _siltop siltop\n"));
        assert!(script(Shell::Fish).starts_with("# fish completion"));
        for shell in Shell::ALL {
            assert!(!script(shell).contains('\x1b'));
        }
    }

    #[test]
    fn theme_values_complete_in_every_shell() {
        for shell in Shell::ALL {
            let text = script(shell);
            for name in siltop_ui::Theme::builtins() {
                assert!(text.contains(name), "{shell:?} misses theme {name}");
            }
        }
    }

    #[test]
    fn zsh_help_never_breaks_the_optspec() {
        let text = script(Shell::Zsh);
        for line in text.lines().filter(|l| l.contains("--")) {
            // Balanced quotes per line keep `_arguments` from swallowing the
            // next spec (an escaped `'\''` closes and reopens its own).
            let unescaped = line.replace(r"'\''", "");
            assert_eq!(unescaped.matches('\'').count() % 2, 0, "{line}");
        }
    }

    /// Regression: `--addr`'s `HOST:PORT` split its spec, so zsh ran a
    /// command named `port` as the completion action; help lost its `:`
    /// and apostrophes.
    #[test]
    fn zsh_escapes_colons_and_apostrophes_instead_of_splitting_the_spec() {
        let text = script(Shell::Zsh);
        assert!(text.contains(r":host\:port: '"), "{text}");
        assert!(!text.contains(":host:port"), "{text}");
        assert!(!text.contains(" -9101"), "{text}");
        assert_eq!(zsh_escape("it's a:b [x]"), r"it'\''s a\:b (x)");
    }

    /// Regression: `theme import <TAB>` re-offered the sub-words instead of
    /// files, and nothing completed a command's own options after it.
    #[test]
    fn zsh_completes_files_after_file_sub_words_and_flags_after_commands() {
        let text = script(Shell::Zsh);
        assert!(text.contains(
            "theme) _arguments -s $flags '1: :(list import)' '2: :{case $words[2] in (import) _files ;; esac}' ;;"
        ), "{text}");
        assert!(text.contains("line) _arguments -s $flags ;;"), "{text}");
    }

    /// The zsh script parses (where zsh is installed).
    #[test]
    fn zsh_script_is_valid_zsh() {
        let Ok(mut child) = std::process::Command::new("zsh")
            .arg("-n")
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        else {
            return;
        };
        {
            use std::io::Write as _;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(script(Shell::Zsh).as_bytes()).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn quote_escapes_single_quotes() {
        assert_eq!(quote("it's"), r"'it'\''s'");
    }
}
