//! `siltop doctor`: what this Mac exposes, as plain text for bug reports.

use std::fmt::Write as _;
use std::io::Write;
use std::time::{Duration, Instant};

use siltop_core::{Error, Result, Snapshot};
use siltop_metrics::{Diagnostics, Sampler, diagnostics};

/// Ticks timed for the self-cost line.
const COST_TICKS: u32 = 8;
const COST_INTERVAL: Duration = Duration::from_millis(250);
/// SMC keys listed by name; the rest are summarised by type.
const SMC_SAMPLE: usize = 24;

/// Sampler cost over a short run: CPU seconds per wall second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelfCost {
    pub cpu_share: f64,
    pub ticks: u32,
    pub interval: Duration,
    pub mean_tick: Duration,
}

fn opt(value: Option<&str>) -> &str {
    value.unwrap_or("unknown")
}

/// The whole report. Pure, so it is testable without hardware.
#[must_use]
pub fn render(diag: &Diagnostics, snap: Option<&Snapshot>, cost: Option<SelfCost>) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "{}", crate::cli::version_line());
    let _ = writeln!(text, "\n[machine]");
    let _ = writeln!(text, "model        {}", opt(diag.model.as_deref()));
    let _ = writeln!(text, "chip         {}", opt(diag.brand.as_deref()));
    let _ = writeln!(
        text,
        "macos        {} ({})",
        opt(diag.os_version.as_deref()),
        opt(diag.os_build.as_deref())
    );
    let _ = writeln!(
        text,
        "chip table   {}",
        if diag.chip_table {
            "curated"
        } else {
            "none (prefix fallback)"
        }
    );
    if let Some(snap) = snap {
        let soc = &snap.soc;
        let _ = writeln!(
            text,
            "cores        {}E + {}P + {}S, {} GPU cores, {} GB",
            soc.e_cores,
            soc.p_cores,
            soc.s_cores,
            soc.gpu_cores,
            soc.memory_bytes / (1024 * 1024 * 1024)
        );
    }

    let _ = writeln!(text, "\n[smc]");
    match &diag.smc {
        None => {
            let _ = writeln!(text, "unavailable (AppleSMC did not open)");
        }
        Some(inv) => {
            let _ = writeln!(
                text,
                "keys         {} ({} temperature, {} fan, {} power)",
                inv.keys.len(),
                inv.with_prefix("T"),
                inv.with_prefix("F"),
                inv.with_prefix("P")
            );
            let types: Vec<String> = inv
                .type_histogram()
                .iter()
                .map(|(name, n)| format!("{name}:{n}"))
                .collect();
            let _ = writeln!(text, "types        {}", types.join(" "));
            let sample: Vec<String> = inv
                .keys
                .iter()
                .take(SMC_SAMPLE)
                .map(|k| format!("{}({})", k.name, k.data_type))
                .collect();
            let _ = writeln!(text, "sample       {}", sample.join(" "));
        }
    }

    let _ = writeln!(text, "\n[hid temperature services]");
    if diag.hid_products.is_empty() {
        let _ = writeln!(text, "none");
    }
    for (name, n) in &diag.hid_products {
        let _ = writeln!(text, "{n:>4}  {name}");
    }

    let _ = writeln!(text, "\n[processes]");
    let _ = writeln!(
        text,
        "readable     {} of {} ({} need root)",
        diag.procs.readable,
        diag.procs.total,
        diag.procs.denied()
    );

    if let Some(snap) = snap {
        let _ = writeln!(text, "\n[collectors]");
        for (id, health) in snap.health.iter() {
            let _ = writeln!(text, "{:<12} {health:?}", format!("{id:?}").to_lowercase());
        }
    }

    let _ = writeln!(text, "\n[self-cost]");
    match cost {
        None => {
            let _ = writeln!(text, "not measured");
        }
        Some(cost) => {
            let _ = writeln!(
                text,
                "cpu          {:.2}% over {} ticks at {} ms (mean tick {:.1} ms)",
                cost.cpu_share * 100.0,
                cost.ticks,
                cost.interval.as_millis(),
                cost.mean_tick.as_secs_f64() * 1000.0
            );
        }
    }
    text
}

