//! `siltop serve`: Prometheus text exposition over a tiny HTTP handler.
//! Loopback by default, `GET /metrics` only, no dependencies.
//!
//! Metric names follow Prometheus conventions (base units, `_ratio` for
//! 0..=1, `_bytes`, `_watts`, `_celsius`) and are listed in
//! `docs/research/18-record-and-export.md`. A reading this Mac lacks, or a
//! rate not measured yet, is left out: never a made-up zero.

use std::fmt::{Display, Write as _};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use siltop_core::Snapshot;

/// Processes exported per scrape, busiest first.
pub const TOP_PROCS: usize = 10;
/// A request line and headers never need more than this.
const MAX_REQUEST: usize = 8 * 1024;
/// The whole request must arrive within this, and each write within it.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// Connections answered at once; a scrape needs one.
const MAX_CLIENTS: usize = 8;
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

type Labels = Vec<(&'static str, String)>;

/// Accumulates metric families in exposition order.
#[derive(Default)]
struct Exposition {
    text: String,
}

impl Exposition {
    /// One gauge family; skipped entirely when it has no samples.
    fn gauge<V: Display>(
        &mut self,
        name: &str,
        help: &str,
        samples: impl IntoIterator<Item = (Labels, V)>,
    ) {
        let mut body = String::new();
        for (labels, value) in samples {
            let _ = write!(body, "{name}");
            if !labels.is_empty() {
                let joined: Vec<String> = labels
                    .iter()
                    .map(|(k, v)| format!("{k}=\"{}\"", escape(v)))
                    .collect();
                let _ = write!(body, "{{{}}}", joined.join(","));
            }
            let _ = writeln!(body, " {value}");
        }
        if !body.is_empty() {
            let _ = writeln!(self.text, "# HELP {name} {help}");
            let _ = writeln!(self.text, "# TYPE {name} gauge");
            self.text.push_str(&body);
        }
    }

    /// A family with no labels and at most one value.
    fn single<V: Display>(&mut self, name: &str, help: &str, value: Option<V>) {
        self.gauge(name, help, value.map(|v| (Vec::new(), v)));
    }
}

/// What `serve` prints once it listens on `addr`: a URL that works from
/// this Mac and, when other machines can reach the port, one warning line
/// saying so, since the page lists process names and pids.
#[must_use]
pub fn announce(addr: SocketAddr) -> String {
    let ip = addr.ip();
    let local = match ip {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    };
    let url = SocketAddr::new(local, addr.port());
    let mut text = format!("siltop: serving http://{url}/metrics (Ctrl-C stops)\n");
    if !ip.is_loopback() {
        let _ = writeln!(
            text,
            "siltop: warning: {addr} is reachable from other machines, and the page lists this Mac's process names and pids"
        );
    }
    text
}

/// Label values escape backslash, quote and newline, per the format; any
/// other control character (a process name is untrusted) becomes U+FFFD.
fn escape(value: &str) -> String {
    siltop_core::into_printable(
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n"),
    )
}

fn labelled<V>(key: &'static str, pairs: Vec<(&str, Option<V>)>) -> Vec<(Labels, V)> {
    pairs
        .into_iter()
        .filter_map(|(label, value)| value.map(|v| (vec![(key, String::from(label))], v)))
        .collect()
}

/// The whole `/metrics` body for one snapshot.
#[must_use]
pub fn exposition(s: &Snapshot) -> String {
    let mut e = Exposition::default();
    for families in [load, heat_and_power, battery, processes] {
        families(&mut e, s);
    }
    e.text
}

/// CPU, GPU, memory, network and disk.
fn load(e: &mut Exposition, s: &Snapshot) {
    let rate = |v: u64| s.primed.then_some(v);
    e.single(
        "siltop_cpu_usage_ratio",
        "Machine CPU busy, frequency-weighted, 0..1.",
        s.primed.then_some(s.cpu.scaled),
    );
    e.gauge(
        "siltop_cpu_cluster_usage_ratio",
        "CPU cluster busy, frequency-weighted, 0..1.",
        labelled(
            "cluster",
            vec![
                ("e", s.cpu.e_cluster.filter(|_| s.primed).map(|c| c.scaled)),
                ("p", s.cpu.p_cluster.filter(|_| s.primed).map(|c| c.scaled)),
                ("s", s.cpu.s_cluster.filter(|_| s.primed).map(|c| c.scaled)),
            ],
        ),
    );
    e.single(
        "siltop_gpu_usage_ratio",
        "GPU busy, 0..1.",
        s.gpu.map(|g| g.scaled),
    );
    let has_memory = s.memory.total_bytes > 0;
    e.single(
        "siltop_memory_used_bytes",
        "Memory in use.",
        has_memory.then_some(s.memory.used_bytes),
    );
    e.single(
        "siltop_memory_total_bytes",
        "Installed memory.",
        has_memory.then_some(s.memory.total_bytes),
    );
    e.single(
        "siltop_swap_used_bytes",
        "Swap in use.",
        has_memory.then_some(s.memory.swap_used_bytes),
    );
    // The sampler measures the network in bits per second.
    e.single(
        "siltop_network_receive_bytes_per_second",
        "Bytes received per second, headline interface.",
        rate(s.network.rx_bps / 8),
    );
    e.single(
        "siltop_network_transmit_bytes_per_second",
        "Bytes sent per second, headline interface.",
        rate(s.network.tx_bps / 8),
    );
    e.single(
        "siltop_disk_read_bytes_per_second",
        "Bytes read per second, all disks.",
        rate(s.disk.read_bps),
    );
    e.single(
        "siltop_disk_write_bytes_per_second",
        "Bytes written per second, all disks.",
        rate(s.disk.write_bps),
    );
}

fn heat_and_power(e: &mut Exposition, s: &Snapshot) {
    let watts = |r: Option<siltop_core::Reading>| r.and_then(siltop_core::Reading::usable);
    e.gauge(
        "siltop_power_watts",
        "Power by rail; system is the whole Mac's input.",
        labelled(
            "rail",
            vec![
                ("system", watts(s.power.system)),
                ("cpu", watts(s.power.cpu)),
                ("gpu", watts(s.power.gpu)),
                ("ane", watts(s.power.ane)),
                ("dram", watts(s.power.dram)),
            ],
        ),
    );
    e.gauge(
        "siltop_temp_celsius",
        "Temperature by zone.",
        labelled(
            "zone",
            vec![
                ("cpu", s.cpu.temp_c.or_else(|| s.sensors.best_cpu_c())),
                ("gpu", s.gpu.and_then(|g| g.temp_c).or(s.sensors.gpu_c)),
                ("hotspot", s.sensors.hotspot_c),
                ("e", s.sensors.e_c),
                ("p", s.sensors.p_c),
                ("s", s.sensors.s_c),
            ],
        ),
    );
    e.gauge(
        "siltop_fan_rpm",
        "Fan speed; absent on fanless Macs.",
        s.fans
            .fans
            .iter()
            .map(|f| (vec![("fan", f.name.clone())], f.rpm)),
    );
    e.single(
        "siltop_thermal_pressure",
        "macOS thermal pressure: 0 nominal, 1 fair, 2 serious, 3 critical.",
        Some(s.thermal.level()),
    );
}

/// Laptops only: a desktop exports no battery family at all.
fn battery(e: &mut Exposition, s: &Snapshot) {
    let battery = Some(&s.battery).filter(|b| b.present);
    e.single(
        "siltop_battery_charge_ratio",
        "Battery charge, 0..1.",
        battery
            .and_then(|b| b.percent)
            .map(|p| f32::from(p) / 100.0),
    );
    e.single(
        "siltop_battery_watts",
        "Battery power; negative while discharging.",
        battery.and_then(|b| b.watts),
    );
    e.single(
        "siltop_battery_cycles",
        "Battery charge cycles.",
        battery.and_then(|b| b.cycle_count),
    );
    e.single(
        "siltop_battery_health_ratio",
        "Battery full-charge capacity against design, 0..1.",
        battery
            .and_then(|b| b.health_pct)
            .map(|p| f32::from(p) / 100.0),
    );
    e.single(
        "siltop_battery_external_power",
        "1 while on an adapter, 0 on battery.",
        battery.and_then(|b| b.external_connected).map(u8::from),
    );
}

/// The [`TOP_PROCS`] busiest readable processes, once rates are measured.
fn processes(e: &mut Exposition, s: &Snapshot) {
    let mut procs: Vec<_> = s
        .processes
        .iter()
        .filter_map(|p| p.cpu.filter(|_| s.primed).map(|cpu| (p, cpu)))
        .collect();
    procs.sort_by(|a, b| b.1.total_cmp(&a.1));
    e.gauge(
        "siltop_process_cpu_ratio",
        "CPU of the busiest processes; 1 is one core fully busy.",
        procs.into_iter().take(TOP_PROCS).map(|(p, cpu)| {
            (
                vec![("name", p.name.clone()), ("pid", p.pid.to_string())],
                cpu / 100.0,
            )
        }),
    );
}

/// A stream whose next read can be told how long it may block.
pub trait CapRead: Read {
    fn cap_read(&mut self, left: Duration) -> io::Result<()>;
}

impl CapRead for TcpStream {
    fn cap_read(&mut self, left: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(left))
    }
}

