//! Network throughput from exact 64-bit `ifmib` counters (D29) with the
//! primary interface taken from the default route (D30).
//!
//! `getifaddrs`' `if_data` and `NET_RT_IFLIST2` both truncate byte counters
//! to 32 bits for binaries without Apple's private entitlement; the
//! `net.link.generic.ifdata.<idx>.general` sysctl returns the full
//! `if_data64`. See `docs/research/02-macos-metrics.md` §6.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use siltop_core::{CollectorId, IfaceRate, NetworkSnapshot, Result, Snapshot};

use crate::cadence::Pace;
use crate::collector::{Collector, Outcome};

/// How often the default route is re-resolved.
pub(crate) const ROUTE_EVERY: Duration = Duration::from_secs(2);

/// `IFF_UP` from `<net/if.h>`.
const IFF_UP: u32 = 0x1;

/// Byte offsets inside `struct ifmibdata` (`<net/if_mib.h>`), verified with
/// `offsetof` on macOS 27: total 180, `ifmd_data` (packed `if_data64`) at 52.
pub(crate) const IFMIB_SIZE: usize = 180;
const IFMIB_FLAGS: usize = 20;
const IFMIB_BAUD: usize = 52 + 16;
const IFMIB_IBYTES: usize = 52 + 64;
const IFMIB_OBYTES: usize = 52 + 72;

/// `sizeof(struct rt_msghdr)` on Darwin.
pub(crate) const RT_MSGHDR_LEN: usize = 92;
const RTM_VERSION: u8 = 5;
const RTM_GET: u8 = 4;
const RTA_DST: i32 = 0x1;
const RTF_UP: i32 = 0x1;
const RTF_GATEWAY: i32 = 0x2;
const RTF_STATIC: i32 = 0x800;

/// Coarse interface classes; only [`IfaceKind::Physical`] is listed by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IfaceKind {
    Physical,
    Loopback,
    Bridge,
    /// awdl, llw, ap, anpi: Apple's peer-to-peer and internal links.
    Internal,
    Tunnel,
    Other,
}

/// Classify a BSD interface name. The physical set is `en<N>`; everything
/// else (bridge, anpi, ap, awdl, llw, utun, ipsec, gif, stf, lo) is not.
pub(crate) fn classify(name: &str) -> IfaceKind {
    let digits_after = |prefix: &str| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    };
    if digits_after("en") {
        IfaceKind::Physical
    } else if digits_after("lo") {
        IfaceKind::Loopback
    } else if digits_after("bridge") {
        IfaceKind::Bridge
    } else if ["awdl", "llw", "ap", "anpi"]
        .iter()
        .any(|p| digits_after(p))
    {
        IfaceKind::Internal
    } else if ["utun", "ipsec", "gif", "stf", "ppp", "tun", "tap"]
        .iter()
        .any(|p| digits_after(p))
    {
        IfaceKind::Tunnel
    } else {
        IfaceKind::Other
    }
}

pub(crate) fn is_physical(name: &str) -> bool {
    classify(name) == IfaceKind::Physical
}

/// Counter delta between two exact 64-bit `ifmib` counters. A counter that
/// goes backwards means the interface was re-created (XNU clears `if_data`
/// on attach), so the delta is 0 rather than a spike. There is no 32-bit
/// wrap to undo: reading a reset below 4 GiB as one turned a replugged
/// dongle into a one-sample 30–60 Gb/s spike.
pub(crate) fn counter_delta(prev: u64, cur: u64) -> u64 {
    cur.saturating_sub(prev)
}

/// Bytes over `dt` seconds as bits per second; 0 for a non-positive or
/// non-finite interval.
pub(crate) fn bits_per_sec(delta_bytes: u64, dt: f64) -> u64 {
    if !dt.is_finite() || dt <= 0.0 {
        return 0;
    }
    let bps = delta_bytes as f64 * 8.0 / dt;
    if bps.is_finite() { bps as u64 } else { 0 }
}

/// `ifi_baudrate` of 0 or the 0xFFFFFFFF sentinel means "unknown".
pub(crate) fn link_speed(baud: u64) -> Option<u64> {
    if baud == 0 || baud == u64::from(u32::MAX) || baud == u64::MAX {
        None
    } else {
        Some(baud)
    }
}

/// One interface's raw `ifmib` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IfCounters {
    pub index: u32,
    pub name: String,
    pub up: bool,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub baud: u64,
}

fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
    let bytes: [u8; 4] = buf.get(at..at + 4)?.try_into().ok()?;
    Some(u32::from_ne_bytes(bytes))
}

