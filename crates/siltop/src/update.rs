//! Update check and install.
//!
//! The check asks the GitHub API for the repository's latest release
//! (`GET https://api.github.com/repos/CLoaKY233/siltop/releases/latest`)
//! through `/usr/bin/curl`, so this binary links no HTTP stack and opens no
//! socket of its own. macOS maintains the TLS. The only thing sent is the
//! User-Agent `siltop/<version>`. A test build may point
//! `SILTOP_UPDATE_URL` at a local fixture server.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::cli::{self, EXIT_ERROR};
use crate::onboarding::Launch;

/// The release JSON is a few kilobytes: a background check that takes
/// longer is offline, and it must never hold anything up.
const CHECK_TIMEOUT: &str = "3";
/// `siltop update` is waited on, so a slow link gets longer.
const MANUAL_TIMEOUT: &str = "20";
/// Seconds to open a connection, for both the check and the download.
const CONNECT_TIMEOUT: &str = "10";
/// A download slower than this many bytes a second...
const STALL_BYTES: &str = "1024";
/// ...for this many seconds has stalled. A slow link still finishes.
const STALL_SECS: &str = "30";
/// At most one automatic check per day.
const THROTTLE: Duration = Duration::from_hours(24);

/// Where a background check looks, unless a test build overrides it.
#[must_use]
pub fn endpoint() -> String {
    #[cfg(test)]
    if let Ok(url) = std::env::var("SILTOP_UPDATE_URL")
        && !url.is_empty()
    {
        return url;
    }
    String::from("https://api.github.com/repos/CLoaKY233/siltop/releases/latest")
}

/// Where siltop's source and releases live.
const REPO_URL: &str = "https://github.com/CLoaKY233/siltop";

/// What the GitHub API is asked to answer in.
const GITHUB_JSON: &str = "application/vnd.github+json";

/// The tail of the archive a release carries for this build:
/// `<bin>-<tag>-macos-arm64.tar.gz`, as `scripts/package.sh` names it.
const ARCHIVE_SUFFIX: &str = "-macos-arm64.tar.gz";

/// `siltop/<version>`. Nothing else is sent.
#[must_use]
pub fn user_agent() -> String {
    format!("siltop/{}", cli::VERSION)
}

/// A release this build can install, read from the GitHub release JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The tag without its leading `v`.
    pub version: String,
    pub released: String,
    /// The archive's download URL.
    pub url: String,
    pub sha256: String,
    /// The release page.
    pub notes: String,
}

#[derive(Debug)]
pub enum ManifestError {
    Json(String),
    Missing(&'static str),
    BadSha,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(why) => write!(f, "the release information is not JSON: {why}"),
            Self::Missing(field) => write!(f, "the latest release has no {field}"),
            Self::BadSha => write!(f, "the release archive's sha256 is not 64 hex digits"),
        }
    }
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: Option<String>,
    published_at: Option<String>,
    html_url: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    /// `sha256:<hex>` on releases GitHub has digested; absent on older ones.
    digest: Option<String>,
}

/// A parsed release, before its checksum is known for sure: the archive's
/// `digest` when GitHub gave one, else the `.sha256` asset to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub released: String,
    pub url: String,
    pub notes: String,
    pub sha256: Option<String>,
    pub sha256_url: Option<String>,
}

impl Release {
    fn into_manifest(self, sha256: String) -> Manifest {
        Manifest {
            version: self.version,
            released: self.released,
            url: self.url,
            sha256,
            notes: self.notes,
        }
    }
}

/// The archive for this build among `assets`: exactly
/// `<bin>-<tag>-macos-arm64.tar.gz`, else the one archive with that tail.
fn archive<'a>(assets: &'a [RawAsset], tag: &str) -> Option<&'a RawAsset> {
    let exact = format!("{}-{tag}{ARCHIVE_SUFFIX}", env!("CARGO_BIN_NAME"));
    assets.iter().find(|asset| asset.name == exact).or_else(|| {
        let mut tails = assets.iter().filter(|a| a.name.ends_with(ARCHIVE_SUFFIX));
        tails.next().filter(|_| tails.next().is_none())
    })
}

/// Parse the body of `GET /repos/<owner>/<repo>/releases/latest`. A
/// release without a tag or without this build's archive is an error; a
/// `digest` that is not `sha256:` and 64 hex digits is a bad checksum.
pub fn parse_release(bytes: &str) -> Result<Release, ManifestError> {
    let raw: RawRelease =
        serde_json::from_str(bytes).map_err(|err| ManifestError::Json(err.to_string()))?;
    let tag = raw
        .tag_name
        .filter(|text| !text.trim().is_empty())
        .ok_or(ManifestError::Missing("tag"))?;
    let tag = tag.trim();
    let asset = archive(&raw.assets, tag).ok_or(ManifestError::Missing("macOS archive"))?;
    let sha256 = match asset.digest.as_deref() {
        Some(digest) => {
            let hex = digest
                .strip_prefix("sha256:")
                .filter(|hex| is_sha256(hex))
                .ok_or(ManifestError::BadSha)?;
            Some(hex.to_ascii_lowercase())
        }
        None => None,
    };
    let checksum_name = format!("{}.sha256", asset.name);
    let sha256_url = raw
        .assets
        .iter()
        .find(|a| a.name == checksum_name)
        .map(|a| a.browser_download_url.clone());
    Ok(Release {
        version: tag.strip_prefix('v').unwrap_or(tag).to_owned(),
        released: raw.published_at.unwrap_or_default(),
        url: asset.browser_download_url.clone(),
        notes: raw.html_url.unwrap_or_default(),
        sha256,
        sha256_url,
    })
}

