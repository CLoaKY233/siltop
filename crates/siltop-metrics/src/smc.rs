//! `AppleSMC` user client: key enumeration, a KEYINFO cache and typed decoding.

pub(crate) const fn fourcc(key: [u8; 4]) -> u32 {
    u32::from_be_bytes(key)
}

pub(crate) fn key_name(key: u32) -> String {
    String::from_utf8_lossy(&key.to_be_bytes())
        .trim()
        .to_string()
}

const FLT: u32 = fourcc(*b"flt ");
const FPE2: u32 = fourcc(*b"fpe2");
const SP78: u32 = fourcc(*b"sp78");
const IOFT: u32 = fourcc(*b"ioft");
const UI8: u32 = fourcc(*b"ui8 ");
const UI16: u32 = fourcc(*b"ui16");
const UI32: u32 = fourcc(*b"ui32");
const SI8: u32 = fourcc(*b"si8 ");
const SI16: u32 = fourcc(*b"si16");
const SI32: u32 = fourcc(*b"si32");

/// Byte order of the SMC's plain integer types. Apple Silicon firmware
/// stores them little-endian (`B0DC` reads 8579 mAh, not 33569); Intel
/// SMCs use big-endian. The `#KEY` count is big-endian on both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntOrder {
    Big,
    Little,
}

pub(crate) const NATIVE_INTS: IntOrder = if cfg!(target_arch = "aarch64") {
    IntOrder::Little
} else {
    IntOrder::Big
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyInfo {
    pub(crate) size: u32,
    pub(crate) data_type: u32,
}

impl KeyInfo {
    /// Types that carry a temperature in °C.
    pub(crate) fn is_temperature(self) -> bool {
        matches!(self.data_type, FLT | SP78 | IOFT)
    }
}

/// Decodes an SMC value by its declared type; the size decides only for
/// types this table does not know.
pub(crate) fn decode_numeric(data_type: u32, bytes: &[u8], ints: IntOrder) -> Option<f64> {
    let int = |raw: &[u8], signed: bool| -> Option<f64> {
        if raw.is_empty() || raw.len() > 8 {
            return None;
        }
        let push = |acc: u64, b: &u8| (acc << 8) | u64::from(*b);
        let unsigned = match ints {
            IntOrder::Big => raw.iter().fold(0, push),
            IntOrder::Little => raw.iter().rev().fold(0, push),
        };
        let shift = 64 - 8 * raw.len() as u32;
        Some(if signed {
            ((unsigned << shift) as i64 >> shift) as f64
        } else {
            unsigned as f64
        })
    };
    match data_type {
        FLT => Some(f64::from(f32::from_le_bytes(
            bytes.get(..4)?.try_into().ok()?,
        ))),
        FPE2 => Some(f64::from(u16::from_be_bytes(bytes.get(..2)?.try_into().ok()?)) / 4.0),
        SP78 => Some(f64::from(i16::from_be_bytes(bytes.get(..2)?.try_into().ok()?)) / 256.0),
        IOFT => Some(u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?) as f64 / 65536.0),
        UI8 => int(bytes.get(..1)?, false),
        UI16 => int(bytes.get(..2)?, false),
        UI32 => int(bytes.get(..4)?, false),
        SI8 => int(bytes.get(..1)?, true),
        SI16 => int(bytes.get(..2)?, true),
        SI32 => int(bytes.get(..4)?, true),
        _ => match bytes.len() {
            1 | 2 | 4 => int(bytes, false),
            _ => None,
        },
    }
}

/// A fan speed in RPM, or `None` for out-of-range values.
pub(crate) fn decode_rpm(info: KeyInfo, bytes: &[u8]) -> Option<u16> {
    let rpm = decode_numeric(info.data_type, bytes, NATIVE_INTS)?;
    (rpm.is_finite() && (0.0..=20_000.0).contains(&rpm)).then(|| rpm.round() as u16)
}