fn read_u64(buf: &[u8], at: usize) -> Option<u64> {
    let bytes: [u8; 8] = buf.get(at..at + 8)?.try_into().ok()?;
    Some(u64::from_ne_bytes(bytes))
}

/// Parse a `struct ifmibdata` as returned by the `IFDATA_GENERAL` sysctl.
pub(crate) fn parse_ifmibdata(index: u32, buf: &[u8]) -> Option<IfCounters> {
    if buf.len() < IFMIB_SIZE {
        return None;
    }
    let name_raw = buf.get(..16)?;
    let n = name_raw.iter().position(|&b| b == 0).unwrap_or(16);
    let name = String::from_utf8_lossy(&name_raw[..n]).into_owned();
    if name.is_empty() {
        return None;
    }
    Some(IfCounters {
        index,
        name,
        up: read_u32(buf, IFMIB_FLAGS)? & IFF_UP != 0,
        rx_bytes: read_u64(buf, IFMIB_IBYTES)?,
        tx_bytes: read_u64(buf, IFMIB_OBYTES)?,
        baud: read_u64(buf, IFMIB_BAUD)?,
    })
}

/// Build an `RTM_GET` request for the IPv4 (`0.0.0.0`) or IPv6 (`::`)
/// default route.
pub(crate) fn route_request(seq: i32, v6: bool) -> Vec<u8> {
    let sa_len: usize = if v6 { 28 } else { 16 };
    let total = RT_MSGHDR_LEN + sa_len;
    let mut buf = vec![0_u8; total];
    let total16 = u16::try_from(total).unwrap_or(u16::MAX);
    buf[0..2].copy_from_slice(&total16.to_ne_bytes());
    buf[2] = RTM_VERSION;
    buf[3] = RTM_GET;
    buf[8..12].copy_from_slice(&(RTF_UP | RTF_GATEWAY | RTF_STATIC).to_ne_bytes());
    buf[12..16].copy_from_slice(&RTA_DST.to_ne_bytes());
    buf[20..24].copy_from_slice(&seq.to_ne_bytes());
    let family: u8 = if v6 { 30 } else { 2 };
    buf[RT_MSGHDR_LEN] = u8::try_from(sa_len).unwrap_or(0);
    buf[RT_MSGHDR_LEN + 1] = family;
    buf
}

/// Parse a routing-socket reply to [`route_request`]. Returns the interface
/// index of the matched route, or `None` for someone else's message, an
/// error reply, a down route, or a truncated buffer.
pub(crate) fn parse_route_reply(buf: &[u8], seq: i32, pid: i32) -> Option<u32> {
    if buf.len() < RT_MSGHDR_LEN {
        return None;
    }
    let msglen = usize::from(u16::from_ne_bytes([buf[0], buf[1]]));
    if msglen < RT_MSGHDR_LEN || msglen > buf.len() {
        return None;
    }
    if buf[2] != RTM_VERSION || buf[3] != RTM_GET {
        return None;
    }
    let index = u16::from_ne_bytes([buf[4], buf[5]]);
    let flags = i32::from_ne_bytes(buf[8..12].try_into().ok()?);
    let msg_pid = i32::from_ne_bytes(buf[16..20].try_into().ok()?);
    let msg_seq = i32::from_ne_bytes(buf[20..24].try_into().ok()?);
    let errno = i32::from_ne_bytes(buf[24..28].try_into().ok()?);
    if msg_pid != pid || msg_seq != seq || errno != 0 || flags & RTF_UP == 0 || index == 0 {
        return None;
    }
    Some(u32::from(index))
}

#[derive(Debug, Clone, Copy)]
struct Prev {
    rx: u64,
    tx: u64,
}

/// Pure rate state: previous counters per (index, name).
#[derive(Debug, Default)]
pub(crate) struct RateState {
    prev: HashMap<(u32, String), Prev>,
}

impl RateState {
    /// Advance by one sample of `rows` taken `dt` seconds after the last.
    /// A row seen for the first time gets a 0 rate. Rows missing from this
    /// sample keep their previous value (no reset, so no spike on return).
    pub(crate) fn advance(&mut self, rows: &[IfCounters], dt: f64) -> Vec<(u64, u64)> {
        rows.iter()
            .map(|row| {
                let key = (row.index, row.name.clone());
                let rate = self.prev.get(&key).map_or((0, 0), |p| {
                    (
                        bits_per_sec(counter_delta(p.rx, row.rx_bytes), dt),
                        bits_per_sec(counter_delta(p.tx, row.tx_bytes), dt),
                    )
                });
                self.prev.insert(
                    key,
                    Prev {
                        rx: row.rx_bytes,
                        tx: row.tx_bytes,
                    },
                );
                rate
            })
            .collect()
    }
}

