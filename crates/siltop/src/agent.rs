//! `siltop agent`: a per-user `LaunchAgent` that samples this Mac every
//! few seconds with no window open, evaluates alert rules, feeds the
//! history store and keeps a tiny state file for `siltop line`.
//!
//! No sudo: the plist lives in `~/Library/LaunchAgents` and is loaded into
//! the `gui/<uid>` domain. Without the agent, alerts run only while the
//! dashboard is open.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use siltop_core::{Config, Result, RuleSpec, Snapshot, VerdictEngine, battery_drain_w};

use crate::cli::AgentAction;
use crate::line::Summary;
use crate::sinks::{Delivery, Watch};

pub const LABEL: &str = "com.siltop.agent";
/// The agent's cadence: slow enough to cost next to nothing.
pub const AGENT_INTERVAL: Duration = Duration::from_secs(5);

/// `~/Library/LaunchAgents/com.siltop.agent.plist`.
#[must_use]
pub fn plist_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LABEL}.plist")),
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The XDG variables that place the config, history and
/// `agent.json`. launchd starts agents without the user's shell
/// environment, so `install` bakes the values it resolved into the plist;
/// otherwise the agent would read a different config and state than the
/// CLI that installed it.
pub const INHERITED_ENV: [&str; 2] = ["XDG_CONFIG_HOME", "XDG_STATE_HOME"];

/// [`INHERITED_ENV`] as `var` sees it: set, non-empty, made absolute.
#[must_use]
pub fn inherited_env(var: impl Fn(&str) -> Option<OsString>) -> Vec<(&'static str, PathBuf)> {
    INHERITED_ENV
        .iter()
        .filter_map(|&name| {
            let value = var(name).filter(|value| !value.is_empty())?;
            Some((name, absolute(Path::new(&value))))
        })
        .collect()
}

/// `path` resolved against the current directory now, because launchd
/// starts agents with `/` as their working directory.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The `LaunchAgent` property list: start at login, restart only after a
/// crash, run as a background (low-priority, efficiency-core) process,
/// with the installer's `env` (see [`inherited_env`]) and `config` made
/// absolute.
#[must_use]
pub fn plist(
    exe: &Path,
    config: Option<&Path>,
    log: Option<&Path>,
    env: &[(&str, PathBuf)],
) -> String {
    let mut args = vec![exe.to_string_lossy().into_owned()];
    if let Some(config) = config {
        args.push(String::from("--config"));
        args.push(absolute(config).to_string_lossy().into_owned());
    }
    args.extend([String::from("agent"), String::from("run")]);
    let args = args.iter().fold(String::new(), |mut out, a| {
        let _ = writeln!(out, "\t\t<string>{}</string>", xml_escape(a));
        out
    });
    let log = log.map_or_else(String::new, |log| {
        let log = xml_escape(&log.to_string_lossy());
        format!(
            "\t<key>StandardOutPath</key>\n\t<string>{log}</string>\n\
             \t<key>StandardErrorPath</key>\n\t<string>{log}</string>\n"
        )
    });
    let env = if env.is_empty() {
        String::new()
    } else {
        let vars = env.iter().fold(String::new(), |mut out, (name, value)| {
            let _ = writeln!(
                out,
                "\t\t<key>{}</key>\n\t\t<string>{}</string>",
                xml_escape(name),
                xml_escape(&value.to_string_lossy())
            );
            out
        });
        format!("\t<key>EnvironmentVariables</key>\n\t<dict>\n{vars}\t</dict>\n")
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>Label</key>\n\t<string>{LABEL}</string>\n\
\t<key>ProgramArguments</key>\n\t<array>\n{args}\t</array>\n\
\t<key>RunAtLoad</key>\n\t<true/>\n\
\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n\
\t<key>ThrottleInterval</key>\n\t<integer>30</integer>\n\
\t<key>ProcessType</key>\n\t<string>Background</string>\n\
\t<key>LowPriorityIO</key>\n\t<true/>\n\
\t<key>Nice</key>\n\t<integer>10</integer>\n\
{env}{log}</dict>\n\
</plist>\n"
    )
}

