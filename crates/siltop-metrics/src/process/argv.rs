//! `KERN_PROCARGS2` parsing and argv-aware display names.
//!
//! `python train.py` should read `train.py`, not `python`; a build full of
//! `rustc` rows should say which crate each one compiles.

/// argv beyond this many bytes is dropped (the buffer also carries the
/// environment, which is never read).
pub(crate) const ARGS_CAP: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Procargs {
    pub exec_path: String,
    pub argv: Vec<String>,
}

/// Parses `[i32 argc][exec_path\0][\0 padding][argv0\0]…[argvN\0][env…]`.
/// Reads at most `argc` strings and [`ARGS_CAP`] bytes of them.
pub(crate) fn parse_procargs(buf: &[u8]) -> Option<Procargs> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok().filter(|&n| n >= 1)?;
    let rest = &buf[4..];
    let path_end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    let exec_path = String::from_utf8_lossy(&rest[..path_end]).into_owned();
    let mut i = path_end;
    while rest.get(i) == Some(&0) {
        i += 1;
    }
    let mut strings = Vec::new();
    let mut used = 0_usize;
    while strings.len() < argc && i < rest.len() {
        let end = rest[i..]
            .iter()
            .position(|&b| b == 0)
            .map_or(rest.len(), |n| i + n);
        let arg = &rest[i..end];
        used = used.saturating_add(arg.len() + 1);
        if used > ARGS_CAP {
            break;
        }
        strings.push(String::from_utf8_lossy(arg).into_owned());
        i = end + 1;
    }
    if strings.is_empty() && exec_path.is_empty() {
        return None;
    }
    Some(Procargs {
        exec_path,
        argv: strings,
    })
}

pub(crate) fn basename(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Python,
    Node,
    Ruby,
    Shell,
    Perl,
    Rustc,
    Swift,
    Java,
}

fn tool_of(base: &str) -> Option<Tool> {
    let lower = base.to_ascii_lowercase();
    let stem = lower.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    Some(match stem {
        "python" | "pythonw" => Tool::Python,
        "node" | "bun" | "deno" => Tool::Node,
        "ruby" => Tool::Ruby,
        "bash" | "sh" | "zsh" | "dash" | "fish" | "ksh" => Tool::Shell,
        "perl" => Tool::Perl,
        "rustc" | "clippy-driver" => Tool::Rustc,
        "swift-frontend" => Tool::Swift,
        "java" => Tool::Java,
        _ => return None,
    })
}

/// Options that consume the next argument, per tool.
fn takes_value(tool: Tool, opt: &str) -> bool {
    match tool {
        Tool::Python => matches!(opt, "-W" | "-X" | "--check-hash-based-pycs"),
        Tool::Node => matches!(
            opt,
            "-r" | "--require"
                | "--import"
                | "--loader"
                | "--experimental-loader"
                | "--inspect-port"
        ),
        Tool::Ruby => matches!(opt, "-I" | "-r" | "-C" | "-E" | "--encoding"),
        Tool::Shell => matches!(opt, "-o" | "-O" | "+O" | "+o"),
        Tool::Perl => matches!(opt, "-I" | "-M" | "-m"),
        Tool::Java => matches!(
            opt,
            "-cp"
                | "-classpath"
                | "--class-path"
                | "-p"
                | "--module-path"
                | "--add-opens"
                | "--add-exports"
                | "--add-modules"
        ),
        Tool::Rustc | Tool::Swift => false,
    }
}

/// Options after which the "script" is inline code: keep the tool name.
fn inline_code(tool: Tool, opt: &str) -> bool {
    match tool {
        Tool::Python => opt == "-c" || (opt.starts_with("-c") && !opt.starts_with("--")),
        Tool::Node => matches!(opt, "-e" | "--eval" | "-p" | "--print"),
        Tool::Ruby | Tool::Perl => opt == "-e" || opt == "-E",
        Tool::Shell => {
            opt == "-c" || (opt.starts_with('-') && !opt.starts_with("--") && opt.contains('c'))
        }
        _ => false,
    }
}

/// The best short name for a process, from its argv when that says more
/// than the executable does. `None` means "use the executable name".
pub(crate) fn display_name(argv: &[String]) -> Option<String> {
    let argv0 = argv.first()?;
    let tool = tool_of(basename(argv0))?;
    let rest = &argv[1..];
    match tool {
        Tool::Rustc => flag_value(rest, "--crate-name").map(|c| format!("rustc {c}")),
        Tool::Swift => {
            flag_value(rest, "-primary-file").map(|f| format!("swift-frontend {}", basename(f)))
        }
        Tool::Java => java_target(rest),
        _ => script_target(tool, rest),
    }
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == flag {
            return it.next().map(String::as_str).filter(|v| !v.is_empty());
        }
        if let Some(v) = arg.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            return Some(v).filter(|v| !v.is_empty());
        }
    }
    None
}