/// A physical interface worth listing: up, and either carrying traffic or
/// reporting a link speed. Hides idle Thunderbolt bridge members (`en1`–`en6`
/// on a 14-inch Pro) that are up but have never moved a byte.
pub(crate) fn listed_physical(row: &IfCounters) -> bool {
    row.up
        && is_physical(&row.name)
        && (row.rx_bytes.saturating_add(row.tx_bytes) > 0 || link_speed(row.baud).is_some())
}

/// Build the snapshot from rows, their rates and the primary index.
/// Listed: physical interfaces that are up, plus the primary whatever it is.
pub(crate) fn build_snapshot(
    rows: &[IfCounters],
    rates: &[(u64, u64)],
    primary: Option<u32>,
) -> NetworkSnapshot {
    let mut ifaces = Vec::new();
    let mut primary_name = None;
    for (row, &(rx_bps, tx_bps)) in rows.iter().zip(rates) {
        let is_primary = primary == Some(row.index);
        if is_primary {
            primary_name = Some(row.name.clone());
        }
        if !(is_primary || listed_physical(row)) {
            continue;
        }
        ifaces.push(IfaceRate {
            name: row.name.clone(),
            rx_bps,
            tx_bps,
            rx_bytes: row.rx_bytes,
            tx_bytes: row.tx_bytes,
            link_bps: link_speed(row.baud),
            primary: is_primary,
        });
    }
    let headline = ifaces.iter().find(|i| i.primary).or_else(|| {
        ifaces
            .iter()
            .max_by_key(|i| i.rx_bytes.saturating_add(i.tx_bytes))
    });
    let (iface, rx_bps, tx_bps) = headline.map_or_else(
        || (String::from("—"), 0, 0),
        |i| (i.name.clone(), i.rx_bps, i.tx_bps),
    );
    NetworkSnapshot {
        iface,
        rx_bps,
        tx_bps,
        primary: primary_name,
        ifaces,
    }
}

#[derive(Default)]
pub(crate) struct NetCollector {
    rates: RateState,
    last: Option<Instant>,
    primary: Option<u32>,
    route_checked: Pace,
    seq: i32,
}

