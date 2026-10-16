//! Delivering fired alerts: a macOS notification, a terminal notification
//! (OSC 9 and OSC 777, then a bell) while a TUI owns the terminal, and a
//! user shell hook. Deciding what to send is pure ([`Courier::plan`]);
//! only [`Delivery::send`] spawns anything.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use siltop_core::{
    AlertEngine, Fired, RuleSpec, Signals, Snapshot, battery_drain_w, effective_rules,
};

/// At most one macOS notification this often, whatever fires: `osascript`
/// is a process spawn, and a burst of banners helps nobody.
pub const NOTIFY_GAP: Duration = Duration::from_secs(30);

/// Control characters would end or corrupt an escape sequence.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The `AppleScript` that shows one notification.
#[must_use]
pub fn notify_script(title: &str, body: &str) -> String {
    let quote = |text: &str| clean(text).replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "display notification \"{}\" with title \"siltop\" subtitle \"{}\"",
        quote(body),
        quote(title)
    )
}

/// OSC 9 (`iTerm2`, Ghostty, kitty, `WezTerm`), OSC 777 (foot, Konsole,
/// Ghostty) and BEL. A terminal ignores the one it does not know.
#[must_use]
pub fn terminal_sequence(title: &str, body: &str) -> String {
    let title = clean(title);
    let body = clean(body);
    format!(
        "\x1b]9;{title}: {body}\x07\x1b]777;notify;{};{}\x07\x07",
        title.replace(';', ","),
        body.replace(';', ",")
    )
}