/// The checksum in a `shasum -a 256` line: `<hex>  <file name>`.
fn parse_sha256_file(text: &str) -> Result<String, ManifestError> {
    text.split_whitespace()
        .next()
        .filter(|word| is_sha256(word))
        .map(str::to_ascii_lowercase)
        .ok_or(ManifestError::BadSha)
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Semver, including prereleases: `1.1.0-rc.1` < `1.1.0-rc.2` < `1.1.0`.
#[must_use]
pub fn cmp_version(left: &str, right: &str) -> std::cmp::Ordering {
    let (left, left_pre) = split_pre(left);
    let (right, right_pre) = split_pre(right);
    let ord = cmp_core(&left, &right);
    if ord != std::cmp::Ordering::Equal {
        return ord;
    }
    match (left_pre, right_pre) {
        (None, None) => std::cmp::Ordering::Equal,
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => cmp_pre(a, b),
    }
}

fn split_pre(version: &str) -> (Vec<u64>, Option<&str>) {
    let version = version.trim().trim_start_matches('v');
    let (core, pre) = version.split_once('-').unwrap_or((version, ""));
    let numbers = core
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect();
    let pre = if pre.is_empty() { None } else { Some(pre) };
    (numbers, pre)
}

fn cmp_core(left: &[u64], right: &[u64]) -> std::cmp::Ordering {
    let len = left.len().max(right.len());
    for i in 0..len {
        let l = left.get(i).copied().unwrap_or(0);
        let r = right.get(i).copied().unwrap_or(0);
        if l != r {
            return l.cmp(&r);
        }
    }
    std::cmp::Ordering::Equal
}

fn cmp_pre(left: &str, right: &str) -> std::cmp::Ordering {
    let left: Vec<&str> = left.split('.').collect();
    let right: Vec<&str> = right.split('.').collect();
    for (l, r) in left.iter().zip(right.iter()) {
        let ord = match (l.parse::<u64>(), r.parse::<u64>()) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            (Err(_), Err(_)) => l.cmp(r),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    left.len().cmp(&right.len())
}

#[must_use]
pub fn is_newer(current: &str, remote: &str) -> bool {
    cmp_version(remote, current) == std::cmp::Ordering::Greater
}

/// A strict `MAJOR.MINOR.PATCH[-PRE][+BUILD]`. Build metadata is ignored,
/// as semver says. Anything else is `None`.
fn parse_exact(text: &str) -> Option<([u64; 3], Option<&str>)> {
    let text = text.trim().strip_prefix('v').unwrap_or(text.trim());
    let text = text.split_once('+').map_or(text, |(version, _)| version);
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (text, None),
    };
    let mut parts = core.split('.');
    let mut numbers = [0_u64; 3];
    for number in &mut numbers {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *number = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    if let Some(pre) = pre
        && pre
            .split('.')
            .any(|id| id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    {
        return None;
    }
    Some((numbers, pre))
}

/// `true` only when both parse and name the same release, prerelease
/// included: `1.1.0` is not `1.1.0-rc.1`, and `1.1.1` is not `1.1.10`.
#[must_use]
pub fn same_version(left: &str, right: &str) -> bool {
    match (parse_exact(left), parse_exact(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// The version in a `siltop --version` line such as `siltop 1.2.0 (abc1234)`.
#[must_use]
pub fn reported_version(line: &str) -> Option<&str> {
    line.lines()
        .next()?
        .trim()
        .strip_prefix("siltop ")?
        .split_whitespace()
        .next()
}

/// How this binary was installed. Only a release download replaces itself:
/// a keg belongs to Homebrew and a cargo build to cargo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    Brew,
    Cargo,
    /// A release archive unpacked into a `bin` directory, such as
    /// `~/.local/bin`, `~/bin` or `/usr/local/bin`.
    Download,
    Other,
}

impl InstallKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Brew => "brew",
            Self::Cargo => "cargo",
            Self::Download => "download",
            Self::Other => "other",
        }
    }
}

/// System directories that are never ours to write, even as root.
const SYSTEM_BINS: [&str; 4] = ["/bin", "/sbin", "/usr/bin", "/usr/sbin"];

/// `Cellar` in the canonical path is a Homebrew keg, and `.cargo/bin` (or
/// `$CARGO_HOME/bin`) is `cargo install`'s. Any other `bin` directory holds
/// a release download, which can replace itself. A binary anywhere else,
/// such as a build in `target/release`, is left alone.
#[must_use]
pub fn install_kind(exe: &Path) -> InstallKind {
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    classify(exe, cargo_home.as_deref())
}

/// `cargo_home` is `$CARGO_HOME` when set. Tests pass it directly so they
/// do not have to change the process environment.
#[must_use]
pub fn classify(exe: &Path, cargo_home: Option<&Path>) -> InstallKind {
    let canon = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    if canon.to_string_lossy().contains("/Cellar/") {
        return InstallKind::Brew;
    }
    let Some(dir) = canon.parent() else {
        return InstallKind::Other;
    };
    let cargo_bin = cargo_home
        .map(|home| fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf()))
        .is_some_and(|home| dir == home.join("bin"));
    if cargo_bin || dir.ends_with(".cargo/bin") {
        return InstallKind::Cargo;
    }
    let in_bin = dir.file_name().is_some_and(|name| name == "bin");
    if in_bin && !SYSTEM_BINS.iter().any(|system| dir == Path::new(system)) {
        InstallKind::Download
    } else {
        InstallKind::Other
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// Unix seconds of the last automatic check. `None` means never.
    pub last_check: Option<u64>,
    /// A version the user asked to skip.
    pub skipped: String,
    /// The newer version the last check found, so a launch inside the
    /// throttle window still offers it.
    pub available: String,
}

impl State {
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        let mut state = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "last_check" => state.last_check = value.parse().ok(),
                "skipped" => value.clone_into(&mut state.skipped),
                "available" => value.clone_into(&mut state.available),
                _ => {}
            }
        }
        state
    }

    /// Written as TOML. Versions are plain `1.2.3` strings, so they need no escaping.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let last_check = self
            .last_check
            .map(|then| format!("last_check = {then}\n"))
            .unwrap_or_default();
        let text = format!(
            "{last_check}skipped = \"{}\"\navailable = \"{}\"\n",
            self.skipped, self.available
        );
        crate::config_io::atomic_write(path, &text)
    }
}

/// `$XDG_STATE_HOME/siltop/updates.toml`, else `~/.local/state/...`.
#[must_use]
pub fn state_path() -> Option<PathBuf> {
    crate::store_disk::default_root()
        .and_then(|history| history.parent().map(Path::to_path_buf))
        .map(|dir| dir.join("updates.toml"))
}

#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// An automatic check runs only when the user opted in, the launch is
/// interactive, nothing forced it off, and the last check is at least a
/// day old. The manual command does not call this.
#[must_use]
pub fn should_check(
    opted_in: bool,
    state: &State,
    now: u64,
    launch: Launch,
    forced_off: bool,
) -> bool {
    if !opted_in || forced_off || launch.non_interactive() {
        return false;
    }
    // A known, unskipped update is fetched again so it can still be offered.
    if !state.available.is_empty()
        && state.available != state.skipped
        && is_newer(cli::VERSION, &state.available)
    {
        return true;
    }
    match state.last_check {
        Some(then) => now.saturating_sub(then) >= THROTTLE.as_secs(),
        None => true,
    }
}

/// `SILTOP_NO_UPDATE_CHECK=1` (any value but empty, `0` or `false`).
#[must_use]
pub fn forced_off() -> bool {
    std::env::var("SILTOP_NO_UPDATE_CHECK")
        .is_ok_and(|value| !matches!(value.trim(), "" | "0" | "false"))
}

/// A newer release the user has not skipped.
#[must_use]
pub fn offer(current: &str, manifest: &Manifest, skipped: &str) -> bool {
    is_newer(current, &manifest.version) && manifest.version != skipped
}

#[derive(Debug)]
pub enum FetchError {
    Offline(String),
    Manifest(ManifestError),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Offline(why) => write!(f, "{why}"),
            Self::Manifest(err) => write!(f, "{err}"),
        }
    }
}

/// Ask for the latest release and its archive's checksum: GitHub's
/// `digest` when the release has one, else its `.sha256` asset. A network
/// failure is [`FetchError::Offline`]: the caller stays quiet.
pub fn fetch(url: &str) -> Result<Manifest, FetchError> {
    fetch_with(url, Limit::Check)
}

/// [`fetch`] with the background check's short cap, or the manual
/// command's longer one.
fn fetch_with(url: &str, limit: Limit) -> Result<Manifest, FetchError> {
    let offline = |err: io::Error| FetchError::Offline(err.to_string());
    let not_utf8 = || FetchError::Manifest(ManifestError::Json(String::from("not utf-8")));
    let body = curl(url, None, limit, Some(GITHUB_JSON)).map_err(offline)?;
    let text = String::from_utf8(body).map_err(|_| not_utf8())?;
    let release = parse_release(&text).map_err(FetchError::Manifest)?;
    if let Some(sha256) = release.sha256.clone() {
        return Ok(release.into_manifest(sha256));
    }
    let sha_url = release
        .sha256_url
        .clone()
        .ok_or(FetchError::Manifest(ManifestError::Missing("sha256")))?;
    let body = curl(&sha_url, None, limit, None).map_err(offline)?;
    let text = String::from_utf8(body).map_err(|_| not_utf8())?;
    let sha256 = parse_sha256_file(&text).map_err(FetchError::Manifest)?;
    Ok(release.into_manifest(sha256))
}

/// How long curl may take: the check is capped outright, a download only
/// has to keep moving.
#[derive(Debug, Clone, Copy)]
enum Limit {
    Check,
    Manual,
    Download,
}