impl NetCollector {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// `None` until a previous sample gives the rates a baseline. A tick on
    /// which no interface answers is an error, never a zero rate.
    pub(crate) fn sample(&mut self, now: Instant) -> Result<Option<NetworkSnapshot>> {
        #[cfg(target_os = "macos")]
        {
            if self.route_checked.due(now, ROUTE_EVERY) {
                self.seq = self.seq.wrapping_add(2);
                self.primary = macos::default_route_index(self.seq);
                self.route_checked.ran(now);
            }
            let rows = macos::read_all();
            if rows.is_empty() {
                return Err(siltop_core::Error::system(format!(
                    "ifmib: no interface answered ({})",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(self.advance(&rows, now))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = now;
            Ok(None)
        }
    }

    /// One step over already-read rows: `None` on the first (baseline)
    /// sample, then per-interface rates over the elapsed interval.
    fn advance(&mut self, rows: &[IfCounters], now: Instant) -> Option<NetworkSnapshot> {
        let dt = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f64());
        let rates = self.rates.advance(rows, dt.unwrap_or(0.0));
        self.last = Some(now);
        dt.map(|_| build_snapshot(rows, &rates, self.primary))
    }
}

impl Collector for NetCollector {
    fn id(&self) -> CollectorId {
        CollectorId::Network
    }

    fn collect(&mut self, now: Instant, snap: &mut Snapshot) -> Result<Outcome> {
        if cfg!(not(target_os = "macos")) {
            return Ok(Outcome::Absent);
        }
        Ok(match self.sample(now)? {
            Some(network) => {
                snap.network = network;
                Outcome::Fresh
            }
            None => Outcome::Baseline,
        })
    }
}

#[cfg(target_os = "macos")]
pub(crate) mod macos {
    use super::{IFMIB_SIZE, IfCounters, parse_ifmibdata, parse_route_reply, route_request};
    use std::mem::size_of;
    use std::ptr;

    const PF_LINK: libc::c_int = 18;
    const NETLINK_GENERIC: libc::c_int = 0;
    const IFMIB_SYSTEM: libc::c_int = 1;
    const IFMIB_IFCOUNT: libc::c_int = 1;
    const IFMIB_IFDATA: libc::c_int = 2;
    const IFDATA_GENERAL: libc::c_int = 1;

    fn sysctl_raw(mib: &mut [libc::c_int], out: &mut [u8]) -> Option<usize> {
        let mut len = out.len();
        let rc = unsafe {
            // SAFETY: `mib` is a valid MIB array and `out` a writable buffer of `len` bytes.
            libc::sysctl(
                mib.as_mut_ptr(),
                libc::c_uint::try_from(mib.len()).ok()?,
                out.as_mut_ptr().cast(),
                &raw mut len,
                ptr::null_mut(),
                0,
            )
        };
        (rc == 0).then_some(len)
    }

    fn if_count() -> u32 {
        let mut mib = [
            libc::CTL_NET,
            PF_LINK,
            NETLINK_GENERIC,
            IFMIB_SYSTEM,
            IFMIB_IFCOUNT,
        ];
        let mut buf = [0_u8; size_of::<u32>()];
        sysctl_raw(&mut mib, &mut buf).map_or(0, |_| u32::from_ne_bytes(buf))
    }

    /// Read one interface's `ifmibdata` by index.
    pub(crate) fn read_index(index: u32) -> Option<IfCounters> {
        let mut mib = [
            libc::CTL_NET,
            PF_LINK,
            NETLINK_GENERIC,
            IFMIB_IFDATA,
            libc::c_int::try_from(index).ok()?,
            IFDATA_GENERAL,
        ];
        let mut buf = [0_u8; IFMIB_SIZE];
        let len = sysctl_raw(&mut mib, &mut buf)?;
        parse_ifmibdata(index, buf.get(..len)?)
    }

    /// Every interface index 1..=ifcount that answers.
    pub(crate) fn read_all() -> Vec<IfCounters> {
        (1..=if_count()).filter_map(read_index).collect()
    }

    /// Resolve the default route (IPv4 first, then IPv6) to an interface index.
    pub(crate) fn default_route_index(seq: i32) -> Option<u32> {
        query_route(seq, false).or_else(|| query_route(seq.wrapping_add(1), true))
    }

    fn query_route(seq: i32, v6: bool) -> Option<u32> {
        let fd = unsafe {
            // SAFETY: plain socket(2) call; the fd is closed below.
            libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, 0)
        };
        if fd < 0 {
            return None;
        }
        let out = exchange(fd, seq, v6);
        unsafe {
            // SAFETY: `fd` is the socket opened above and not used afterwards.
            libc::close(fd);
        }
        out
    }

    fn exchange(fd: libc::c_int, seq: i32, v6: bool) -> Option<u32> {
        let tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 100_000,
        };
        let rc = unsafe {
            // SAFETY: `tv` is a valid timeval for SO_RCVTIMEO; size matches.
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&raw const tv).cast(),
                libc::socklen_t::try_from(size_of::<libc::timeval>()).ok()?,
            )
        };
        if rc != 0 {
            return None;
        }
        let req = route_request(seq, v6);
        let wrote = unsafe {
            // SAFETY: `req` is a live buffer of `req.len()` bytes.
            libc::write(fd, req.as_ptr().cast(), req.len())
        };
        if usize::try_from(wrote).ok()? != req.len() {
            return None;
        }
        let pid = unsafe {
            // SAFETY: getpid has no preconditions.
            libc::getpid()
        };
        let mut buf = [0_u8; 1024];
        for _ in 0..16 {
            let n = unsafe {
                // SAFETY: `buf` is writable for its full length.
                libc::read(fd, buf.as_mut_ptr().cast(), buf.len())
            };
            let n = usize::try_from(n).ok()?;
            if let Some(idx) = parse_route_reply(buf.get(..n)?, seq, pid) {
                return Some(idx);
            }
        }
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn row(index: u32, name: &str, up: bool, rx: u64, tx: u64) -> IfCounters {
        IfCounters {
            index,
            name: String::from(name),
            up,
            rx_bytes: rx,
            tx_bytes: tx,
            baud: 0,
        }
    }

    /// A real `ifmibdata` for en0 from this machine's layout: name, flags
    /// (UP|BROADCAST|RUNNING|SIMPLEX|MULTICAST = 0x8863), 31.7 GB received.
    fn ifmib_fixture(name: &str, flags: u32, rx: u64, tx: u64, baud: u64) -> Vec<u8> {
        let mut buf = vec![0_u8; IFMIB_SIZE];
        buf[..name.len()].copy_from_slice(name.as_bytes());
        buf[IFMIB_FLAGS..IFMIB_FLAGS + 4].copy_from_slice(&flags.to_ne_bytes());
        buf[IFMIB_BAUD..IFMIB_BAUD + 8].copy_from_slice(&baud.to_ne_bytes());
        buf[IFMIB_IBYTES..IFMIB_IBYTES + 8].copy_from_slice(&rx.to_ne_bytes());
        buf[IFMIB_OBYTES..IFMIB_OBYTES + 8].copy_from_slice(&tx.to_ne_bytes());
        buf
    }