fn script_target(tool: Tool, args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            return it.next().map(|s| basename(s).to_owned());
        }
        if tool == Tool::Python {
            if arg == "-m" {
                return it.next().filter(|m| !m.is_empty()).cloned();
            }
            if let Some(module) = arg.strip_prefix("-m").filter(|m| !m.is_empty()) {
                return Some(module.to_owned());
            }
        }
        if inline_code(tool, arg) {
            return None;
        }
        if takes_value(tool, arg) {
            it.next();
            continue;
        }
        if arg.starts_with('-') || (tool == Tool::Shell && arg.starts_with('+')) {
            continue;
        }
        let base = basename(arg);
        return (!base.is_empty()).then(|| base.to_owned());
    }
    None
}

fn java_target(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "-jar" {
            return it.next().map(|j| basename(j).to_owned());
        }
        if arg == "-m" || arg == "--module" {
            return it.next().cloned();
        }
        if takes_value(Tool::Java, arg) {
            it.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.rsplit('.').next().unwrap_or(arg).to_owned());
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Real argv captured with `ps -axww -o args=` on the Mac17,8 plus the
    /// common toolchain shapes; `None` keeps the executable name.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn display_name_fixture_table() {
        let table: &[(&[&str], Option<&str>)] = &[
            (
                &[
                    "/Applications/Xcode.app/Contents/Developer/Library/Frameworks/Python3.framework/Versions/3.9/Resources/Python.app/Contents/MacOS/Python",
                    "/Users/cloaky/dev/siltop/scripts/pty-shot.py",
                    "--no-png",
                ],
                Some("pty-shot.py"),
            ),
            (&["python3", "train.py", "--epochs", "3"], Some("train.py")),
            (
                &["python3.12", "-u", "-W", "ignore", "serve.py"],
                Some("serve.py"),
            ),
            (
                &["python3", "-m", "mlx_lm.server", "--port", "8080"],
                Some("mlx_lm.server"),
            ),
            (&["python", "-mpytest", "-q"], Some("pytest")),
            (&["python3", "-c", "import time; time.sleep(9)"], None),
            (&["python3"], None),
            (
                &[
                    "/opt/homebrew/bin/node",
                    "/opt/homebrew/bin/npm",
                    "run",
                    "dev",
                ],
                Some("npm"),
            ),
            (
                &[
                    "node",
                    "--require",
                    "ts-node/register",
                    "./node_modules/.bin/vite",
                ],
                Some("vite"),
            ),
            (&["node", "-e", "setInterval(()=>{},1e3)"], None),
            (&["node", "--inspect", "server.js"], Some("server.js")),
            (&["ruby", "-I", "lib", "bin/rails", "server"], Some("rails")),
            (&["ruby", "-e", "loop{}"], None),
            (&["/bin/bash", "./run.sh"], Some("run.sh")),
            (&["bash", "-c", "sleep 2; exec yes"], None),
            (&["/bin/zsh", "-c", "source ~/.zshrc"], None),
            (&["/bin/zsh", "-lc", "make"], None),
            (&["-zsh"], None),
            (&["bash", "-o", "pipefail", "ci.sh"], Some("ci.sh")),
            (&["sh", "--", "-weird.sh"], Some("-weird.sh")),
            (
                &[
                    "/usr/bin/perl",
                    "/Applications/Wispr Flow.app/Contents/Resources/run.pl",
                    "/Applications/Wispr Flow.app/Contents/x",
                ],
                Some("run.pl"),
            ),
            (
                &[
                    "/Users/cloaky/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc",
                    "--crate-name",
                    "siltop_metrics",
                    "--edition=2024",
                    "crates/siltop-metrics/src/lib.rs",
                ],
                Some("rustc siltop_metrics"),
            ),
            (
                &["rustc", "--crate-name=serde_derive", "src/lib.rs"],
                Some("rustc serde_derive"),
            ),
            (&["rustc", "-vV"], None),
            (
                &["clippy-driver", "rustc", "--crate-name", "siltop_ui"],
                Some("rustc siltop_ui"),
            ),
            (
                &[
                    "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/swift-frontend",
                    "-frontend",
                    "-c",
                    "-primary-file",
                    "/src/App/ContentView.swift",
                ],
                Some("swift-frontend ContentView.swift"),
            ),
            (
                &[
                    "java",
                    "-Xmx2g",
                    "-jar",
                    "/opt/minecraft/server.jar",
                    "nogui",
                ],
                Some("server.jar"),
            ),
            (
                &["java", "-cp", "a.jar:b.jar", "com.example.Main"],
                Some("Main"),
            ),
            (
                &[
                    "java",
                    "-m",
                    "jdk.jshell/jdk.internal.jshell.tool.JShellToolProvider",
                ],
                Some("jdk.jshell/jdk.internal.jshell.tool.JShellToolProvider"),
            ),
            (&["/Applications/Safari.app/Contents/MacOS/Safari"], None),
            (&["claude", "--resume"], None),
            (&["python3", "\u{1f40d}.py"], Some("\u{1f40d}.py")),
        ];
        assert!(table.len() >= 20);
        for (argv, want) in table {
            let got = display_name(&v(argv));
            assert_eq!(got.as_deref(), *want, "argv {argv:?}");
        }
    }

    #[test]
    fn empty_argv_has_no_display_name() {
        assert_eq!(display_name(&[]), None);
        assert_eq!(display_name(&v(&[""])), None);
    }

    fn procargs(argc: i32, exec: &str, pad: usize, words: &[&str], env: &[&str]) -> Vec<u8> {
        let mut b = argc.to_ne_bytes().to_vec();
        b.extend_from_slice(exec.as_bytes());
        b.push(0);
        b.extend(std::iter::repeat_n(0, pad));
        for a in words.iter().chain(env) {
            b.extend_from_slice(a.as_bytes());
            b.push(0);
        }
        b
    }

    #[test]
    fn procargs_reads_argc_strings_and_never_the_environment() {
        let buf = procargs(
            3,
            "/usr/bin/python3",
            7,
            &["python3", "train.py", "--fast"],
            &["HOME=/Users/x", "SECRET=1"],
        );
        let p = parse_procargs(&buf).unwrap();
        assert_eq!(p.exec_path, "/usr/bin/python3");
        assert_eq!(p.argv, v(&["python3", "train.py", "--fast"]));
    }

    #[test]
    fn procargs_keeps_empty_arguments() {
        let buf = procargs(3, "/bin/echo", 0, &["echo", "", "x"], &[]);
        assert_eq!(parse_procargs(&buf).unwrap().argv, v(&["echo", "", "x"]));
    }

    #[test]
    fn procargs_rejects_short_or_zero_argc_buffers() {
        assert_eq!(parse_procargs(&[]), None);
        assert_eq!(parse_procargs(&[1, 0]), None);
        assert_eq!(parse_procargs(&procargs(0, "/bin/x", 0, &["x"], &[])), None);
        assert_eq!(
            parse_procargs(&procargs(-4, "/bin/x", 0, &["x"], &[])),
            None
        );
    }

    #[test]
    fn procargs_tolerates_truncation_and_missing_terminator() {
        let mut buf = procargs(2, "/bin/x", 3, &["x", "tail"], &[]);
        buf.pop();
        let p = parse_procargs(&buf).unwrap();
        assert_eq!(p.argv, v(&["x", "tail"]));
        // argc larger than the strings present.
        let p = parse_procargs(&procargs(9, "/bin/x", 0, &["x"], &[])).unwrap();
        assert_eq!(p.argv, v(&["x"]));
    }

    #[test]
    fn procargs_caps_argv_at_four_kib() {
        let big = "a".repeat(1000);
        let args: Vec<&str> = std::iter::repeat_n(big.as_str(), 10).collect();
        let p = parse_procargs(&procargs(10, "/bin/x", 0, &args, &[])).unwrap();
        assert_eq!(p.argv.len(), 4, "4 × 1001 bytes fit in 4096");
        let total: usize = p.argv.iter().map(|a| a.len() + 1).sum();
        assert!(total <= ARGS_CAP);
    }

    #[test]
    fn procargs_sweep_never_panics_on_garbage() {
        let mut seed = 0x9e37_79b9_u32;
        for len in 0..600 {
            let buf: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed % 7) as u8 // lots of NULs and tiny argc values
                })
                .collect();
            if let Some(p) = parse_procargs(&buf) {
                let total: usize = p.argv.iter().map(|a| a.len() + 1).sum();
                assert!(total <= ARGS_CAP);
                let _ = display_name(&p.argv);
            }
        }
    }

    #[test]
    fn basename_edges() {
        assert_eq!(
            basename("/Applications/Xcode.app/Contents/MacOS/Xcode"),
            "Xcode"
        );
        assert_eq!(basename("kernel_task"), "kernel_task");
        assert_eq!(basename("/"), "/");
        assert_eq!(basename("dir/"), "dir/");
        assert_eq!(basename(""), "");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_own_argv_round_trips() {
        let own = super::super::macos::procargs(std::process::id()).unwrap();
        let want: Vec<String> = std::env::args().collect();
        assert_eq!(own.argv, want);
        assert!(own.exec_path.contains('/'));
    }
}