/// `accept` is sent as the `Accept` header when given (the API call only).
fn curl(url: &str, out: Option<&Path>, limit: Limit, accept: Option<&str>) -> io::Result<Vec<u8>> {
    let mut cmd = Command::new("/usr/bin/curl");
    // `-q` must come first: it stops curl reading `~/.curlrc`, which could
    // add a proxy, `--insecure` or an output file behind our back.
    cmd.args(["-q", "-fsS", "--connect-timeout", CONNECT_TIMEOUT]);
    match limit {
        Limit::Check => cmd.args(["--max-time", CHECK_TIMEOUT]),
        Limit::Manual => cmd.args(["--max-time", MANUAL_TIMEOUT]),
        Limit::Download => cmd.args(["--speed-limit", STALL_BYTES, "--speed-time", STALL_SECS]),
    };
    cmd.args(["-A", &user_agent()]);
    if let Some(accept) = accept {
        cmd.args(["-H", &format!("Accept: {accept}")]);
    }
    // Production is HTTPS only. A test fixture may be plain HTTP on loopback.
    let https = url.starts_with("https://");
    if https || !cfg!(test) {
        cmd.args(["--proto", "=https"]);
    }
    // Follow a few redirects, HTTPS only: a download URL may move to a release host.
    // Without -L curl saves the redirect notice and the sha256 check fails.
    cmd.args(["-L", "--max-redirs", "5", "--proto-redir", "=https"]);
    if let Some(path) = out {
        cmd.arg("-o");
        cmd.arg(path);
    }
    cmd.arg(url);
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(io::Error::other(curl_reason(&String::from_utf8_lossy(
            &output.stderr,
        ))));
    }
    if out.is_some() {
        return Ok(Vec::new());
    }
    Ok(output.stdout)
}

/// curl's error as one line. GitHub answers 403 when an address has used up
/// its 60 unauthenticated API calls for the hour.
fn curl_reason(stderr: &str) -> String {
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("curl failed");
    if first.contains("error: 403") || first.contains("error: 429") {
        String::from("GitHub is rate-limiting this address; try again in an hour")
    } else {
        first.trim_start_matches("curl: ").to_owned()
    }
}

/// Why an install or a rollback stopped. The old binary is still the one on disk.
#[derive(Debug)]
pub enum InstallError {
    Brew,
    /// The version to ask cargo for.
    Cargo(String),
    /// The release page to download from instead.
    Other(String),
    /// The directory this user cannot write, with the archive URL to
    /// download by hand (empty when unknown).
    NotWritable(String),
    /// The running binary's path could not be resolved.
    Exe(String),
    Download(String),
    Sha,
    Archive(String),
    /// The new binary would not run `--version`.
    Exec(String),
    /// The new binary ran but reported another version.
    Version {
        want: String,
        got: String,
    },
    Swap(String),
    /// Rollback on a copy that `siltop update` did not install.
    RollbackBrew,
    RollbackOther,
    /// Rollback with no `<name>.old` beside the binary.
    NoOld(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Brew => write!(f, "this copy is a Homebrew keg; run `brew upgrade siltop`"),
            Self::Cargo(version) => write!(
                f,
                "this copy was installed by cargo, so cargo should replace it: `cargo install --git {REPO_URL} --tag v{version} siltop --locked`"
            ),
            Self::Other(url) => write!(
                f,
                "this copy is not in a bin directory (a local build?), so it is not replaced; download the update from {url}"
            ),
            Self::NotWritable(url) if url.is_empty() => {
                write!(f, "you cannot write to the directory this copy is in")
            }
            Self::NotWritable(url) => write!(
                f,
                "you cannot write to the directory this copy is in; run `sudo siltop update --yes`, or download the update from {url}"
            ),
            Self::Exe(why) => write!(f, "could not find the running binary: {why}"),
            Self::Download(why) => write!(f, "the download failed: {why}"),
            Self::Sha => write!(f, "the download's sha256 does not match"),
            Self::Archive(why) => write!(f, "the download could not be unpacked: {why}"),
            Self::Exec(why) => write!(
                f,
                "the new binary did not run ({why}), so it was not installed"
            ),
            Self::Version { want, got } => write!(
                f,
                "the new binary reported {got:?}, not {want}, so it was not installed"
            ),
            Self::Swap(why) => write!(f, "could not replace the binary: {why}"),
            Self::RollbackBrew => write!(
                f,
                "this copy is a Homebrew keg, so rollback does not apply; Homebrew manages its versions (`brew upgrade siltop`)"
            ),
            Self::RollbackOther => write!(
                f,
                "rollback only restores a copy that `siltop update` replaced; this one was installed another way"
            ),
            Self::NoOld(name) => write!(f, "there is no {name} to restore"),
        }
    }
}

/// The partial download and the unpacked archive. Dropping this removes
/// both, so every return from [`install_in`] (and a panic) cleans up.
struct Scratch {
    archive: PathBuf,
    staging: PathBuf,
}

impl Scratch {
    fn new(dir: &Path) -> Self {
        let scratch = Self {
            archive: dir.join(".siltop-download.tar.gz"),
            staging: dir.join(".siltop-new"),
        };
        // A run killed mid-download leaves these behind.
        scratch.clear();
        scratch
    }

    fn clear(&self) {
        let _ = fs::remove_file(&self.archive);
        let _ = fs::remove_dir_all(&self.staging);
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.clear();
    }
}

/// The file this process runs from, symlinks resolved, so an update or a
/// rollback acts on this copy under whatever name it has, never a sibling.
fn resolve(exe: &Path) -> Result<PathBuf, InstallError> {
    fs::canonicalize(exe).map_err(|err| InstallError::Exe(format!("{}: {err}", exe.display())))
}

/// `siltop` → `siltop.old`; a renamed `pt` → `pt.old`.
fn old_path(exe: &Path) -> Result<PathBuf, InstallError> {
    let name = exe
        .file_name()
        .ok_or_else(|| InstallError::Exe(exe.display().to_string()))?;
    let mut old = name.to_os_string();
    old.push(".old");
    Ok(exe.with_file_name(old))
}

/// Replace a release download with `manifest`, or explain why not.
/// Any failure leaves the current binary in place.
pub fn install(exe: &Path, manifest: &Manifest) -> Result<(), InstallError> {
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    install_in(exe, manifest, cargo_home.as_deref())
}

/// Why `kind` may not replace itself, or `None` when it may.
fn refusal(kind: InstallKind, manifest: &Manifest) -> Option<InstallError> {
    match kind {
        InstallKind::Brew => Some(InstallError::Brew),
        InstallKind::Cargo => Some(InstallError::Cargo(manifest.version.clone())),
        InstallKind::Other => Some(InstallError::Other(manifest.url.clone())),
        InstallKind::Download => None,
    }
}

/// [`install`] with an explicit `$CARGO_HOME`, so a test does not race on the environment.
pub fn install_in(
    exe: &Path,
    manifest: &Manifest,
    cargo_home: Option<&Path>,
) -> Result<(), InstallError> {
    if let Some(why) = refusal(classify(exe, cargo_home), manifest) {
        return Err(why);
    }
    let dest = resolve(exe)?;
    let old = old_path(&dest)?;
    let dir = dest
        .parent()
        .ok_or(InstallError::NotWritable(String::new()))?;
    if !writable(dir) {
        return Err(InstallError::NotWritable(manifest.url.clone()));
    }
    let scratch = Scratch::new(dir);
    curl(&manifest.url, Some(&scratch.archive), Limit::Download, None)
        .map_err(|err| InstallError::Download(err.to_string()))?;
    let digest =
        file_sha256(&scratch.archive).map_err(|err| InstallError::Download(err.to_string()))?;
    if !digest.eq_ignore_ascii_case(&manifest.sha256) {
        return Err(InstallError::Sha);
    }
    fs::create_dir_all(&scratch.staging)
        .map_err(|_| InstallError::NotWritable(manifest.url.clone()))?;
    let unpacked = Command::new("/usr/bin/tar")
        .args(["-xzf"])
        .arg(&scratch.archive)
        .arg("-C")
        .arg(&scratch.staging)
        .output()
        .map_err(|err| InstallError::Archive(err.to_string()))?;
    if !unpacked.status.success() {
        let why = String::from_utf8_lossy(&unpacked.stderr).trim().to_owned();
        return Err(InstallError::Archive(why));
    }
    let new_bin = find_binary(&scratch.staging)
        .ok_or_else(|| InstallError::Archive(String::from("the archive has no siltop binary")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&new_bin, fs::Permissions::from_mode(0o755));
    }
    let reported = binary_version(&new_bin).map_err(|err| InstallError::Exec(err.to_string()))?;
    if !reported_version(&reported).is_some_and(|got| same_version(got, &manifest.version)) {
        return Err(InstallError::Version {
            want: manifest.version.clone(),
            got: reported,
        });
    }
    if old.exists() {
        let _ = fs::remove_file(&old);
    }
    if dest.exists() {
        fs::rename(&dest, &old).map_err(|err| InstallError::Swap(err.to_string()))?;
    }
    if let Err(err) = fs::rename(&new_bin, &dest) {
        if old.exists() {
            let _ = fs::rename(&old, &dest);
        }
        return Err(InstallError::Swap(err.to_string()));
    }
    Ok(())
}