    #[test]
    fn d29_net_counter_deltas_are_exact_64_bit() {
        // Counters cross 4 GiB without wrapping.
        let prev = u64::from(u32::MAX) - 999;
        assert_eq!(counter_delta(prev, u64::from(u32::MAX) + 501), 1500);
        assert_eq!(counter_delta(31_732_493_725, 31_732_494_725), 1000);
    }

    #[test]
    fn counter_reset_below_4_gib_is_zero_not_a_32_bit_wrap() {
        // A replugged dongle comes back as the same en7 with zeroed counters.
        assert_eq!(counter_delta(100_000_000, 5_000), 0);
        assert_eq!(counter_delta(u64::from(u32::MAX) - 999, 500), 0);
        let mut st = RateState::default();
        st.advance(&[row(7, "en7", true, 1_500_000_000, 20_000)], 0.0);
        let r = st.advance(&[row(7, "en7", true, 5_000, 100)], 0.5);
        assert_eq!(r[0], (0, 0));
    }

    #[test]
    fn d29_counters_are_not_truncated_to_32_bits() {
        let buf = ifmib_fixture("en0", 0x8863, 31_732_493_725, 5_605_907_992, 0);
        let c = parse_ifmibdata(15, &buf).unwrap();
        assert_eq!(c.rx_bytes, 31_732_493_725);
        assert_eq!(c.tx_bytes, 5_605_907_992);
        assert!(c.rx_bytes > u64::from(u32::MAX));
    }

    #[test]
    fn d29_idle_rates_are_not_kib_multiples() {
        let mut st = RateState::default();
        st.advance(&[row(1, "en0", true, 1_000_000, 0)], 0.0);
        let r = st.advance(&[row(1, "en0", true, 1_000_037, 3)], 1.0);
        assert_eq!(r[0], (296, 24));
    }

    #[test]
    fn counter_delta_sweep_never_spikes() {
        for i in 0..500_u64 {
            let prev = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (i % 40);
            let step = i * 7919;
            let cur = prev.wrapping_add(step);
            let d = counter_delta(prev, cur);
            if cur >= prev {
                assert_eq!(d, step);
            } else {
                assert!(u32::try_from(d).is_ok(), "spike {d} at {i}");
            }
        }
    }

    #[test]
    fn counter_delta_crosses_4_gib_exactly_and_never_wraps() {
        for step in [1_u64, 2, 1000, 65_536, 1 << 31] {
            for start_back in [1_u64, 10, 999, 1 << 20] {
                let prev = u64::from(u32::MAX) + 1 - start_back;
                assert_eq!(
                    counter_delta(prev, prev + step),
                    step,
                    "prev {prev} step {step}"
                );
                let truncated = (prev + step) & u64::from(u32::MAX);
                if truncated < prev {
                    assert_eq!(counter_delta(prev, truncated), 0, "prev {prev} step {step}");
                }
            }
        }
    }

    #[test]
    fn counter_reset_of_64_bit_counter_is_zero() {
        assert_eq!(counter_delta(10_000_000_000, 5), 0);
    }

    #[test]
    fn counter_delta_equal_is_zero() {
        assert_eq!(counter_delta(0, 0), 0);
        assert_eq!(counter_delta(u64::MAX, u64::MAX), 0);
    }

    #[test]
    fn bits_per_sec_guards_bad_intervals() {
        assert_eq!(bits_per_sec(1000, 0.0), 0);
        assert_eq!(bits_per_sec(1000, -1.0), 0);
        assert_eq!(bits_per_sec(1000, f64::NAN), 0);
        assert_eq!(bits_per_sec(1000, f64::INFINITY), 0);
        assert_eq!(bits_per_sec(1000, 0.5), 16_000);
        assert_eq!(bits_per_sec(0, 1.0), 0);
        assert!(bits_per_sec(u64::MAX, 1e-9) > 0);
    }

    #[test]
    fn link_speed_sentinels_are_unknown() {
        assert_eq!(link_speed(0), None);
        assert_eq!(link_speed(0xFFFF_FFFF), None);
        assert_eq!(link_speed(u64::MAX), None);
        assert_eq!(link_speed(1_000_000_000), Some(1_000_000_000));
        assert_eq!(link_speed(10_000_000_000), Some(10_000_000_000));
    }