/// When the whole request must be in, on a clock tests can drive.
pub struct Deadline<'a> {
    pub until: Instant,
    pub now: &'a dyn Fn() -> Instant,
}

impl Deadline<'_> {
    /// Time left, or `TimedOut` once it is spent.
    fn left(&self) -> io::Result<Duration> {
        let left = self.until.saturating_duration_since((self.now)());
        if left.is_zero() {
            Err(io::ErrorKind::TimedOut.into())
        } else {
            Ok(left)
        }
    }
}

/// Reads one request and answers it. Only `GET /metrics` gets a body.
/// Every read shares `deadline`: a client that drips a byte at a time
/// loses its connection when it passes, not 8 KiB of timeouts later.
pub fn handle<S: CapRead + Write>(
    stream: &mut S,
    metrics: &dyn Fn() -> String,
    deadline: &Deadline<'_>,
) -> io::Result<()> {
    let mut request = Vec::new();
    let mut buf = [0_u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < MAX_REQUEST {
        stream.cap_read(deadline.left()?)?;
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&request);
    let mut words = text.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let path = target.split('?').next().unwrap_or("");
    let (status, body) = match (method, path) {
        ("GET", "/metrics") => ("200 OK", metrics()),
        ("GET", _) => ("404 Not Found", String::from("try /metrics\n")),
        _ => ("405 Method Not Allowed", String::from("GET only\n")),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {CONTENT_TYPE}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

/// The latest exposition, shared between the sampler and the listener.
#[derive(Debug, Clone, Default)]
pub struct Latest(Arc<Mutex<String>>);

impl Latest {
    pub fn set(&self, text: String) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = text;
    }

    pub fn get(&self) -> String {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Frees a client slot when its connection is done.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(busy: &Arc<AtomicUsize>) -> Option<Self> {
        let taken = busy
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_CLIENTS).then_some(n + 1)
            })
            .is_ok();
        taken.then(|| Self(Arc::clone(busy)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Answers each connection on its own short-lived thread, up to
/// [`MAX_CLIENTS`] at once, until the listener fails. A slow or bad client
/// only loses its own connection and never holds up a scrape; past the
/// cap a new connection is closed at once.
pub fn accept_loop(listener: &TcpListener, latest: &Latest) {
    let busy = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let Some(slot) = Slot::take(&busy) else {
            continue;
        };
        let latest = latest.clone();
        let _ = std::thread::Builder::new()
            .name(String::from("serve-client"))
            .spawn(move || {
                let _slot = slot;
                let _ = answer(&mut stream, &latest);
            });
    }
}

fn answer(stream: &mut TcpStream, latest: &Latest) -> io::Result<()> {
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let deadline = Deadline {
        until: Instant::now() + IO_TIMEOUT,
        now: &Instant::now,
    };
    handle(stream, &|| latest.get(), &deadline)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::{Cluster, ClusterKind, FanMetric, Process, Reading};

    fn fixture() -> Snapshot {
        let mut s = Snapshot::empty();
        s.primed = true;
        s.cpu.scaled = 0.25;
        s.cpu.p_cluster = Some(Cluster {
            kind: ClusterKind::Performance,
            scaled: 0.5,
            active: 0.6,
            freq_mhz: 3000,
        });
        s.cpu.temp_c = Some(61.5);
        s.memory.used_bytes = 8 << 30;
        s.memory.total_bytes = 24 << 30;
        s.network.rx_bps = 12_000; // bits: 1500 bytes a second
        s.power.system = Some(Reading::measured(21.5));
        s.power.cpu = Some(Reading::measured(4.25));
        s.fans.fans = vec![FanMetric {
            name: String::from("Left \"main\""),
            rpm: 1800,
            max_rpm: 6000,
        }];
        s.battery.present = true;
        s.battery.percent = Some(80);
        s.battery.external_connected = Some(true);
        s.processes = vec![
            Process {
                pid: 7,
                name: String::from("cargo"),
                cpu: Some(150.0),
                ..Process::default()
            },
            Process {
                pid: 9,
                name: String::from("idle"),
                cpu: Some(0.5),
                ..Process::default()
            },
            Process {
                pid: 11,
                name: String::from("secret"),
                cpu: None,
                restricted: true,
                ..Process::default()
            },
        ];
        s
    }

    const GOLDEN: &str = r#"# HELP siltop_cpu_usage_ratio Machine CPU busy, frequency-weighted, 0..1.
# TYPE siltop_cpu_usage_ratio gauge
siltop_cpu_usage_ratio 0.25
# HELP siltop_cpu_cluster_usage_ratio CPU cluster busy, frequency-weighted, 0..1.
# TYPE siltop_cpu_cluster_usage_ratio gauge
siltop_cpu_cluster_usage_ratio{cluster="p"} 0.5
# HELP siltop_memory_used_bytes Memory in use.
# TYPE siltop_memory_used_bytes gauge
siltop_memory_used_bytes 8589934592
# HELP siltop_memory_total_bytes Installed memory.
# TYPE siltop_memory_total_bytes gauge
siltop_memory_total_bytes 25769803776
# HELP siltop_swap_used_bytes Swap in use.
# TYPE siltop_swap_used_bytes gauge
siltop_swap_used_bytes 0
# HELP siltop_network_receive_bytes_per_second Bytes received per second, headline interface.
# TYPE siltop_network_receive_bytes_per_second gauge
siltop_network_receive_bytes_per_second 1500
# HELP siltop_network_transmit_bytes_per_second Bytes sent per second, headline interface.
# TYPE siltop_network_transmit_bytes_per_second gauge
siltop_network_transmit_bytes_per_second 0
# HELP siltop_disk_read_bytes_per_second Bytes read per second, all disks.
# TYPE siltop_disk_read_bytes_per_second gauge
siltop_disk_read_bytes_per_second 0
# HELP siltop_disk_write_bytes_per_second Bytes written per second, all disks.
# TYPE siltop_disk_write_bytes_per_second gauge
siltop_disk_write_bytes_per_second 0
# HELP siltop_power_watts Power by rail; system is the whole Mac's input.
# TYPE siltop_power_watts gauge
siltop_power_watts{rail="system"} 21.5
siltop_power_watts{rail="cpu"} 4.25
# HELP siltop_temp_celsius Temperature by zone.
# TYPE siltop_temp_celsius gauge
siltop_temp_celsius{zone="cpu"} 61.5
# HELP siltop_fan_rpm Fan speed; absent on fanless Macs.
# TYPE siltop_fan_rpm gauge
siltop_fan_rpm{fan="Left \"main\""} 1800
# HELP siltop_thermal_pressure macOS thermal pressure: 0 nominal, 1 fair, 2 serious, 3 critical.
# TYPE siltop_thermal_pressure gauge
siltop_thermal_pressure 0
# HELP siltop_battery_charge_ratio Battery charge, 0..1.
# TYPE siltop_battery_charge_ratio gauge
siltop_battery_charge_ratio 0.8
# HELP siltop_battery_external_power 1 while on an adapter, 0 on battery.
# TYPE siltop_battery_external_power gauge
siltop_battery_external_power 1
# HELP siltop_process_cpu_ratio CPU of the busiest processes; 1 is one core fully busy.
# TYPE siltop_process_cpu_ratio gauge
siltop_process_cpu_ratio{name="cargo",pid="7"} 1.5
siltop_process_cpu_ratio{name="idle",pid="9"} 0.005
"#;

    #[test]
    fn exposition_matches_the_golden_text() {
        assert_eq!(exposition(&fixture()), GOLDEN);
    }

    /// Every sample line is `name{labels} value` with a parseable value,
    /// and every family is announced once before its samples.
    #[test]
    fn exposition_is_well_formed_for_the_demo_session() {
        let mut demo = crate::demo::Demo::new(Duration::from_millis(500));
        for _ in 0..8 {
            let snap = crate::worker::Tick::tick(&mut demo).unwrap();
            let text = exposition(&snap);
            let mut typed = std::collections::HashSet::new();
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("# TYPE ") {
                    assert!(typed.insert(rest.split(' ').next().unwrap().to_owned()));
                    continue;
                }
                if line.starts_with("# HELP ") {
                    continue;
                }
                let (series, value) = line.rsplit_once(' ').unwrap();
                assert!(value.parse::<f64>().is_ok(), "{line}");
                let name = series.split('{').next().unwrap();
                assert!(typed.contains(name), "{name} before its TYPE");
                assert!(name.starts_with("siltop_"));
            }
        }
    }

    /// Regression: a held rail past its stale age kept being scraped as a
    /// flat, made-up power line for the whole counter outage.
    #[test]
    fn stale_held_power_is_not_exported() {
        let mut s = fixture();
        s.power.system = Some(Reading {
            watts: 21.5,
            source: siltop_core::ReadingSource::Held {
                age: siltop_core::POWER_STALE_AFTER * 2,
            },
        });
        let text = exposition(&s);
        assert!(!text.contains("rail=\"system\""), "{text}");
        assert!(text.contains("siltop_power_watts{rail=\"cpu\"} 4.25"));
    }

    #[test]
    fn unprimed_rates_and_absent_hardware_are_left_out() {
        let mut s = fixture();
        s.primed = false;
        s.battery.present = false;
        s.fans.fans.clear();
        let text = exposition(&s);
        for gone in [
            "siltop_cpu_usage_ratio",
            "siltop_network_receive",
            "siltop_process_cpu_ratio",
            "siltop_battery_",
            "siltop_fan_rpm",
            "siltop_gpu_usage_ratio",
        ] {
            assert!(!text.contains(gone), "{gone} in\n{text}");
        }
        assert!(text.contains("siltop_memory_used_bytes 8589934592"));
    }

    #[test]
    fn process_list_is_capped_to_the_top_ten() {
        let mut s = fixture();
        s.processes = (0..30_u32)
            .map(|pid| Process {
                pid,
                name: format!("p{pid}"),
                cpu: Some(pid as f32),
                ..Process::default()
            })
            .collect();
        let text = exposition(&s);
        let rows = text
            .lines()
            .filter(|l| l.starts_with("siltop_process_cpu_ratio{"))
            .count();
        assert_eq!(rows, TOP_PROCS);
        assert!(text.contains("{name=\"p29\",pid=\"29\"} 0.29"));
    }

    #[test]
    fn label_escaping_covers_backslash_quote_and_newline() {
        assert_eq!(escape("a\\b\"c\nd"), "a\\\\b\\\"c\\nd");
    }

    #[test]
    fn announce_warns_only_when_other_machines_can_connect() {
        // Regression: `--addr 0.0.0.0:19179` printed only
        // `serving http://0.0.0.0:19179/metrics`: an unusable URL, and no
        // word that the process list was now open to the network.
        let quiet = announce("127.0.0.1:9101".parse().unwrap());
        assert_eq!(
            quiet,
            "siltop: serving http://127.0.0.1:9101/metrics (Ctrl-C stops)\n"
        );
        assert_eq!(announce("[::1]:9101".parse().unwrap()).lines().count(), 1);
        for (addr, url) in [
            ("0.0.0.0:19179", "http://127.0.0.1:19179/metrics"),
            ("[::]:19179", "http://[::1]:19179/metrics"),
            ("192.168.1.20:19179", "http://192.168.1.20:19179/metrics"),
        ] {
            let text = announce(addr.parse().unwrap());
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 2, "{text}");
            assert!(lines[0].contains(url), "{text}");
            assert!(lines[1].starts_with("siltop: warning: "), "{text}");
            assert!(lines[1].contains("process names"), "{text}");
        }
    }

    #[test]
    fn label_values_carry_no_escape_sequences() {
        // Regression: a process named `x<ESC>]2;RVBC<BEL>y` went into the
        // `name` label byte for byte.
        assert_eq!(escape("x\u{1b}]2;RVBC\u{7}y"), "x\u{FFFD}]2;RVBC\u{FFFD}y");
        assert_eq!(escape("tab\there\r"), "tab\u{FFFD}here\u{FFFD}");
    }

    fn roundtrip(request: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let latest = Latest::default();
        latest.set(String::from("siltop_up 1\n"));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            answer(&mut stream, &latest).unwrap();
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        response
    }

    #[test]
    fn http_get_metrics_over_loopback() {
        let response = roundtrip("GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        assert!(response.contains(&format!("Content-Type: {CONTENT_TYPE}\r\n")));
        assert!(response.contains("Content-Length: 12\r\n"));
        assert!(response.ends_with("\r\n\r\nsiltop_up 1\n"));
    }

    #[test]
    fn http_other_paths_and_methods_get_no_metrics() {
        let table = [
            ("GET / HTTP/1.1\r\n\r\n", "404 Not Found"),
            ("GET /metricsx HTTP/1.1\r\n\r\n", "404 Not Found"),
            ("POST /metrics HTTP/1.1\r\n\r\n", "405 Method Not Allowed"),
            ("\r\n\r\n", "405 Method Not Allowed"),
        ];
        for (request, status) in table {
            let response = roundtrip(request);
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}\r\n")),
                "{request:?}"
            );
            assert!(!response.contains("siltop_up"));
        }
        assert!(roundtrip("GET /metrics?x=1 HTTP/1.0\r\n\r\n").contains("200 OK"));
    }

    #[test]
    fn oversized_request_is_answered_not_hung() {
        let mut cursor = io::Cursor::new(vec![b'a'; MAX_REQUEST * 2]);
        let mut duplex = Duplex {
            input: &mut cursor,
            output: Vec::new(),
        };
        let t0 = Instant::now();
        let deadline = Deadline {
            until: t0 + IO_TIMEOUT,
            now: &|| t0,
        };
        handle(&mut duplex, &|| String::new(), &deadline).unwrap();
        assert!(String::from_utf8(duplex.output).unwrap().contains("405"));
    }

    /// One byte per read, each a second apart on a fake clock: once took
    /// 8 KiB of per-read timeouts (hours) while every scrape queued behind.
    #[test]
    fn slow_drip_client_is_cut_off_at_the_request_deadline() {
        struct Drip<'a> {
            clock: &'a std::cell::Cell<Instant>,
            reads: usize,
            output: Vec<u8>,
        }
        impl Read for Drip<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                self.clock.set(self.clock.get() + Duration::from_secs(1));
                buf[0] = b'G';
                Ok(1)
            }
        }
        impl CapRead for Drip<'_> {
            fn cap_read(&mut self, left: Duration) -> io::Result<()> {
                assert!(left <= IO_TIMEOUT, "no read may outlast the deadline");
                Ok(())
            }
        }
        impl Write for Drip<'_> {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.output.write(buf)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let t0 = Instant::now();
        let clock = std::cell::Cell::new(t0);
        let mut drip = Drip {
            clock: &clock,
            reads: 0,
            output: Vec::new(),
        };
        let now = || clock.get();
        let deadline = Deadline {
            until: t0 + IO_TIMEOUT,
            now: &now,
        };
        let error = handle(&mut drip, &|| String::from("x"), &deadline).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(drip.reads, 2, "two seconds, two bytes");
        assert!(drip.output.is_empty(), "no answer to a partial request");
    }

    /// A client that connects and says nothing no longer holds up a scrape.
    #[test]
    fn a_silent_client_does_not_block_the_next_scrape() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let latest = Latest::default();
        latest.set(String::from("siltop_up 1\n"));
        std::thread::spawn(move || accept_loop(&listener, &latest));
        let mut silent = TcpStream::connect(addr).unwrap();
        let mut scrape = TcpStream::connect(addr).unwrap();
        scrape.write_all(b"GET /metrics HTTP/1.1\r\n\r\n").unwrap();
        let mut response = String::new();
        scrape.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        // Sequential answering would have dropped the silent client after
        // its timeout, before the scrape; it is still being served.
        silent.write_all(b"GET /metrics HTTP/1.1\r\n\r\n").unwrap();
        let mut late = String::new();
        silent.read_to_string(&mut late).unwrap();
        assert!(late.ends_with("siltop_up 1\n"), "{late}");
    }

    #[test]
    fn network_rates_export_bytes_not_the_sampled_bits() {
        let mut s = fixture();
        s.network.rx_bps = 80_000_000; // a 10 MB/s download
        s.network.tx_bps = 8;
        let text = exposition(&s);
        assert!(text.contains("\nsiltop_network_receive_bytes_per_second 10000000\n"));
        assert!(text.contains("\nsiltop_network_transmit_bytes_per_second 1\n"));
    }

    struct Duplex<'a> {
        input: &'a mut io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Duplex<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl CapRead for Duplex<'_> {
        fn cap_read(&mut self, _: Duration) -> io::Result<()> {
            Ok(())
        }
    }

    impl Write for Duplex<'_> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