/// Whether this user can create files in `dir`: the permission bits do not
/// say, since `/usr/local/bin` may belong to root or to the admin group.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(".siltop-write-probe");
    let created = fs::File::create(&probe).is_ok();
    let _ = fs::remove_file(&probe);
    created
}

/// The `siltop` binary in an unpacked release, at the top or one folder down.
fn find_binary(root: &Path) -> Option<PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    if let Ok(entries) = fs::read_dir(root) {
        dirs.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    dirs.iter()
        .map(|dir| dir.join("siltop"))
        .find(|path| path.is_file())
}

fn binary_version(path: &Path) -> io::Result<String> {
    let output = Command::new(path).arg("--version").output()?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(io::Error::other(if why.is_empty() {
            format!("`--version` exited with {}", output.status)
        } else {
            format!("`--version` failed: {why}")
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The system `shasum` by absolute path, never whatever `PATH` offers.
fn file_sha256(path: &Path) -> io::Result<String> {
    let output = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("sha256 failed"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let hex = text
        .split_whitespace()
        .next()
        .filter(|word| is_sha256(word))
        .ok_or_else(|| io::Error::other("sha256 returned nothing"))?;
    Ok(hex.to_ascii_lowercase())
}

/// Put `<name>.old` back over this copy. Release downloads only.
pub fn rollback(exe: &Path) -> Result<(), InstallError> {
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    rollback_in(exe, cargo_home.as_deref())
}

pub fn rollback_in(exe: &Path, cargo_home: Option<&Path>) -> Result<(), InstallError> {
    match classify(exe, cargo_home) {
        InstallKind::Brew => return Err(InstallError::RollbackBrew),
        InstallKind::Cargo | InstallKind::Other => return Err(InstallError::RollbackOther),
        InstallKind::Download => {}
    }
    let dest = resolve(exe)?;
    let old = old_path(&dest)?;
    if !old.is_file() {
        let name = old.file_name().map_or_else(
            || String::from("siltop.old"),
            |name| name.to_string_lossy().into_owned(),
        );
        return Err(InstallError::NoOld(name));
    }
    let dir = dest
        .parent()
        .ok_or(InstallError::NotWritable(String::new()))?;
    let aside = dir.join(".siltop-rollback");
    let _ = fs::remove_file(&aside);
    if dest.exists() {
        fs::rename(&dest, &aside).map_err(|err| InstallError::Swap(err.to_string()))?;
    }
    if let Err(err) = fs::rename(&old, &dest) {
        let _ = fs::rename(&aside, &dest);
        return Err(InstallError::Swap(err.to_string()));
    }
    let _ = fs::remove_file(&aside);
    Ok(())
}

/// `siltop update --check`: exit 0 up to date, 10 when one is available.
pub const EXIT_UPDATE: u8 = 10;

/// What bare `siltop update` prints after "`<version>` available".
#[must_use]
pub fn how_to_install(kind: InstallKind, version: &str, url: &str) -> String {
    match kind {
        InstallKind::Download => String::from("install it with `siltop update --yes`"),
        InstallKind::Brew => String::from("install it with `brew upgrade siltop`"),
        InstallKind::Cargo => format!(
            "install it with `cargo install --git {REPO_URL} --tag v{version} siltop --locked`"
        ),
        InstallKind::Other => format!("download it from {url}"),
    }
}

/// The manual command. It runs even when automatic checks are off.
/// Bare `update` checks and says how to install; `--yes` installs.
pub fn command(action: cli::UpdateAction, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            let _ = writeln!(err, "siltop: could not find the running binary: {error}");
            return EXIT_ERROR;
        }
    };
    if action == cli::UpdateAction::Rollback {
        return match rollback(&exe) {
            Ok(()) => {
                let name = resolve(&exe)
                    .ok()
                    .and_then(|path| old_path(&path).ok())
                    .and_then(|old| old.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| String::from("siltop.old"));
                let _ = writeln!(out, "siltop: restored {name}; restart to use it");
                0
            }
            Err(why) => {
                let _ = writeln!(err, "siltop: {why}");
                EXIT_ERROR
            }
        };
    }
    let manifest = match fetch_with(&endpoint(), Limit::Manual) {
        Ok(manifest) => manifest,
        Err(FetchError::Offline(why)) => {
            let _ = writeln!(err, "siltop: could not reach GitHub: {why}");
            return EXIT_ERROR;
        }
        Err(FetchError::Manifest(why)) => {
            let _ = writeln!(err, "siltop: {why}");
            return EXIT_ERROR;
        }
    };
    if !is_newer(cli::VERSION, &manifest.version) {
        let _ = writeln!(out, "up to date ({})", cli::VERSION);
        return 0;
    }
    match action {
        cli::UpdateAction::Check => {
            let _ = writeln!(out, "{} available", manifest.version);
            return EXIT_UPDATE;
        }
        cli::UpdateAction::Ask => {
            let _ = writeln!(
                out,
                "{} available (you have {}); {}",
                manifest.version,
                cli::VERSION,
                how_to_install(install_kind(&exe), &manifest.version, &manifest.url)
            );
            return 0;
        }
        cli::UpdateAction::Yes | cli::UpdateAction::Rollback => {}
    }
    if let Some(why) = refusal(install_kind(&exe), &manifest) {
        let _ = writeln!(err, "siltop: {why}");
        return EXIT_ERROR;
    }
    let _ = writeln!(err, "siltop: downloading {}", manifest.version);
    match install(&exe, &manifest) {
        Ok(()) => {
            let _ = writeln!(out, "Updated to {}, restart to use it", manifest.version);
            0
        }
        Err(error) => {
            let _ = writeln!(err, "siltop: {error}");
            EXIT_ERROR
        }
    }
}

/// What the update dialog's four buttons do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogChoice {
    Now,
    Always,
    Skip,
    Later,
}

impl DialogChoice {
    pub const ALL: [Self; 4] = [Self::Now, Self::Always, Self::Skip, Self::Later];
}

/// The result of one dialog choice. `install_now` is only "Update now".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogEffect {
    pub install_now: bool,
}

/// Apply a dialog choice. "Always" installs this version now and turns on
/// quiet installs for later ones. "Skip" hides this version. "Later"
/// changes nothing.
pub fn apply_dialog(
    choice: DialogChoice,
    version: &str,
    config: &mut siltop_core::Config,
    state: &mut State,
) -> DialogEffect {
    match choice {
        DialogChoice::Now => DialogEffect { install_now: true },
        DialogChoice::Always => {
            config.updates_auto_install = true;
            DialogEffect { install_now: true }
        }
        DialogChoice::Skip => {
            version.clone_into(&mut state.skipped);
            DialogEffect { install_now: false }
        }
        DialogChoice::Later => DialogEffect { install_now: false },
    }
}

/// What a finished check should do. Auto-install only replaces a release download,
/// and only when a newer version is actually available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckPlan {
    Install,
    Offer,
    Quiet,
}