    #[test]
    fn classification_table() {
        let table = [
            ("en0", IfaceKind::Physical),
            ("en12", IfaceKind::Physical),
            ("en", IfaceKind::Other),
            ("enx", IfaceKind::Other),
            ("lo0", IfaceKind::Loopback),
            ("bridge0", IfaceKind::Bridge),
            ("bridge100", IfaceKind::Bridge),
            ("awdl0", IfaceKind::Internal),
            ("llw0", IfaceKind::Internal),
            ("ap1", IfaceKind::Internal),
            ("anpi0", IfaceKind::Internal),
            ("anpi2", IfaceKind::Internal),
            ("utun4", IfaceKind::Tunnel),
            ("ipsec0", IfaceKind::Tunnel),
            ("gif0", IfaceKind::Tunnel),
            ("stf0", IfaceKind::Tunnel),
            ("vmenet0", IfaceKind::Other),
            ("", IfaceKind::Other),
            ("é0", IfaceKind::Other),
        ];
        for (name, kind) in table {
            assert_eq!(classify(name), kind, "{name}");
        }
    }

    #[test]
    fn physical_set_excludes_apple_internal_links() {
        for name in ["bridge0", "anpi0", "ap1", "awdl0", "llw0", "lo0", "utun0"] {
            assert!(!is_physical(name), "{name}");
        }
        for i in 0..64 {
            assert!(is_physical(&format!("en{i}")));
        }
    }

    #[test]
    fn parse_ifmibdata_rejects_short_and_nameless() {
        let buf = ifmib_fixture("en0", 1, 1, 1, 0);
        assert!(parse_ifmibdata(1, &buf[..IFMIB_SIZE - 1]).is_none());
        assert!(parse_ifmibdata(1, &[]).is_none());
        let nameless = ifmib_fixture("", 1, 1, 1, 0);
        assert!(parse_ifmibdata(1, &nameless).is_none());
    }

    #[test]
    fn parse_ifmibdata_flags_and_full_name() {
        let down = ifmib_fixture("en3", 0x8862, 0, 0, 0);
        assert!(!parse_ifmibdata(3, &down).unwrap().up);
        let full = ifmib_fixture("abcdefghijklmnop", 1, 0, 0, 0);
        assert_eq!(parse_ifmibdata(3, &full).unwrap().name, "abcdefghijklmnop");
        let baud = ifmib_fixture("en0", 1, 0, 0, 2_401_000_000);
        assert_eq!(parse_ifmibdata(3, &baud).unwrap().baud, 2_401_000_000);
    }

    #[test]
    fn route_request_layout() {
        let v4 = route_request(7, false);
        assert_eq!(v4.len(), RT_MSGHDR_LEN + 16);
        assert_eq!(usize::from(u16::from_ne_bytes([v4[0], v4[1]])), v4.len());
        assert_eq!(v4[2], RTM_VERSION);
        assert_eq!(v4[3], RTM_GET);
        assert_eq!(i32::from_ne_bytes(v4[12..16].try_into().unwrap()), RTA_DST);
        assert_eq!(i32::from_ne_bytes(v4[20..24].try_into().unwrap()), 7);
        assert_eq!(v4[RT_MSGHDR_LEN], 16);
        assert_eq!(v4[RT_MSGHDR_LEN + 1], 2);
        let v6 = route_request(8, true);
        assert_eq!(v6.len(), RT_MSGHDR_LEN + 28);
        assert_eq!(v6[RT_MSGHDR_LEN + 1], 30);
        assert!(v6[RT_MSGHDR_LEN + 2..].iter().all(|&b| b == 0));
    }

    /// A reply shaped like the kernel's answer on this machine:
    /// `route -n get default` → en0 (index 15), flags UP|GATEWAY|DONE|STATIC.
    fn reply(index: u16, flags: i32, pid: i32, seq: i32, errno: i32) -> Vec<u8> {
        let mut buf = route_request(seq, false);
        buf.resize(RT_MSGHDR_LEN + 16 * 3, 0);
        let len = u16::try_from(buf.len()).unwrap();
        buf[0..2].copy_from_slice(&len.to_ne_bytes());
        buf[4..6].copy_from_slice(&index.to_ne_bytes());
        buf[8..12].copy_from_slice(&flags.to_ne_bytes());
        buf[12..16].copy_from_slice(&0x7_i32.to_ne_bytes());
        buf[16..20].copy_from_slice(&pid.to_ne_bytes());
        buf[24..28].copy_from_slice(&errno.to_ne_bytes());
        buf
    }

    #[test]
    fn d30_route_reply_yields_interface_index() {
        let buf = reply(15, 0x843, 4242, 9, 0);
        assert_eq!(parse_route_reply(&buf, 9, 4242), Some(15));
    }