/// The `gui/<uid>` launchd domain of the user running this.
fn gui_domain() -> String {
    // SAFETY: getuid takes no arguments, cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}")
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("/bin/launchctl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The agent's state kept between snapshots.
pub struct Agent {
    verdict: VerdictEngine,
    watch: Watch,
    specs: Vec<RuleSpec>,
}

impl Agent {
    #[must_use]
    pub fn new(config: &Config) -> Self {
        Self {
            verdict: VerdictEngine::default(),
            // No terminal: the agent runs under launchd.
            watch: Watch::new(&config.alerts, false),
            specs: config.alerts.clone(),
        }
    }

    /// Judges one snapshot: the summary to write and what to deliver.
    pub fn step(
        &mut self,
        snap: &Snapshot,
        now: Instant,
        unix_ms: u64,
    ) -> (Summary, Vec<Delivery>) {
        let drain = battery_drain_w(&snap.battery);
        let verdict = self.verdict.update(snap, now, drain);
        let mut summary = Summary::from_snapshot(snap, verdict, unix_ms);
        let deliveries = self.watch.observe(&self.specs, snap, now);
        summary.alerts = self.watch.active().map(String::from).collect();
        (summary, deliveries)
    }
}

fn write_state(path: &Path, summary: &Summary) -> std::io::Result<()> {
    let text = serde_json::to_string(summary).map_err(std::io::Error::other)?;
    crate::config_io::atomic_write_private(path, &text)
}

/// The agent's loop: sample, judge, deliver, record into the history
/// store under `history`. Runs until `stop` answers true (launchd sends
/// SIGTERM on unload and logout) or `ticks` runs out. Returning drops the
/// recorder, which writes the open history bucket: a stop never loses the
/// last few seconds.
pub fn run_loop<T, S>(
    tick: &mut T,
    wait: &mut S,
    stop: &dyn Fn() -> bool,
    config: &Config,
    history: Option<&Path>,
    state: Option<&Path>,
    ticks: Option<u64>,
) -> Result<()>
where
    T: FnMut() -> Result<Snapshot>,
    S: FnMut(Duration),
{
    let mut agent = Agent::new(config);
    let (mut recorder, _warning) = crate::store_disk::Recorder::new(history);
    let _prime = tick()?;
    let mut done = 0_u64;
    while ticks.is_none_or(|n| done < n) && !stop() {
        wait(AGENT_INTERVAL);
        // A stop during the wait samples nothing more.
        if stop() {
            break;
        }
        let snap = tick()?;
        let unix_ms = crate::headless::unix_ms_now();
        let (summary, deliveries) = agent.step(&snap, Instant::now(), unix_ms);
        for delivery in &deliveries {
            delivery.send();
        }
        let _ = recorder.observe(&snap, unix_ms / 1000);
        if let Some(path) = state {
            let _ = write_state(path, &summary);
        }
        done += 1;
    }
    Ok(())
}

/// `install`, `uninstall` and `status`; `run` is dispatched by the caller
/// because it needs a sampler.
pub fn manage(
    action: AgentAction,
    config_path: Option<&Path>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let Some(plist_file) = plist_path() else {
        let _ = writeln!(err, "siltop: no home directory for the LaunchAgent");
        return 1;
    };
    let target = format!("{}/{LABEL}", gui_domain());
    match action {
        AgentAction::Install => {
            let exe = match std::env::current_exe() {
                Ok(exe) => exe,
                Err(error) => {
                    let _ = writeln!(err, "siltop: {error}");
                    return 1;
                }
            };
            let log = crate::line::state_path().map(|p| p.with_file_name("agent.log"));
            if let Some(config) = config_path.filter(|c| !c.exists()) {
                let _ = writeln!(
                    err,
                    "siltop: warning: {} does not exist; the agent uses the defaults until it does",
                    absolute(config).display()
                );
            }
            let env = inherited_env(|name| std::env::var_os(name));
            let text = plist(&exe, config_path, log.as_deref(), &env);
            if let Err(error) = crate::config_io::atomic_write(&plist_file, &text) {
                let _ = writeln!(err, "siltop: write {}: {error}", plist_file.display());
                return 1;
            }
            let _ = launchctl(&["bootout", &target]);
            let plist_arg = plist_file.to_string_lossy();
            if !launchctl(&["bootstrap", &gui_domain(), &plist_arg]) {
                let _ = writeln!(err, "siltop: launchctl bootstrap failed");
                return 1;
            }
            let _ = writeln!(out, "agent installed: {}", plist_file.display());
            0
        }
        AgentAction::Uninstall => {
            let loaded = launchctl(&["bootout", &target]);
            let removed = std::fs::remove_file(&plist_file).is_ok();
            let _ = writeln!(
                out,
                "{}",
                if loaded || removed {
                    "agent removed"
                } else {
                    "agent was not installed"
                }
            );
            0
        }
        AgentAction::Status => {
            let installed = plist_file.exists();
            let running = installed && launchctl(&["print", &target]);
            let fresh = crate::line::state_path()
                .and_then(|p| crate::line::read_fresh(&p, crate::headless::unix_ms_now()))
                .is_some();
            let _ = writeln!(out, "{}", status_line(installed, running, fresh));
            0
        }
        AgentAction::Run => 2,
    }
}

/// The one line `agent status` prints.
#[must_use]
pub fn status_line(installed: bool, running: bool, fresh: bool) -> String {
    let state = match (installed, running, fresh) {
        (false, ..) => "not installed",
        (true, false, _) => "installed, not running",
        (true, true, false) => "running, no recent sample",
        (true, true, true) => "running",
    };
    format!("agent: {state}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::Thermal;

    #[test]
    fn plist_runs_agent_run_in_the_background_at_login() {
        let text = plist(Path::new("/opt/homebrew/bin/siltop"), None, None, &[]);
        assert!(text.starts_with("<?xml version=\"1.0\""));
        assert!(!text.contains("EnvironmentVariables"));
        assert!(text.contains("<string>com.siltop.agent</string>"));
        assert!(text.contains(
            "\t\t<string>/opt/homebrew/bin/siltop</string>\n\t\t<string>agent</string>\n\t\t<string>run</string>\n"
        ));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(text.contains("<string>Background</string>"));
        assert!(!text.contains("StandardOutPath"));
        assert!(text.trim_end().ends_with("</plist>"));
        assert_eq!(
            text.matches("<dict>").count(),
            text.matches("</dict>").count()
        );
    }

    #[test]
    fn plist_escapes_paths_and_passes_the_config() {
        let text = plist(
            Path::new("/Users/a&b/<bin>/siltop"),
            Some(Path::new("/tmp/c.toml")),
            Some(Path::new("/tmp/agent.log")),
            &[],
        );
        assert!(text.contains("<string>/Users/a&amp;b/&lt;bin&gt;/siltop</string>"));
        assert!(text.contains("<string>--config</string>\n\t\t<string>/tmp/c.toml</string>"));
        assert!(text.contains("<key>StandardErrorPath</key>\n\t<string>/tmp/agent.log</string>"));
    }

    #[test]
    fn plist_bakes_the_installers_xdg_dirs_so_the_agent_reads_the_same_config_and_state() {
        // Regression: the plist carried no environment, so under launchd the
        // agent looked in ~/.config and ~/.local/state while the CLI that
        // installed it used $XDG_CONFIG_HOME / $XDG_STATE_HOME.
        let env = inherited_env(|name| match name {
            "XDG_CONFIG_HOME" => Some(OsString::from("/Users/j/dot&files/config")),
            "XDG_STATE_HOME" => Some(OsString::new()),
            _ => None,
        });
        assert_eq!(
            env,
            [(
                "XDG_CONFIG_HOME",
                PathBuf::from("/Users/j/dot&files/config")
            )]
        );
        let text = plist(Path::new("/bin/siltop"), None, None, &env);
        assert!(text.contains(
            "\t<key>EnvironmentVariables</key>\n\t<dict>\n\
             \t\t<key>XDG_CONFIG_HOME</key>\n\t\t<string>/Users/j/dot&amp;files/config</string>\n\
             \t</dict>\n"
        ));
        assert_eq!(
            text.matches("<dict>").count(),
            text.matches("</dict>").count()
        );
        let cwd = std::env::current_dir().unwrap();
        let relative = inherited_env(|_| Some(OsString::from("state")));
        assert!(relative.iter().all(|(_, dir)| *dir == cwd.join("state")));
    }

    #[test]
    fn plist_bakes_an_absolute_config_path_since_launchd_starts_agents_in_root() {
        // Regression: `--config ./p.toml agent install` wrote the relative
        // path, which the agent then resolved against `/`.
        let text = plist(
            Path::new("/bin/siltop"),
            Some(Path::new("p.toml")),
            None,
            &[],
        );
        let cwd = std::env::current_dir().unwrap().join("p.toml");
        let expected = format!(
            "<string>--config</string>\n\t\t<string>{}</string>",
            xml_escape(&cwd.to_string_lossy())
        );
        assert!(text.contains(&expected), "{text}");
    }

    #[test]
    fn status_line_table() {
        assert_eq!(status_line(false, false, false), "agent: not installed");
        assert_eq!(
            status_line(true, false, false),
            "agent: installed, not running"
        );
        assert_eq!(status_line(true, true, true), "agent: running");
        assert_eq!(
            status_line(true, true, false),
            "agent: running, no recent sample"
        );
    }

    #[test]
    fn step_fires_rules_and_lists_them_in_the_summary() {
        let config = Config {
            alerts: vec![RuleSpec {
                id: String::from("hot"),
                sustain_s: Some(0),
                ..RuleSpec::default()
            }],
            ..Config::default()
        };
        let mut agent = Agent::new(&config);
        let mut snap = Snapshot::empty();
        snap.primed = true;
        snap.thermal = Thermal::Serious;
        let (summary, deliveries) = agent.step(&snap, Instant::now(), 42);
        assert_eq!(summary.alerts, ["hot"]);
        assert_eq!(summary.unix_ms, 42);
        assert_eq!(summary.thermal, 2);
        assert_eq!(deliveries.len(), 1);
        assert!(
            deliveries[0].terminal.is_none(),
            "the agent has no terminal"
        );
        assert!(deliveries[0].notify.is_some());
    }

    #[test]
    fn run_loop_writes_a_fresh_state_file_line_can_read() {
        let dir = std::env::temp_dir().join(format!("siltop-agent-{}", std::process::id()));
        let path = dir.join("agent.json");
        let mut tick = || {
            let mut snap = Snapshot::empty();
            snap.primed = true;
            snap.cpu.scaled = 0.25;
            Ok(snap)
        };
        let mut waited = Vec::new();
        let mut wait = |d: Duration| waited.push(d);
        run_loop(
            &mut tick,
            &mut wait,
            &|| false,
            &Config::default(),
            None,
            Some(&path),
            Some(2),
        )
        .unwrap();
        assert_eq!(waited, [AGENT_INTERVAL, AGENT_INTERVAL]);
        let summary = crate::line::read_fresh(&path, crate::headless::unix_ms_now()).unwrap();
        assert_eq!(summary.cpu_pct, Some(25.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: launchd's SIGTERM killed `agent run` (exit 143) and the
    /// open history bucket was lost. A stop now ends the loop cleanly and
    /// the partial bucket reaches the store.
    #[test]
    fn a_stop_ends_the_loop_and_keeps_the_open_history_bucket() {
        use siltop_core::store::{HistoryStore as _, Tier};
        let dir = std::env::temp_dir().join(format!(
            "siltop-agent-stop-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let history = dir.join("history");
        let mut tick = || {
            let mut snap = Snapshot::empty();
            snap.primed = true;
            snap.cpu.scaled = 0.5;
            snap.cpu.active = 0.5;
            Ok(snap)
        };
        let waits = std::cell::Cell::new(0);
        let mut wait = |_| waits.set(waits.get() + 1);
        // The signal lands during the second wait.
        let stop = || waits.get() >= 2;
        run_loop(
            &mut tick,
            &mut wait,
            &stop,
            &Config::default(),
            Some(&history),
            None,
            None,
        )
        .unwrap();
        assert_eq!(waits.get(), 2, "no tick after the stop");
        let store =
            crate::store_disk::DiskStore::open(&history, crate::store_disk::SIZE_CAP).unwrap();
        let now = crate::store_disk::unix_now();
        let kept = store.buckets(now - 3600, now + 3600, Tier::Day).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].samples, 1);
    }
}
