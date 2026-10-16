//! Stamps the commit and build date into the binary for `--version
//! --verbose` and the settings About page. Both are best effort: a build
//! outside git, or without a clock, simply leaves them out.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SILTOP_GIT_SHA");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    // Release CI sets the full sha itself; `option_env!` reads it directly.
    if std::env::var_os("SILTOP_GIT_SHA").is_none()
        && let Some(sha) = git_sha()
    {
        println!("cargo:rustc-env=SILTOP_GIT_SHA={sha}");
    }
    // Reproducible builds pin the date with SOURCE_DATE_EPOCH.
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_secs()).ok())
        });
    if let Some(secs) = secs {
        let (y, m, d) = civil(secs.div_euclid(86_400));
        println!("cargo:rustc-env=SILTOP_BUILD_DATE={y:04}-{m:02}-{d:02}");
    }
}

fn git_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit())).then_some(sha)
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day)
/// (Howard Hinnant's `civil_from_days`).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}