/// A temperature in °C from a temperature-typed key. Range checks belong to
/// the plausibility filter.
pub(crate) fn decode_temp(info: KeyInfo, bytes: &[u8]) -> Option<f32> {
    if !info.is_temperature() {
        return None;
    }
    let t = decode_numeric(info.data_type, bytes, NATIVE_INTS)?;
    t.is_finite().then_some(t as f32)
}

/// `kSMCKeyNotFound`: the SMC answered, and the key does not exist.
#[cfg(test)]
const KEY_NOT_FOUND: u8 = 0x84;
/// `kSMCCommCollision`, `kSMCSpuriousData` and `kSMCTimeoutError`: the SMC
/// was busy (often with another poller), which says nothing about the key.
const BUSY_RESULTS: [u8; 3] = [0x80, 0x81, 0xB7];

/// A call that failed without an answer about the key: the `IOKit` call
/// itself failed (`None`) or the SMC reported itself busy.
pub(crate) fn is_transient(result: Option<u8>) -> bool {
    result.is_none_or(|r| BUSY_RESULTS.contains(&r))
}

/// What a `KEYINFO` call established about a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// The key exists with this type and size.
    Found(KeyInfo),
    /// The SMC says there is no such (usable) key: safe to remember.
    Missing,
    /// See [`is_transient`]: the key is asked again next time.
    Transient,
}

/// Classifies one `KEYINFO` reply: `reply` is `None` when the `IOKit` call
/// failed, else the SMC's `(result, data_size, data_type)`.
pub(crate) fn keyinfo_lookup(reply: Option<(u8, u32, u32)>) -> Lookup {
    match reply {
        _ if is_transient(reply.map(|(result, _, _)| result)) => Lookup::Transient,
        Some((0, size, data_type)) if (1..=32).contains(&size) => {
            Lookup::Found(KeyInfo { size, data_type })
        }
        _ => Lookup::Missing,
    }
}

#[cfg(target_os = "macos")]
pub(crate) use macos::Client;

#[cfg(target_os = "macos")]
mod macos {
    use std::collections::HashMap;

    use super::{KeyInfo, Lookup, fourcc, is_transient, keyinfo_lookup};
    use crate::iokit::{self, IoConnect, IoObject, PlainData};

    const KERNEL_INDEX_SMC: u32 = 2;
    const CMD_READ_BYTES: u8 = 5;
    const CMD_READ_INDEX: u8 = 8;
    const CMD_READ_KEYINFO: u8 = 9;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(crate) struct SmcKeyData {
        key: u32,
        vers: [u8; 6],
        _vers_pad: [u8; 2],
        p_limit: [u8; 16],
        data_size: u32,
        data_type: u32,
        data_attributes: u8,
        _info_pad: [u8; 3],
        result: u8,
        status: u8,
        data8: u8,
        _cmd_pad: u8,
        data32: u32,
        bytes: [u8; 32],
    }

    // SAFETY: repr(C) integers and byte arrays only, so every bit pattern the
    // kernel writes is a valid SmcKeyData.
    unsafe impl PlainData for SmcKeyData {}

    pub(crate) struct Client {
        conn: IoConnect,
        _service: IoObject,
        infos: HashMap<u32, Option<KeyInfo>>,
        /// A call failed for a reason other than a missing key since open.
        transient: bool,
    }

    impl Client {
        pub(crate) fn open() -> Option<Self> {
            let mut fallback = None;
            for service in iokit::services(c"AppleSMC") {
                let Some(conn) = IoConnect::open(&service) else {
                    continue;
                };
                let keys_endpoint = service.name().as_deref() == Some("AppleSMCKeysEndpoint");
                let client = Self {
                    conn,
                    _service: service,
                    infos: HashMap::new(),
                    transient: false,
                };
                if keys_endpoint {
                    return Some(client);
                }
                if fallback.is_none() {
                    fallback = Some(client);
                }
            }
            fallback
        }