fn process_cpu_secs() -> Option<f64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let rc = unsafe {
        // SAFETY: getrusage writes one rusage into `usage`.
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr())
    };
    if rc != 0 {
        return None;
    }
    let usage = unsafe {
        // SAFETY: getrusage returned 0, so the struct is initialised.
        usage.assume_init()
    };
    // `tv_usec` is i32 on macOS and i64 on Linux, so one cast serves both.
    #[allow(clippy::cast_lossless)]
    let secs = |tv: libc::timeval| tv.tv_sec as f64 + tv.tv_usec as f64 / 1_000_000.0;
    Some(secs(usage.ru_utime) + secs(usage.ru_stime))
}

/// Tick a real sampler for a couple of seconds and measure its cost.
fn measure(sampler: &mut Sampler) -> Result<(Snapshot, Option<SelfCost>)> {
    let mut last = sampler.tick()?;
    let cpu0 = process_cpu_secs();
    let start = Instant::now();
    let mut busy = Duration::ZERO;
    for _ in 0..COST_TICKS {
        std::thread::sleep(COST_INTERVAL);
        let t = Instant::now();
        last = sampler.tick()?;
        busy += t.elapsed();
    }
    let wall = start.elapsed().as_secs_f64().max(0.001);
    let cost = cpu0.zip(process_cpu_secs()).map(|(a, b)| SelfCost {
        cpu_share: (b - a).max(0.0) / wall,
        ticks: COST_TICKS,
        interval: COST_INTERVAL,
        mean_tick: busy / COST_TICKS,
    });
    Ok((last, cost))
}

pub fn run(out: &mut dyn Write) -> Result<()> {
    // Hardware probes first: SMC and HID clients must not overlap the sampler's.
    let diag = diagnostics();
    let (snap, cost) = match Sampler::new() {
        Ok(mut sampler) => match measure(&mut sampler) {
            Ok((snap, cost)) => (Some(snap), cost),
            Err(_) => (None, None),
        },
        Err(_) => (None, None),
    };
    let text = render(&diag, snap.as_ref(), cost);
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(Error::system(format!("write output: {err}"))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use siltop_core::{CollectorId, Health};
    use siltop_metrics::{ProcReadability, SmcInventory, SmcKey};

    fn diag() -> Diagnostics {
        Diagnostics {
            model: Some(String::from("Mac17,8")),
            brand: Some(String::from("Apple M5 Pro")),
            os_version: Some(String::from("27.0")),
            os_build: Some(String::from("27A5")),
            chip_table: true,
            smc: Some(SmcInventory {
                keys: (0..40)
                    .map(|i| SmcKey {
                        name: format!("T{i:03}"),
                        data_type: String::from(if i % 4 == 0 { "ui8" } else { "flt" }),
                        size: 4,
                    })
                    .collect(),
            }),
            hid_products: vec![(String::from("PMU tdie1"), 2)],
            procs: ProcReadability {
                total: 700,
                readable: 450,
            },
        }
    }

    #[test]
    fn render_lists_every_section() {
        let mut snap = Snapshot::empty();
        snap.soc.p_cores = 12;
        snap.soc.s_cores = 6;
        snap.soc.gpu_cores = 20;
        snap.soc.memory_bytes = 24 * 1024 * 1024 * 1024;
        snap.health.set(CollectorId::Cpu, Health::Ok);
        let cost = SelfCost {
            cpu_share: 0.0086,
            ticks: 8,
            interval: Duration::from_millis(250),
            mean_tick: Duration::from_micros(1500),
        };
        let text = render(&diag(), Some(&snap), Some(cost));
        for needle in [
            "model        Mac17,8",
            "chip         Apple M5 Pro",
            "macos        27.0 (27A5)",
            "chip table   curated",
            "cores        0E + 12P + 6S, 20 GPU cores, 24 GB",
            "keys         40 (40 temperature, 0 fan, 0 power)",
            "types        flt:30 ui8:10",
            "   2  PMU tdie1",
            "readable     450 of 700 (250 need root)",
            "cpu          Ok",
            "cpu          0.86% over 8 ticks at 250 ms (mean tick 1.5 ms)",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in\n{text}");
        }
        let sample = text.lines().find(|l| l.starts_with("sample")).unwrap();
        assert_eq!(sample.split_whitespace().count() - 1, SMC_SAMPLE);
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn render_absent_hardware_says_so() {
        let text = render(&Diagnostics::default(), None, None);
        assert!(text.contains("model        unknown"));
        assert!(text.contains("unavailable (AppleSMC did not open)"));
        assert!(text.contains("[hid temperature services]\nnone\n"));
        assert!(text.contains("not measured"));
        assert!(!text.contains("[collectors]"));
    }
}
