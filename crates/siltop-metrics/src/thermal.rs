use siltop_core::Thermal;

pub(crate) fn sample() -> Thermal {
    #[cfg(target_os = "macos")]
    {
        macos::sample()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Thermal::Nominal
    }
}

pub(crate) fn thermal_from_nsprocessinfo(state: i64) -> Thermal {
    match state {
        1 => Thermal::Fair,
        2 => Thermal::Serious,
        3 => Thermal::Critical,
        _ => Thermal::Nominal,
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::thermal_from_nsprocessinfo;
    use siltop_core::Thermal;
    use std::ffi::c_void;

    #[link(name = "objc")]
    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {
        fn objc_getClass(name: *const libc::c_char) -> *mut c_void;
        fn sel_registerName(name: *const libc::c_char) -> *mut c_void;
        fn objc_msgSend(obj: *mut c_void, sel: *mut c_void) -> *mut c_void;
    }

    pub(super) fn sample() -> Thermal {
        let info = unsafe {
            // SAFETY: Foundation class lookup; null means the class is unavailable.
            let cls = objc_getClass(c"NSProcessInfo".as_ptr());
            if cls.is_null() {
                return Thermal::Nominal;
            }
            let sel = sel_registerName(c"processInfo".as_ptr());
            if sel.is_null() {
                return Thermal::Nominal;
            }
            objc_msgSend(cls, sel)
        };
        if info.is_null() {
            return Thermal::Nominal;
        }
        let state = unsafe {
            // SAFETY: NSProcessInfo -thermalState returns NSInteger in the register result.
            let sel = sel_registerName(c"thermalState".as_ptr());
            if sel.is_null() {
                return Thermal::Nominal;
            }
            objc_msgSend(info, sel) as i64
        };
        thermal_from_nsprocessinfo(state)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn maps_nsprocessinfo_states() {
        assert_eq!(thermal_from_nsprocessinfo(0), Thermal::Nominal);
        assert_eq!(thermal_from_nsprocessinfo(1), Thermal::Fair);
        assert_eq!(thermal_from_nsprocessinfo(2), Thermal::Serious);
        assert_eq!(thermal_from_nsprocessinfo(3), Thermal::Critical);
        assert_eq!(thermal_from_nsprocessinfo(-1), Thermal::Nominal);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_thermal_is_known_variant() {
        let t = sample();
        assert!(matches!(
            t,
            Thermal::Nominal | Thermal::Fair | Thermal::Serious | Thermal::Critical
        ));
    }
}