/// The environment a hook command sees.
#[must_use]
pub fn hook_env(fired: &Fired) -> [(&'static str, String); 3] {
    [
        ("SILTOP_ALERT_ID", fired.id.clone()),
        ("SILTOP_ALERT_TITLE", fired.title.clone()),
        ("SILTOP_ALERT_BODY", fired.body.clone()),
    ]
}

/// What one fired alert turns into.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Delivery {
    pub notify: Option<String>,
    pub terminal: Option<String>,
    pub hook: Option<(String, [(&'static str, String); 3])>,
}

impl Delivery {
    /// Spawns the notification and the hook; never waits on either. The
    /// terminal sequence is the caller's to write after its next draw.
    pub fn send(&self) {
        #[cfg(target_os = "macos")]
        if let Some(script) = &self.notify {
            reap(
                Command::new("/usr/bin/osascript")
                    .arg("-e")
                    .arg(script)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn(),
            );
        }
        if let Some((hook, env)) = &self.hook {
            reap(
                Command::new("/bin/sh")
                    .arg("-c")
                    .arg(hook)
                    .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn(),
            );
        }
    }
}

/// Waits for a spawned child on a throwaway thread, so it never lingers
/// as a zombie and the caller never blocks.
fn reap(child: std::io::Result<std::process::Child>) {
    if let Ok(mut child) = child {
        let _ = std::thread::Builder::new()
            .name(String::from("siltop-sink"))
            .spawn(move || child.wait());
    }
}

/// Turns fired alerts into deliveries, rate-limiting notifications.
#[derive(Debug, Clone, Default)]
pub struct Courier {
    last_notify: Option<Instant>,
    /// A terminal is ours to write to (the TUI is running).
    pub terminal: bool,
}

impl Courier {
    #[must_use]
    pub fn new(terminal: bool) -> Self {
        Self {
            last_notify: None,
            terminal,
        }
    }

    pub fn plan(&mut self, fired: &Fired, now: Instant) -> Delivery {
        let notify_ok = fired.sinks.notify
            && self
                .last_notify
                .is_none_or(|at| now.saturating_duration_since(at) >= NOTIFY_GAP);
        if notify_ok {
            self.last_notify = Some(now);
        }
        Delivery {
            notify: notify_ok.then(|| notify_script(&fired.title, &fired.body)),
            terminal: (self.terminal && fired.sinks.terminal)
                .then(|| terminal_sequence(&fired.title, &fired.body)),
            hook: fired.sinks.hook.clone().map(|hook| (hook, hook_env(fired))),
        }
    }
}

/// Alert rules kept in step with the config, judged snapshot by snapshot:
/// what both the TUI and the agent run.
#[derive(Debug, Clone)]
pub struct Watch {
    specs: Vec<RuleSpec>,
    engine: AlertEngine,
    courier: Courier,
}

impl Watch {
    #[must_use]
    pub fn new(specs: &[RuleSpec], terminal: bool) -> Self {
        Self {
            specs: specs.to_vec(),
            engine: AlertEngine::new(effective_rules(specs)),
            courier: Courier::new(terminal),
        }
    }

    /// Judges one snapshot and plans what fired. A changed rule set (a
    /// settings toggle) keeps each unchanged rule's state, so a rule still
    /// firing is not delivered again; the notification limit stays.
    pub fn observe(&mut self, specs: &[RuleSpec], snap: &Snapshot, now: Instant) -> Vec<Delivery> {
        if specs != self.specs.as_slice() {
            specs.clone_into(&mut self.specs);
            self.engine.reconfigure(effective_rules(specs));
        }
        let signals = Signals {
            swap_growth_bps: None,
            battery_drain_w: battery_drain_w(&snap.battery),
        };
        self.engine
            .update(snap, &signals, now)
            .iter()
            .map(|fired| self.courier.plan(fired, now))
            .collect()
    }

    /// Ids of rules that fired and have not cleared.
    pub fn active(&self) -> impl Iterator<Item = &str> {
        self.engine.active()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::{Sinks, Thermal};

    #[test]
    fn watch_follows_a_settings_toggle() {
        let mut snap = Snapshot::empty();
        snap.thermal = Thermal::Critical;
        let t0 = Instant::now();
        let quiet = vec![RuleSpec {
            id: String::from("hot"),
            enabled: Some(false),
            ..RuleSpec::default()
        }];
        let mut watch = Watch::new(&quiet, true);
        assert!(watch.observe(&quiet, &snap, t0).is_empty());
        let loud = vec![RuleSpec {
            id: String::from("hot"),
            sustain_s: Some(0),
            ..RuleSpec::default()
        }];
        let fired = watch.observe(&loud, &snap, t0);
        assert_eq!(fired.len(), 1);
        assert!(fired[0].terminal.is_some());
        assert_eq!(watch.active().collect::<Vec<_>>(), ["hot"]);
    }

    /// Regression: toggling one rule rebuilt the engine, so another rule
    /// still firing was delivered again a tick later.
    #[test]
    fn toggling_another_rule_does_not_refire_an_active_one() {
        let mut snap = Snapshot::empty();
        snap.thermal = Thermal::Critical;
        let t0 = Instant::now();
        let hot = RuleSpec {
            id: String::from("hot"),
            sustain_s: Some(0),
            ..RuleSpec::default()
        };
        let mut watch = Watch::new(std::slice::from_ref(&hot), true);
        assert_eq!(
            watch.observe(std::slice::from_ref(&hot), &snap, t0).len(),
            1
        );
        let toggled = vec![
            hot,
            RuleSpec {
                id: String::from("busy"),
                enabled: Some(true),
                ..RuleSpec::default()
            },
        ];
        let later = t0 + Duration::from_secs(5);
        assert!(watch.observe(&toggled, &snap, later).is_empty());
        assert_eq!(watch.active().collect::<Vec<_>>(), ["hot"]);
    }

    fn fired(sinks: Sinks) -> Fired {
        Fired {
            id: String::from("hot"),
            title: String::from("Mac is running hot"),
            body: String::from("thermal pressure serious"),
            sinks,
        }
    }

    #[test]
    fn notify_script_escapes_quotes_backslashes_and_controls() {
        let script = notify_script("a \"b\"", "c\\d\ne");
        assert_eq!(
            script,
            "display notification \"c\\\\d e\" with title \"siltop\" subtitle \"a \\\"b\\\"\""
        );
    }

    #[test]
    fn terminal_sequence_is_osc9_osc777_and_bell_with_no_stray_controls() {
        let seq = terminal_sequence("Hot; very", "cpu\x1b[31m 99 °C");
        assert!(
            seq.starts_with("\x1b]9;Hot; very: cpu [31m 99 °C\x07"),
            "{seq:?}"
        );
        assert!(seq.contains("\x1b]777;notify;Hot, very;cpu [31m 99 °C\x07"));
        assert!(seq.ends_with("\x07\x07"));
        assert_eq!(seq.matches('\x1b').count(), 2, "only our own escapes");
    }

    #[test]
    fn notifications_are_rate_limited_but_hooks_and_terminal_are_not() {
        let mut courier = Courier::new(true);
        let t0 = Instant::now();
        let sinks = Sinks {
            hook: Some(String::from("echo $SILTOP_ALERT_ID")),
            ..Sinks::standard()
        };
        let first = courier.plan(&fired(sinks.clone()), t0);
        assert!(first.notify.is_some() && first.terminal.is_some() && first.hook.is_some());
        let second = courier.plan(&fired(sinks.clone()), t0 + Duration::from_secs(5));
        assert!(second.notify.is_none(), "inside the gap");
        assert!(second.terminal.is_some() && second.hook.is_some());
        let third = courier.plan(&fired(sinks), t0 + NOTIFY_GAP);
        assert!(third.notify.is_some());
    }

    #[test]
    fn headless_agent_never_writes_terminal_sequences() {
        let mut courier = Courier::new(false);
        let plan = courier.plan(&fired(Sinks::standard()), Instant::now());
        assert!(plan.terminal.is_none());
        assert!(plan.notify.is_some());
    }

    #[test]
    fn hook_env_names_the_alert() {
        let env = hook_env(&fired(Sinks::default()));
        assert_eq!(env[0], ("SILTOP_ALERT_ID", String::from("hot")));
        assert_eq!(env[2].1, "thermal pressure serious");
    }

    #[test]
    fn muted_sinks_plan_nothing() {
        let mut courier = Courier::new(true);
        let plan = courier.plan(&fired(Sinks::default()), Instant::now());
        assert_eq!(plan, Delivery::default());
    }
}
