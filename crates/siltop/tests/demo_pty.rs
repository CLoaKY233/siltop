//! `--demo` in a real pseudo-terminal: it must go straight to the dashboard,
//! never to the first-run screens, and leave no config file behind.

#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SCRIPT: &str = "/usr/bin/script";

fn files_under(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path.display().to_string());
        }
    }
    found
}

#[test]
fn demo_in_a_pty_skips_onboarding_and_writes_nothing() {
    if !Path::new(SCRIPT).exists() {
        return;
    }
    let home = std::env::temp_dir().join(format!("siltop-demo-pty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let mut child = Command::new(SCRIPT)
        .args(["-q", "/dev/null", env!("CARGO_BIN_EXE_siltop"), "--demo"])
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".state"))
        .env("TERM", "xterm-256color")
        .env_remove("CI")
        .env_remove("SILTOP_NO_ONBOARDING")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    // Esc would skip onboarding and save that choice, so a config file
    // appears if the screens showed; then `q` quits the dashboard. The
    // first `q` can be read together with the Esc, so it is sent twice.
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(b"\x1b").and_then(|()| stdin.flush());
    for _ in 0..2 {
        std::thread::sleep(Duration::from_millis(700));
        let _ = stdin.write_all(b"q").and_then(|()| stdin.flush());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let written = files_under(&home);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        status.is_some_and(|s| s.success()),
        "did not quit: {status:?}"
    );
    assert!(written.is_empty(), "--demo wrote {written:?}");
}