    #[test]
    fn route_reply_rejects_foreign_error_and_truncated() {
        let good = reply(15, 0x843, 4242, 9, 0);
        assert_eq!(parse_route_reply(&good, 10, 4242), None, "other seq");
        assert_eq!(parse_route_reply(&good, 9, 1), None, "other pid");
        assert_eq!(parse_route_reply(&good[..RT_MSGHDR_LEN - 1], 9, 4242), None);
        assert_eq!(parse_route_reply(&good[..RT_MSGHDR_LEN + 4], 9, 4242), None);
        assert_eq!(
            parse_route_reply(&reply(15, 0x843, 4242, 9, 3), 9, 4242),
            None
        );
        assert_eq!(
            parse_route_reply(&reply(15, 0x842, 4242, 9, 0), 9, 4242),
            None
        );
        assert_eq!(
            parse_route_reply(&reply(0, 0x843, 4242, 9, 0), 9, 4242),
            None
        );
        let mut other_type = good.clone();
        other_type[3] = 1;
        assert_eq!(parse_route_reply(&other_type, 9, 4242), None);
        let mut other_ver = good;
        other_ver[2] = 4;
        assert_eq!(parse_route_reply(&other_ver, 9, 4242), None);
    }

    #[test]
    fn route_reply_parser_survives_garbage_sweep() {
        let mut seed = 0x1234_5678_u32;
        for len in 0..400 {
            let buf: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed.to_le_bytes()[0]
                })
                .collect();
            let _ = parse_route_reply(&buf, 1, 1);
            let _ = parse_ifmibdata(1, &buf);
        }
    }

    #[test]
    fn first_sample_of_an_interface_is_zero_rate() {
        let mut st = RateState::default();
        let r = st.advance(&[row(1, "en0", true, 10_000_000_000, 5)], 1.0);
        assert_eq!(r, vec![(0, 0)]);
    }

    #[test]
    fn missing_sample_does_not_reset_previous() {
        let mut st = RateState::default();
        st.advance(&[row(1, "en0", true, 1000, 0)], 0.0);
        st.advance(&[], 1.0);
        let r = st.advance(&[row(1, "en0", true, 2000, 0)], 1.0);
        assert_eq!(r[0].0, 8000);
    }

    #[test]
    fn index_reused_by_new_name_starts_at_zero() {
        let mut st = RateState::default();
        st.advance(&[row(20, "utun3", true, 5, 5)], 0.0);
        let r = st.advance(&[row(20, "en7", true, 9_000_000, 9)], 1.0);
        assert_eq!(r[0], (0, 0));
    }

    #[test]
    fn d30_primary_from_route_wins_over_busiest() {
        let rows = [
            row(15, "en0", true, 100, 100),
            row(16, "en1", true, 9_000_000_000, 0),
            row(30, "utun4", true, 50, 50),
        ];
        let rates = [(10, 20), (30, 40), (50, 60)];
        let snap = build_snapshot(&rows, &rates, Some(30));
        assert_eq!(snap.primary.as_deref(), Some("utun4"));
        assert_eq!(snap.iface, "utun4");
        assert_eq!((snap.rx_bps, snap.tx_bps), (50, 60));
        let names: Vec<_> = snap.ifaces.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["en0", "en1", "utun4"]);
        assert_eq!(snap.ifaces.iter().filter(|i| i.primary).count(), 1);
    }

    #[test]
    fn no_default_route_falls_back_to_busiest_physical() {
        let rows = [
            row(15, "en0", true, 100, 100),
            row(16, "en1", true, 9_000_000_000, 0),
            row(1, "lo0", true, u64::MAX / 2, 0),
        ];
        let snap = build_snapshot(&rows, &[(1, 1), (2, 2), (3, 3)], None);
        assert_eq!(snap.primary, None);
        assert_eq!(snap.iface, "en1");
        assert!(snap.ifaces.iter().all(|i| i.name != "lo0"));
    }

    #[test]
    fn down_and_virtual_interfaces_are_hidden() {
        let rows = [
            row(2, "en2", false, 1, 1),
            row(8, "en8", true, 0, 0),
            row(3, "awdl0", true, 1, 1),
            row(4, "llw0", true, 1, 1),
            row(5, "bridge0", true, 1, 1),
            row(6, "anpi0", true, 1, 1),
            row(7, "ap1", true, 1, 1),
        ];
        let snap = build_snapshot(&rows, &[(0, 0); 7], None);
        assert!(snap.ifaces.is_empty());
        assert_eq!(snap.iface, "—");
        assert_eq!((snap.rx_bps, snap.tx_bps), (0, 0));
    }

    #[test]
    fn idle_bridge_members_are_hidden_but_a_new_dock_nic_shows() {
        let mut dock = row(40, "en7", true, 0, 0);
        dock.baud = 2_500_000_000;
        let rows = [
            row(7, "en3", true, 0, 0),
            row(8, "en4", true, 0, 0),
            dock,
            row(15, "en0", true, 31_786_334_744, 5_734_375_603),
        ];
        let snap = build_snapshot(&rows, &[(0, 0); 4], Some(15));
        let names: Vec<_> = snap.ifaces.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["en7", "en0"]);
        assert_eq!(snap.ifaces[0].link_bps, Some(2_500_000_000));
    }

    #[test]
    fn empty_rows_give_absent_snapshot() {
        let snap = build_snapshot(&[], &[], Some(15));
        assert_eq!(snap.primary, None);
        assert!(snap.ifaces.is_empty());
    }

    #[test]
    fn due_gate_table() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let ran_at = |ms: u64| {
            let mut pace = Pace::default();
            pace.ran(at(ms));
            pace
        };
        assert!(Pace::default().due(t0, ROUTE_EVERY));
        assert!(!ran_at(0).due(at(1999), ROUTE_EVERY));
        assert!(ran_at(0).due(at(2000), ROUTE_EVERY));
        assert!(ran_at(0).due(at(60_000), ROUTE_EVERY));
        assert!(!ran_at(5000).due(t0, ROUTE_EVERY), "backwards clock");
        assert!(ran_at(0).due(t0, Duration::ZERO));
    }

    #[test]
    fn d30_route_rechecked_every_two_seconds_at_any_interval() {
        let t0 = Instant::now();
        for tick_ms in [100_u64, 250, 500, 1000, 1500] {
            let mut pace = Pace::default();
            let mut checks = Vec::new();
            let mut t = 0;
            while t <= 10_000 {
                let now = t0 + Duration::from_millis(t);
                if pace.due(now, ROUTE_EVERY) {
                    pace.ran(now);
                    checks.push(t);
                }
                t += tick_ms;
            }
            assert_eq!(checks[0], 0);
            for w in checks.windows(2) {
                let gap = w[1] - w[0];
                // A VPN flip is seen within one route period plus one tick.
                assert!(
                    (2000..2000 + tick_ms).contains(&gap),
                    "tick {tick_ms}: {checks:?}"
                );
            }
        }
    }

    #[test]
    fn first_sample_is_only_a_baseline() {
        let mut col = NetCollector::new();
        assert!(
            col.sample(Instant::now()).expect("net").is_none(),
            "no made-up 0 b/s"
        );
    }

    #[test]
    fn first_sample_is_baseline_then_unchanged_counters_are_zero_rate() {
        let mut col = NetCollector::new();
        let t0 = Instant::now();
        let rows = [row(15, "en0", true, 10_000_000_000, 5)];
        assert!(col.advance(&rows, t0).is_none());
        let snap = col.advance(&rows, t0 + Duration::from_secs(1)).unwrap();
        assert!(snap.rx_bps == 0 && snap.tx_bps == 0);
        assert!(snap.ifaces.iter().all(|i| i.rx_bps == 0 && i.tx_bps == 0));
        let moved = [row(15, "en0", true, 10_000_000_125, 5)];
        let snap = col.advance(&moved, t0 + Duration::from_secs(2)).unwrap();
        assert_eq!((snap.iface.as_str(), snap.rx_bps), ("en0", 1000));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_ifmib_matches_struct_size_and_has_loopback() {
        let rows = macos::read_all();
        assert!(rows.iter().any(|r| r.name == "lo0"), "{rows:?}");
        let lo = rows.iter().find(|r| r.name == "lo0").unwrap();
        assert!(lo.up);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn d29_live_counters_match_netstat_untruncated() {
        let out = std::process::Command::new("netstat").arg("-ibn").output();
        let Ok(out) = out else { return };
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let ours = macos::read_all();
        let mut matched = 0;
        for line in text.lines().filter(|l| l.contains("<Link#")) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let name = cols[0].trim_end_matches('*');
            let n = cols.len();
            let ib: u64 = cols[n - 5].parse().unwrap();
            let ob: u64 = cols[n - 2].parse().unwrap();
            let Some(row) = ours.iter().find(|r| r.name == name) else {
                continue;
            };
            matched += 1;
            // netstat was read first: ours can only be equal or slightly ahead.
            assert!(
                row.rx_bytes >= ib && row.rx_bytes - ib < 1 << 30,
                "{name} rx {} vs {ib}",
                row.rx_bytes
            );
            assert!(
                row.tx_bytes >= ob && row.tx_bytes - ob < 1 << 30,
                "{name} tx {} vs {ob}",
                row.tx_bytes
            );
        }
        assert!(matched > 0, "no interface matched netstat");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_default_route_resolves_to_a_known_index() {
        let rows = macos::read_all();
        if let Some(idx) = macos::default_route_index(101) {
            assert!(
                rows.iter().any(|r| r.index == idx),
                "idx {idx} not in ifmib"
            );
        }
    }
}