        /// Every key the SMC publishes, in index order. One call per key
        /// (3,626 on an M5 Pro), so it runs once, off the cheap tick.
        pub(crate) fn all_keys(&mut self) -> Vec<u32> {
            let count = self
                .read(fourcc(*b"#KEY"))
                .and_then(|(_, bytes)| Some(u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?)))
                .unwrap_or(0);
            let mut keys = Vec::with_capacity(count as usize);
            for index in 0..count {
                let input = SmcKeyData {
                    data8: CMD_READ_INDEX,
                    data32: index,
                    ..SmcKeyData::default()
                };
                let out = self.conn.call(KERNEL_INDEX_SMC, &input);
                match out {
                    Some(out) if out.result == 0 && out.key != 0 => keys.push(out.key),
                    _ => self.transient |= is_transient(out.map(|o| o.result)),
                }
            }
            self.transient |= count == 0;
            keys
        }

        /// Whether some call since [`open`](Self::open) failed in a way that
        /// says nothing about the key (timeout, contention), or `#KEY` gave
        /// no keys: what discovery found may be incomplete.
        pub(crate) fn had_transient_failure(&self) -> bool {
            self.transient
        }

        /// The key's type and size, asked once and cached. A call that
        /// failed without the SMC saying "no such key" is not cached, so a
        /// timeout under contention never hides the key for the session.
        pub(crate) fn info(&mut self, key: u32) -> Option<KeyInfo> {
            if let Some(known) = self.infos.get(&key) {
                return *known;
            }
            let input = SmcKeyData {
                key,
                data8: CMD_READ_KEYINFO,
                ..SmcKeyData::default()
            };
            let reply = self
                .conn
                .call(KERNEL_INDEX_SMC, &input)
                .map(|out| (out.result, out.data_size, out.data_type));
            match keyinfo_lookup(reply) {
                Lookup::Found(info) => {
                    self.infos.insert(key, Some(info));
                    Some(info)
                }
                Lookup::Missing => {
                    self.infos.insert(key, None);
                    None
                }
                Lookup::Transient => {
                    self.transient = true;
                    None
                }
            }
        }

        /// The key's current bytes, trimmed to its size.
        pub(crate) fn read(&mut self, key: u32) -> Option<(KeyInfo, Vec<u8>)> {
            let info = self.info(key)?;
            let input = SmcKeyData {
                key,
                data8: CMD_READ_BYTES,
                data_size: info.size,
                data_type: info.data_type,
                ..SmcKeyData::default()
            };
            let out = self.call(&input)?;
            let bytes = out.bytes.get(..info.size as usize)?.to_vec();
            Some((info, bytes))
        }