#[must_use]
pub fn plan_check(auto_install: bool, kind: InstallKind, newer: bool) -> CheckPlan {
    if !newer {
        CheckPlan::Quiet
    } else if auto_install && kind == InstallKind::Download {
        CheckPlan::Install
    } else {
        CheckPlan::Offer
    }
}

/// Stamp `last_check` and return a manifest when one should be offered.
/// Offline and a parse error return `Ok(None)`: nothing is shown.
pub fn automatic(opted_in: bool, launch: Launch) -> Option<Manifest> {
    let path = state_path();
    let state = path.as_deref().map(State::load).unwrap_or_default();
    if !should_check(opted_in, &state, now_secs(), launch, forced_off()) {
        return None;
    }
    let skipped = state.skipped.clone();
    let offered = fetch(&endpoint())
        .ok()
        .filter(|manifest| offer(cli::VERSION, manifest, &skipped));
    if let Some(path) = path.as_deref() {
        let mut state = state;
        state.last_check = Some(now_secs());
        state.available = offered
            .as_ref()
            .map(|manifest| manifest.version.clone())
            .unwrap_or_default();
        let _ = state.save(path);
    }
    offered
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const GOOD_SHA: &str = "89cdc2d1993dde0607d942103f393fb5bda6b38e1666ef96c3534d2c56e59f9b";

    /// One asset of a GitHub release, as the API lists it.
    fn asset(id: u64, name: &str, url: &str, digest: Option<&str>) -> String {
        let digest = digest.map_or_else(|| String::from("null"), |d| format!("\"{d}\""));
        format!(
            r#"{{
      "url": "https://api.github.com/repos/CLoaKY233/siltop/releases/assets/{id}",
      "id": {id},
      "node_id": "RA_kwDOAbCdEs4AAAAB",
      "name": "{name}",
      "label": "",
      "uploader": {{ "login": "github-actions[bot]", "id": 41898282, "type": "Bot" }},
      "content_type": "application/gzip",
      "state": "uploaded",
      "size": 1834201,
      "digest": {digest},
      "download_count": 12,
      "created_at": "2026-10-01T09:12:44Z",
      "updated_at": "2026-10-01T09:12:46Z",
      "browser_download_url": "{url}"
    }}"#
        )
    }

    /// The body of `GET /repos/CLoaKY233/siltop/releases/latest` for `tag`,
    /// trimmed to the fields that matter plus enough of the rest to be
    /// realistic. `digest` is the archive's; `base` is where its assets live.
    fn release_json(tag: &str, digest: Option<&str>, base: &str) -> String {
        let bin = env!("CARGO_BIN_NAME");
        let archive = format!("{bin}-{tag}-macos-arm64.tar.gz");
        let zip = format!("{bin}-{tag}-macos-arm64.zip");
        let assets = [
            asset(1, &archive, &format!("{base}/file"), digest),
            asset(
                2,
                &format!("{archive}.sha256"),
                &format!("{base}/sha"),
                None,
            ),
            asset(3, &zip, &format!("{base}/zip"), None),
        ]
        .join(",\n    ");
        format!(
            r#"{{
  "url": "https://api.github.com/repos/CLoaKY233/siltop/releases/250000000",
  "html_url": "https://github.com/CLoaKY233/siltop/releases/tag/{tag}",
  "assets_url": "https://api.github.com/repos/CLoaKY233/siltop/releases/250000000/assets",
  "id": 250000000,
  "author": {{ "login": "CLoaKY233", "id": 1, "type": "User" }},
  "node_id": "RE_kwDOAbCdEs4O5uX0",
  "tag_name": "{tag}",
  "target_commitish": "master",
  "name": "{tag}",
  "draft": false,
  "immutable": false,
  "prerelease": false,
  "created_at": "2026-10-01T09:01:12Z",
  "updated_at": "2026-10-01T09:13:02Z",
  "published_at": "2026-10-01T09:13:02Z",
  "assets": [
    {assets}
  ],
  "tarball_url": "https://api.github.com/repos/CLoaKY233/siltop/tarball/{tag}",
  "zipball_url": "https://api.github.com/repos/CLoaKY233/siltop/zipball/{tag}",
  "body": "Fixes and a new theme."
}}"#
        )
    }

    /// A parsed release whose archive GitHub has digested.
    fn manifest(version: &str) -> Manifest {
        let json = release_json(
            &format!("v{version}"),
            Some(&format!("sha256:{GOOD_SHA}")),
            "https://github.com/CLoaKY233/siltop/releases/download",
        );
        let release = parse_release(&json).unwrap();
        let sha = release.sha256.clone().unwrap();
        release.into_manifest(sha)
    }

    struct Temp(PathBuf);
    impl Temp {
        fn new(name: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "siltop-upd-{}-{name}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn versions_order_releases_and_prereleases() {
        assert_eq!(cmp_version("1.0.0", "1.1.0"), std::cmp::Ordering::Less);
        assert_eq!(cmp_version("1.1.0", "1.1.0"), std::cmp::Ordering::Equal);
        assert_eq!(
            cmp_version("1.1.0-rc.1", "1.1.0-rc.2"),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            cmp_version("1.1.0-rc.2", "1.1.0-rc.10"),
            std::cmp::Ordering::Less
        );
        assert_eq!(cmp_version("1.1.0-rc.2", "1.1.0"), std::cmp::Ordering::Less);
        assert_eq!(cmp_version("v1.2.0", "1.2.0"), std::cmp::Ordering::Equal);
        assert!(is_newer("1.1.0-rc.1", "1.1.0-rc.2"));
        assert!(!is_newer("1.1.0", "1.1.0-rc.2"));
    }

    #[test]
    fn a_github_release_parses_into_version_archive_digest_and_page() {
        let json = release_json(
            "v1.3.0",
            Some(&format!("sha256:{}", GOOD_SHA.to_ascii_uppercase())),
            "https://github.com/CLoaKY233/siltop/releases/download/v1.3.0",
        );
        let release = parse_release(&json).unwrap();
        let bin = env!("CARGO_BIN_NAME");
        assert_eq!(release.version, "1.3.0", "the leading v is dropped");
        assert_eq!(release.released, "2026-10-01T09:13:02Z");
        assert_eq!(
            release.url,
            "https://github.com/CLoaKY233/siltop/releases/download/v1.3.0/file"
        );
        assert_eq!(release.sha256.as_deref(), Some(GOOD_SHA), "lowercased");
        assert_eq!(
            release.notes,
            "https://github.com/CLoaKY233/siltop/releases/tag/v1.3.0"
        );
        assert!(
            release
                .sha256_url
                .as_deref()
                .is_some_and(|url| url.ends_with("/sha")),
            "{bin}: {release:?}"
        );
    }

    #[test]
    fn a_release_without_a_digest_points_at_its_sha256_asset() {
        let json = release_json("v1.3.0", None, "https://example.test");
        let release = parse_release(&json).unwrap();
        assert_eq!(release.sha256, None);
        assert_eq!(
            release.sha256_url.as_deref(),
            Some("https://example.test/sha")
        );
        let line = format!("{GOOD_SHA}  siltop-v1.3.0-macos-arm64.tar.gz\n");
        assert_eq!(parse_sha256_file(&line).unwrap(), GOOD_SHA);
        assert!(matches!(
            parse_sha256_file("abcd  file\n"),
            Err(ManifestError::BadSha)
        ));
    }

    #[test]
    fn a_release_rejects_a_missing_tag_or_archive_and_a_bad_digest() {
        let base = "https://example.test";
        let good = release_json("v1.3.0", Some(&format!("sha256:{GOOD_SHA}")), base);
        assert!(parse_release(&good).is_ok());
        let untagged = good.replace(r#""tag_name": "v1.3.0""#, r#""tag_name": """#);
        assert!(matches!(
            parse_release(&untagged),
            Err(ManifestError::Missing("tag"))
        ));
        let no_archive = good.replace("macos-arm64.tar.gz", "linux-x86_64.tar.gz");
        assert!(matches!(
            parse_release(&no_archive),
            Err(ManifestError::Missing("macOS archive"))
        ));
        let short = release_json("v1.3.0", Some("sha256:abcd"), base);
        assert!(matches!(parse_release(&short), Err(ManifestError::BadSha)));
        let other_algo = release_json("v1.3.0", Some(&format!("sha512:{GOOD_SHA}")), base);
        assert!(matches!(
            parse_release(&other_algo),
            Err(ManifestError::BadSha)
        ));
        assert!(matches!(
            parse_release("not json"),
            Err(ManifestError::Json(_))
        ));
        assert!(matches!(
            parse_release(r#"{"message":"Not Found","status":"404"}"#),
            Err(ManifestError::Missing("tag"))
        ));
    }

    #[test]
    fn the_archive_is_this_builds_name_or_the_only_one_with_its_tail() {
        let at = |name: &str| RawAsset {
            name: name.to_owned(),
            browser_download_url: format!("https://example.test/{name}"),
            digest: None,
        };
        let bin = env!("CARGO_BIN_NAME");
        let exact = format!("{bin}-v2.0.0-macos-arm64.tar.gz");
        let assets = [at("other-v2.0.0-macos-arm64.tar.gz"), at(&exact)];
        assert_eq!(archive(&assets, "v2.0.0").unwrap().name, exact);
        let renamed = [at("renamed-v2.0.0-macos-arm64.tar.gz"), at("notes.txt")];
        assert_eq!(
            archive(&renamed, "v2.0.0").unwrap().name,
            "renamed-v2.0.0-macos-arm64.tar.gz"
        );
        let two = [
            at("a-v2.0.0-macos-arm64.tar.gz"),
            at("b-v2.0.0-macos-arm64.tar.gz"),
        ];
        assert!(archive(&two, "v2.0.0").is_none(), "never a guess");
    }

    #[test]
    fn the_check_is_off_until_opted_in_and_then_once_a_day() {
        let launch = Launch {
            tty: true,
            ci: false,
            disabled: false,
            mode: crate::cli::Mode::Tui,
            demo: false,
        };
        let fresh = State::default();
        assert!(!should_check(false, &fresh, 1_000, launch, false));
        assert!(should_check(true, &fresh, 1_000, launch, false));
        assert!(!should_check(true, &fresh, 1_000, launch, true));
        let recent = State {
            last_check: Some(1_000),
            ..State::default()
        };
        assert!(!should_check(true, &recent, 1_000 + 60, launch, false));
        assert!(should_check(
            true,
            &recent,
            1_000 + THROTTLE.as_secs(),
            launch,
            false
        ));
        let mut headless = launch;
        headless.mode = crate::cli::Mode::Json;
        assert!(!should_check(true, &fresh, 1_000, headless, false));
        let mut ci = launch;
        ci.ci = true;
        assert!(!should_check(true, &fresh, 1_000, ci, false));
        // SILTOP_NO_ONBOARDING skips the screens, not an opted-in check.
        let mut no_onboarding = launch;
        no_onboarding.disabled = true;
        assert!(should_check(true, &fresh, 1_000, no_onboarding, false));
        // A known update is offered again inside the throttle window,
        // unless it was skipped.
        let pending = State {
            last_check: Some(1_000),
            available: String::from("99.0.0"),
            ..State::default()
        };
        assert!(should_check(true, &pending, 1_000 + 60, launch, false));
        let skipped = State {
            skipped: String::from("99.0.0"),
            ..pending
        };
        assert!(!should_check(true, &skipped, 1_000 + 60, launch, false));
    }

    #[test]
    fn a_skipped_version_is_not_offered_and_a_later_one_is() {
        let parsed = manifest("1.1.1");
        assert!(offer("1.1.0", &parsed, ""));
        assert!(!offer("1.1.0", &parsed, "1.1.1"));
        assert!(!offer("1.1.1", &parsed, ""));
    }

    #[test]
    fn state_roundtrips_and_a_missing_file_is_empty() {
        let dir = Temp::new("state");
        let path = dir.0.join("updates.toml");
        assert_eq!(State::load(&path), State::default());
        let state = State {
            last_check: Some(1_700_000_000),
            skipped: String::from("1.1.1"),
            available: String::from("1.2.0"),
        };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path), state);
        // The file is real TOML, empty values included.
        let text = fs::read_to_string(&path).unwrap();
        let parsed: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(parsed["skipped"].as_str(), Some("1.1.1"));
        State::default().save(&path).unwrap();
        let empty: toml_edit::DocumentMut = fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(empty["skipped"].as_str(), Some(""));
    }

    #[test]
    fn a_keg_is_brew_cargo_is_cargo_and_any_other_bin_is_a_download() {
        let dir = Temp::new("kind");
        let place = |rel: &str| {
            let bin = dir.0.join(rel);
            fs::create_dir_all(&bin).unwrap();
            let exe = bin.join("siltop");
            fs::write(&exe, b"x").unwrap();
            exe
        };
        assert_eq!(
            classify(&place("opt/homebrew/Cellar/siltop/1.0.0/bin"), None),
            InstallKind::Brew
        );
        assert_eq!(
            classify(&place("home/.local/bin"), None),
            InstallKind::Download
        );
        // /usr/local/bin is a common place to unpack a download.
        assert_eq!(
            classify(&place("usr/local/bin"), None),
            InstallKind::Download
        );
        // `cargo install` owns ~/.cargo/bin, and $CARGO_HOME/bin when set.
        assert_eq!(
            classify(&place("home/.cargo/bin"), None),
            InstallKind::Cargo
        );
        let cargo_home = dir.0.join("rust/cargo");
        assert_eq!(
            classify(&place("rust/cargo/bin"), Some(&cargo_home)),
            InstallKind::Cargo
        );
        // A build in target/release is a developer's copy, never replaced.
        assert_eq!(
            classify(&place("home/dev/siltop/target/release"), None),
            InstallKind::Other
        );
        assert_eq!(
            classify(Path::new("/usr/bin/siltop"), None),
            InstallKind::Other
        );
    }

    #[test]
    fn curl_errors_are_one_line_and_a_rate_limit_says_so() {
        let tls = "curl: (60) SSL certificate problem: self signed certificate\nMore details here: https://curl.se/docs/sslcerts.html\n\ncurl failed to verify the legitimacy of the server\n";
        assert_eq!(
            curl_reason(tls),
            "(60) SSL certificate problem: self signed certificate"
        );
        let limited = "curl: (22) The requested URL returned error: 403\n";
        assert!(
            curl_reason(limited).contains("rate-limiting"),
            "{}",
            curl_reason(limited)
        );
        assert_eq!(curl_reason(""), "curl failed");
    }

    /// A loopback server for up to four requests: `/latest` answers `body`
    /// (with `{BASE}` replaced by the server's own URL) only to a request
    /// that asks for GitHub JSON, `/file` the archive, `/sha` a `shasum`
    /// line for [`GOOD_SHA`].
    fn fixture(dir: &Path, body: &str, file: Option<(&str, &[u8])>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let body = body.replace("{BASE}", &base);
        let file_bytes = file.map(|(_, bytes)| bytes.to_vec());
        let file_name = file.map(|(name, _)| name.to_owned());
        std::thread::spawn(move || {
            let _ = dir;
            for _ in 0..4 {
                let Ok((mut sock, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0_u8; 1024];
                let _ = std::io::Read::read(&mut sock, &mut buf);
                let request = String::from_utf8_lossy(&buf);
                let github = request.contains(&format!("Accept: {GITHUB_JSON}"));
                let payload = if request.contains("GET /file") {
                    file_bytes.clone().unwrap_or_default()
                } else if request.contains("GET /sha") {
                    format!("{GOOD_SHA}  p.tar.gz\n").into_bytes()
                } else if request.contains("GET /latest") && github {
                    body.as_bytes().to_vec()
                } else {
                    b"no".to_vec()
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = std::io::Write::write_all(&mut sock, header.as_bytes());
                let _ = std::io::Write::write_all(&mut sock, &payload);
                let _ = file_name;
            }
        });
        base
    }

    #[test]
    fn a_good_release_fetches_and_offline_is_an_error_the_caller_can_hide() {
        let dir = Temp::new("fetch");
        let digest = format!("sha256:{GOOD_SHA}");
        let body = release_json("v9.9.9", Some(&digest), "{BASE}");
        let base = fixture(&dir.0, &body, None);
        let parsed = fetch(&format!("{base}/latest")).unwrap();
        assert_eq!(parsed.version, "9.9.9");
        assert_eq!(parsed.sha256, GOOD_SHA);
        assert_eq!(parsed.url, format!("{base}/file"));
        let err = fetch("http://127.0.0.1:1/latest").unwrap_err();
        assert!(matches!(err, FetchError::Offline(_)), "{err}");
    }

    #[test]
    fn a_release_without_a_digest_fetches_its_sha256_asset() {
        let dir = Temp::new("fetch-sha");
        let body = release_json("v9.9.9", None, "{BASE}");
        let base = fixture(&dir.0, &body, None);
        let parsed = fetch(&format!("{base}/latest")).unwrap();
        assert_eq!(parsed.version, "9.9.9");
        assert_eq!(parsed.sha256, GOOD_SHA);
        let lone = body.replace(".tar.gz.sha256", ".tar.gz.sig");
        let base = fixture(&dir.0, &lone, None);
        assert!(matches!(
            fetch(&format!("{base}/latest")),
            Err(FetchError::Manifest(ManifestError::Missing("sha256")))
        ));
    }

    #[test]
    fn sha_mismatch_truncated_archive_bad_version_and_readonly_keep_the_old_binary() {
        let dir = Temp::new("install");
        let home = dir.0.join("home");
        let bin = home.join(".local/bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("siltop");
        fs::write(&exe, b"original-binary").unwrap();
        let mismatch = Manifest {
            version: String::from("1.1.1"),
            released: String::new(),
            url: String::from("http://127.0.0.1:1/missing"),
            sha256: GOOD_SHA.to_owned(),
            notes: String::new(),
        };
        // Point the url at a server after we have bytes to hash.
        let bytes = b"not a tarball";
        let base = fixture(&dir.0, "", Some(("p.tar.gz", bytes)));
        let mut bad_sha = mismatch.clone();
        bad_sha.url = format!("{base}/file");
        bad_sha.sha256 = "a".repeat(64);
        let err = install_in(&exe, &bad_sha, None).unwrap_err();
        assert!(matches!(err, InstallError::Sha), "{err}");
        assert_eq!(fs::read(&exe).unwrap(), b"original-binary");

        let mut truncated = bad_sha.clone();
        truncated.sha256 = file_sha256_for_test(bytes);
        let err = install_in(&exe, &truncated, None).unwrap_err();
        assert!(matches!(err, InstallError::Archive(_)), "{err}");
        assert_eq!(fs::read(&exe).unwrap(), b"original-binary");

        let readonly = dir.0.join("ro");
        fs::create_dir_all(&readonly).unwrap();
        let mut perms = fs::metadata(&readonly).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&readonly, perms).unwrap();
        // A download whose directory is read-only.
        let ro_home = dir.0.join("rohome");
        let ro_bin = ro_home.join(".local/bin");
        fs::create_dir_all(&ro_bin).unwrap();
        let ro_exe = ro_bin.join("siltop");
        fs::write(&ro_exe, b"original-binary").unwrap();
        let mut ro_perms = fs::metadata(&ro_bin).unwrap().permissions();
        ro_perms.set_readonly(true);
        fs::set_permissions(&ro_bin, ro_perms.clone()).unwrap();
        let err = install_in(&ro_exe, &truncated, None).unwrap_err();
        assert!(matches!(err, InstallError::NotWritable(_)), "{err}");
        assert_eq!(fs::read(&ro_exe).unwrap(), b"original-binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ro_perms.set_mode(0o755);
        }
        let _ = fs::set_permissions(&ro_bin, ro_perms);
    }

    #[test]
    fn the_four_dialog_choices_do_what_they_say() {
        for choice in DialogChoice::ALL {
            let mut config = siltop_core::Config::default();
            let mut state = State::default();
            let effect = apply_dialog(choice, "1.1.1", &mut config, &mut state);
            match choice {
                DialogChoice::Now => {
                    assert!(effect.install_now);
                    assert!(!config.updates_auto_install);
                    assert!(state.skipped.is_empty());
                }
                DialogChoice::Always => {
                    assert!(effect.install_now, "Always installs this version too");
                    assert!(config.updates_auto_install);
                    assert!(state.skipped.is_empty());
                    assert_eq!(
                        plan_check(config.updates_auto_install, InstallKind::Download, true),
                        CheckPlan::Install,
                        "the next check installs a download without asking"
                    );
                    assert_eq!(
                        plan_check(config.updates_auto_install, InstallKind::Brew, true),
                        CheckPlan::Offer,
                        "a keg is still offered, never replaced"
                    );
                }
                DialogChoice::Skip => {
                    assert!(!effect.install_now);
                    assert!(!config.updates_auto_install);
                    assert_eq!(state.skipped, "1.1.1");
                    let manifest = manifest("1.1.1");
                    assert!(!offer("1.1.0", &manifest, &state.skipped));
                }
                DialogChoice::Later => {
                    assert!(!effect.install_now);
                    assert!(!config.updates_auto_install);
                    assert!(state.skipped.is_empty());
                    assert_eq!(
                        plan_check(false, InstallKind::Download, true),
                        CheckPlan::Offer
                    );
                }
            }
        }
        assert_eq!(
            plan_check(true, InstallKind::Download, false),
            CheckPlan::Quiet
        );
    }

    fn file_sha256_for_test(bytes: &[u8]) -> String {
        let dir = std::env::temp_dir().join(format!("siltop-sha-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("blob");
        fs::write(&path, bytes).unwrap();
        file_sha256(&path).unwrap()
    }

    #[test]
    fn a_new_binary_whose_version_does_not_match_is_not_swapped() {
        let dir = Temp::new("ver");
        let home = dir.0.join("home");
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("siltop");
        fs::write(&exe, b"original-binary").unwrap();
        // A tarball whose binary is /bin/echo, so --version does not report 9.9.9.
        // Building a tar of a shell script named siltop that prints the wrong version.
        let stage = dir.0.join("stage");
        fs::create_dir_all(&stage).unwrap();
        let script = stage.join("siltop");
        fs::write(&script, b"#!/bin/sh\necho siltop 0.0.1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let archive = dir.0.join("p.tar.gz");
        let packed = Command::new("/usr/bin/tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&stage)
            .arg("siltop")
            .status()
            .unwrap();
        assert!(packed.success());
        let bytes = fs::read(&archive).unwrap();
        let base = fixture(&dir.0, "", Some(("p.tar.gz", &bytes)));
        let manifest = Manifest {
            version: String::from("9.9.9"),
            released: String::new(),
            url: format!("{base}/file"),
            sha256: file_sha256(&archive).unwrap(),
            notes: String::new(),
        };
        let err = install_in(&exe, &manifest, None).unwrap_err();
        assert!(matches!(err, InstallError::Version { .. }), "{err}");
        assert_eq!(fs::read(&exe).unwrap(), b"original-binary");
        assert!(!bin.join("siltop.old").exists());
    }

    #[test]
    fn a_matching_binary_is_swapped_and_the_old_one_is_kept() {
        let dir = Temp::new("good");
        let home = dir.0.join("home");
        let bin = home.join(".local/bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("siltop");
        fs::write(&exe, b"original-binary").unwrap();
        let stage = dir.0.join("stage/siltop-v9");
        fs::create_dir_all(&stage).unwrap();
        let script = stage.join("siltop");
        fs::write(&script, b"#!/bin/sh\necho siltop 9.9.9\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let archive = dir.0.join("p.tar.gz");
        assert!(
            Command::new("/usr/bin/tar")
                .args(["-czf"])
                .arg(&archive)
                .arg("-C")
                .arg(dir.0.join("stage"))
                .arg("siltop-v9")
                .status()
                .unwrap()
                .success()
        );
        let bytes = fs::read(&archive).unwrap();
        let base = fixture(&dir.0, "", Some(("p.tar.gz", &bytes)));
        let manifest = Manifest {
            version: String::from("9.9.9"),
            released: String::new(),
            url: format!("{base}/file"),
            sha256: file_sha256(&archive).unwrap(),
            notes: String::from("notes"),
        };
        install_in(&exe, &manifest, None).unwrap();
        let installed = fs::read_to_string(&exe).unwrap();
        assert!(installed.contains("9.9.9"), "{installed}");
        assert_eq!(
            fs::read(bin.join("siltop.old")).unwrap(),
            b"original-binary"
        );
        rollback_in(&exe, None).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"original-binary");
    }

    /// A curl-style install at `<home>/.local/bin/<name>`, and a manifest
    /// served from loopback whose archive holds `siltop` running `script`.
    fn staged(dir: &Path, name: &str, version: &str, script: &str) -> (PathBuf, PathBuf, Manifest) {
        let home = dir.join("home");
        let bin = home.join(".local/bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(name);
        fs::write(&exe, b"original-binary").unwrap();
        let stage = dir.join("stage");
        fs::create_dir_all(&stage).unwrap();
        fs::write(stage.join("siltop"), script).unwrap();
        let archive = dir.join("p.tar.gz");
        assert!(
            Command::new("/usr/bin/tar")
                .args(["-czf"])
                .arg(&archive)
                .arg("-C")
                .arg(&stage)
                .arg("siltop")
                .status()
                .unwrap()
                .success()
        );
        let bytes = fs::read(&archive).unwrap();
        let base = fixture(dir, "", Some(("p.tar.gz", &bytes)));
        let manifest = Manifest {
            version: version.to_owned(),
            released: String::new(),
            url: format!("{base}/file"),
            sha256: file_sha256(&archive).unwrap(),
            notes: String::new(),
        };
        (home, exe, manifest)
    }

    fn no_scratch_left(exe: &Path) {
        let dir = exe.parent().unwrap();
        assert!(!dir.join(".siltop-download.tar.gz").exists());
        assert!(!dir.join(".siltop-new").exists());
    }

    #[test]
    fn the_new_binary_must_report_exactly_the_manifest_version() {
        assert!(same_version("1.1.0", "1.1.0"));
        assert!(same_version("v1.1.0-rc.1", "1.1.0-rc.1+abc"));
        assert!(!same_version("1.1.0", "1.1.0-rc.1"));
        assert!(!same_version("1.1.1", "1.1.10"));
        assert!(!same_version("1.1", "1.1.0"));
        assert!(!same_version("1.1.0-", "1.1.0-"));
        assert!(!same_version("junk", "junk"));
        assert_eq!(
            reported_version("siltop 1.1.0-rc.1 (abc1234)\n"),
            Some("1.1.0-rc.1")
        );
        assert_eq!(reported_version("siltop 1.1.1\n"), Some("1.1.1"));
        assert_eq!(reported_version("other 1.1.0"), None);
    }

    #[test]
    fn a_prerelease_binary_is_not_installed_as_the_release() {
        let dir = Temp::new("pre");
        let (_, exe, manifest) = staged(
            &dir.0,
            "siltop",
            "1.1.0",
            "#!/bin/sh\necho siltop 1.1.0-rc.1\n",
        );
        let err = install_in(&exe, &manifest, None).unwrap_err();
        assert!(matches!(err, InstallError::Version { .. }), "{err}");
        assert_eq!(fs::read(&exe).unwrap(), b"original-binary");
        no_scratch_left(&exe);
    }

    #[test]
    fn a_longer_patch_number_is_not_a_match() {
        let dir = Temp::new("patch");
        let (_, exe, manifest) =
            staged(&dir.0, "siltop", "1.1.1", "#!/bin/sh\necho siltop 1.1.10\n");
        let err = install_in(&exe, &manifest, None).unwrap_err();
        assert!(matches!(err, InstallError::Version { .. }), "{err}");
        assert_eq!(fs::read(&exe).unwrap(), b"original-binary");
        no_scratch_left(&exe);
    }

    #[test]
    fn a_binary_that_does_not_run_is_an_exec_error_not_a_version() {
        let dir = Temp::new("exec");
        let (_, exe, manifest) = staged(
            &dir.0,
            "siltop",
            "9.9.9",
            "#!/bin/sh\necho broken >&2\nexit 3\n",
        );
        let err = install_in(&exe, &manifest, None).unwrap_err();
        assert!(matches!(err, InstallError::Exec(_)), "{err}");
        let text = err.to_string();
        assert!(
            text.contains("broken") && !text.contains("reported"),
            "{text}"
        );
        assert_eq!(fs::read(&exe).unwrap(), b"original-binary");
        no_scratch_left(&exe);
    }

    #[test]
    fn a_failed_download_or_bad_sha_leaves_no_scratch_files() {
        let dir = Temp::new("scratch");
        let (_, exe, mut manifest) =
            staged(&dir.0, "siltop", "9.9.9", "#!/bin/sh\necho siltop 9.9.9\n");
        manifest.sha256 = "a".repeat(64);
        assert!(matches!(
            install_in(&exe, &manifest, None),
            Err(InstallError::Sha)
        ));
        no_scratch_left(&exe);
        manifest.url = String::from("http://127.0.0.1:1/file");
        assert!(matches!(
            install_in(&exe, &manifest, None),
            Err(InstallError::Download(_))
        ));
        no_scratch_left(&exe);
    }

    #[test]
    fn a_renamed_copy_updates_and_rolls_back_itself_not_its_sibling() {
        let dir = Temp::new("renamed");
        let (_, exe, manifest) =
            staged(&dir.0, "pt-copy", "9.9.9", "#!/bin/sh\necho siltop 9.9.9\n");
        let sibling = exe.with_file_name("siltop");
        fs::write(&sibling, b"sibling").unwrap();
        install_in(&exe, &manifest, None).unwrap();
        assert!(fs::read_to_string(&exe).unwrap().contains("9.9.9"));
        assert_eq!(
            fs::read(exe.with_file_name("pt-copy.old")).unwrap(),
            b"original-binary"
        );
        assert_eq!(fs::read(&sibling).unwrap(), b"sibling");
        assert!(!exe.with_file_name("siltop.old").exists());
        rollback_in(&exe, None).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"original-binary");
        assert_eq!(fs::read(&sibling).unwrap(), b"sibling");
        let err = rollback_in(&exe, None).unwrap_err();
        assert_eq!(err.to_string(), "there is no pt-copy.old to restore");
    }

    #[test]
    fn rollback_on_a_keg_names_brew_in_plain_words() {
        let dir = Temp::new("kegroll");
        let cellar = dir.0.join("opt/homebrew/Cellar/siltop/1.0.0/bin");
        fs::create_dir_all(&cellar).unwrap();
        let keg = cellar.join("siltop");
        fs::write(&keg, b"keg").unwrap();
        let text = rollback_in(&keg, None).unwrap_err().to_string();
        assert!(text.contains("brew upgrade siltop"), "{text}");
        assert!(!text.contains("download the update from"), "{text}");
    }

    #[test]
    fn bare_update_says_how_to_install_for_each_kind() {
        assert!(
            how_to_install(InstallKind::Download, "1.3.0", "u").contains("siltop update --yes")
        );
        assert!(how_to_install(InstallKind::Brew, "1.3.0", "u").contains("brew upgrade siltop"));
        assert!(how_to_install(InstallKind::Cargo, "1.3.0", "u").contains("--tag v1.3.0"));
        assert!(how_to_install(InstallKind::Other, "1.3.0", "https://x/p").contains("https://x/p"));
    }
}