        fn call(&self, input: &SmcKeyData) -> Option<SmcKeyData> {
            self.conn
                .call(KERNEL_INDEX_SMC, input)
                .filter(|out| out.result == 0)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn info(ty: [u8; 4], size: u32) -> KeyInfo {
        KeyInfo {
            size,
            data_type: fourcc(ty),
        }
    }

    fn decode(ty: [u8; 4], bytes: &[u8]) -> Option<f64> {
        decode_numeric(fourcc(ty), bytes, IntOrder::Big)
    }

    #[test]
    fn a_keyinfo_timeout_is_transient_not_a_missing_key() {
        let flt = fourcc(*b"flt ");
        assert_eq!(
            keyinfo_lookup(Some((0, 4, flt))),
            Lookup::Found(info(*b"flt ", 4))
        );
        assert_eq!(keyinfo_lookup(Some((KEY_NOT_FOUND, 0, 0))), Lookup::Missing);
        assert_eq!(
            keyinfo_lookup(Some((0, 0, 0))),
            Lookup::Missing,
            "unusable size"
        );
        assert_eq!(keyinfo_lookup(Some((0, 33, flt))), Lookup::Missing);
        assert_eq!(keyinfo_lookup(None), Lookup::Transient, "IOKit call failed");
        assert_eq!(
            keyinfo_lookup(Some((0xB7, 0, 0))),
            Lookup::Transient,
            "SMC busy"
        );
        assert_eq!(keyinfo_lookup(Some((0x80, 0, 0))), Lookup::Transient);
        assert_eq!(
            keyinfo_lookup(Some((0x85, 0, 0))),
            Lookup::Missing,
            "not readable"
        );
    }

    #[test]
    fn smc_struct_is_80_bytes() {
        #[cfg(target_os = "macos")]
        assert_eq!(std::mem::size_of::<macos::SmcKeyData>(), 80);
    }

    #[test]
    fn fourcc_is_be() {
        assert_eq!(fourcc(*b"F0Ac"), 0x4630_4163);
        assert_eq!(fourcc(*b"flt "), 0x666c_7420);
        assert_eq!(key_name(fourcc(*b"Tp0O")), "Tp0O");
        assert_eq!(key_name(fourcc(*b"ui8 ")), "ui8");
    }

    #[test]
    fn floats_and_fixed_point() {
        assert_eq!(decode(*b"flt ", &42.5_f32.to_le_bytes()), Some(42.5));
        assert_eq!(decode(*b"fpe2", &[0x1c, 0x20]), Some(1800.0));
        assert_eq!(decode(*b"sp78", &[0x2a, 0x80]), Some(42.5));
        assert_eq!(decode(*b"sp78", &[0xff, 0x00]), Some(-1.0));
        let ioft = (32.2_f64 * 65536.0) as u64;
        let got = decode(*b"ioft", &ioft.to_le_bytes()).unwrap();
        assert!((got - 32.2).abs() < 1e-4, "{got}");
    }

    #[test]
    fn sp78_is_not_read_as_ui16() {
        // Regression: size-2 keys used to hit the ui16 branch first, so
        // 0x2A80 decoded as 10880 and was thrown away.
        let t = decode_temp(info(*b"sp78", 2), &[0x2a, 0x80]).unwrap();
        assert!((t - 42.5).abs() < f32::EPSILON);
    }

    #[test]
    fn integers_honour_sign_and_byte_order() {
        assert_eq!(decode(*b"ui8 ", &[200]), Some(200.0));
        assert_eq!(decode(*b"si8 ", &[0xff]), Some(-1.0));
        assert_eq!(decode(*b"ui16", &[0x0e, 0x2a]), Some(3626.0));
        assert_eq!(decode(*b"si16", &[0xff, 0xfe]), Some(-2.0));
        assert_eq!(decode(*b"ui32", &[0, 0, 0x0e, 0x2a]), Some(3626.0));
        assert_eq!(decode(*b"si32", &[0xff, 0xff, 0xff, 0xfd]), Some(-3.0));
        let le = |ty: [u8; 4], b: &[u8]| decode_numeric(fourcc(ty), b, IntOrder::Little);
        assert_eq!(le(*b"ui16", &[0x83, 0x21]), Some(8579.0), "B0DC design mAh");
        assert_eq!(le(*b"si32", &[0x29, 0, 0, 0]), Some(41.0));
        assert_eq!(le(*b"si16", &[0xfe, 0xff]), Some(-2.0));
    }

    #[test]
    fn unknown_types_fall_back_on_size() {
        assert_eq!(decode(*b"xyz ", &[7]), Some(7.0));
        assert_eq!(decode(*b"xyz ", &[1, 0]), Some(256.0));
        assert_eq!(decode(*b"xyz ", &[1, 2, 3]), None);
        assert_eq!(decode(*b"flt ", &[1, 2]), None, "short float");
    }

    #[test]
    fn rpm_and_temperature_gates() {
        assert_eq!(
            decode_rpm(info(*b"flt ", 4), &1850.0_f32.to_le_bytes()),
            Some(1850)
        );
        assert_eq!(
            decode_rpm(info(*b"flt ", 4), &99_000.0_f32.to_le_bytes()),
            None
        );
        assert_eq!(decode_rpm(info(*b"flt ", 4), &[]), None);
        assert!(decode_temp(info(*b"si32", 4), &[0x29, 0, 0, 0]).is_none());
        assert!(decode_temp(info(*b"ui8 ", 1), &[40]).is_none());
        assert!(info(*b"ioft", 8).is_temperature());
    }
}
