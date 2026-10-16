//! The `IOKit` / `CoreFoundation` FFI boundary. macOS only.
//!
//! Every `IOKit` and `CoreFoundation` symbol the crate uses is declared in the one
//! `extern` block below. Collectors never call them directly: they hold
//! owned handles in RAII wrappers ([`IoObject`], [`IoConnect`], [`CfOwned`])
//! and borrow values through [`CfRef`], whose lifetime is tied to the owner.
//! Dictionary keys are [`CfKey`]s created once and kept by the collector.

use std::ffi::{CStr, c_char, c_void};
use std::marker::PhantomData;
use std::mem::size_of;
use std::ptr::{self, NonNull};

type CfTypeRef = *const c_void;
type CfAllocatorRef = *const c_void;
type KernReturn = libc::kern_return_t;

const MAIN_PORT_DEFAULT: u32 = 0;
const UTF8: u32 = 0x0800_0100;
const NUMBER_SINT32: i32 = 3;
const NUMBER_SINT64: i32 = 4;
const NUMBER_FLOAT64: i32 = 6;
const HID_EVENT_TEMPERATURE: i64 = 15;
const HID_FIELD_TEMPERATURE: i32 = 15 << 16;

#[repr(C)]
struct CfCallbacks {
    _opaque: [u8; 0],
}

#[allow(clippy::duplicated_attributes)]
#[link(name = "IOKit", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CfTypeRef;
    fn IOServiceGetMatchingServices(port: u32, matching: CfTypeRef, iter: *mut u32) -> KernReturn;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> KernReturn;
    fn IORegistryEntryGetName(entry: u32, name: *mut c_char) -> KernReturn;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, entry_id: *mut u64) -> KernReturn;
    fn IORegistryEntryGetChildIterator(
        entry: u32,
        plane: *const c_char,
        iter: *mut u32,
    ) -> KernReturn;
    fn IORegistryEntryGetParentEntry(
        entry: u32,
        plane: *const c_char,
        parent: *mut u32,
    ) -> KernReturn;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CfTypeRef,
        allocator: CfAllocatorRef,
        options: u32,
    ) -> CfTypeRef;
    fn IOServiceOpen(service: u32, owning_task: u32, kind: u32, connect: *mut u32) -> KernReturn;
    fn IOServiceClose(connect: u32) -> KernReturn;
    fn IOConnectCallStructMethod(
        connection: u32,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> KernReturn;

    fn IOHIDEventSystemClientCreate(allocator: CfAllocatorRef) -> CfTypeRef;
    fn IOHIDEventSystemClientSetMatching(client: CfTypeRef, matching: CfTypeRef);
    fn IOHIDEventSystemClientCopyServices(client: CfTypeRef) -> CfTypeRef;
    fn IOHIDServiceClientCopyProperty(service: CfTypeRef, key: CfTypeRef) -> CfTypeRef;
    fn IOHIDServiceClientCopyEvent(
        service: CfTypeRef,
        event_type: i64,
        matching: CfTypeRef,
        timeout: i64,
    ) -> CfTypeRef;
    fn IOHIDEventGetFloatValue(event: CfTypeRef, field: i32) -> f64;

    fn IOPSCopyPowerSourcesInfo() -> CfTypeRef;
    fn IOPSCopyPowerSourcesList(blob: CfTypeRef) -> CfTypeRef;
    fn IOPSGetPowerSourceDescription(blob: CfTypeRef, source: CfTypeRef) -> CfTypeRef;
    fn IOPSGetProvidingPowerSourceType(blob: CfTypeRef) -> CfTypeRef;
    fn IOPSGetTimeRemainingEstimate() -> f64;
    fn IOPMCopyAssertionsByProcess(by_pid: *mut CfTypeRef) -> KernReturn;

    fn CFRetain(cf: CfTypeRef) -> CfTypeRef;
    fn CFRelease(cf: CfTypeRef);
    fn CFGetTypeID(cf: CfTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(boolean: CfTypeRef) -> u8;
    fn CFDictionaryGetCount(dict: CfTypeRef) -> isize;
    fn CFDictionaryGetKeysAndValues(dict: CfTypeRef, keys: *mut CfTypeRef, values: *mut CfTypeRef);
    fn CFStringCreateWithCString(
        alloc: CfAllocatorRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CfTypeRef;
    fn CFStringGetCString(string: CfTypeRef, buffer: *mut c_char, size: isize, encoding: u32)
    -> u8;
    fn CFNumberCreate(alloc: CfAllocatorRef, kind: i32, value: *const c_void) -> CfTypeRef;
    fn CFNumberGetValue(number: CfTypeRef, kind: i32, value: *mut c_void) -> u8;
    fn CFDataGetBytePtr(data: CfTypeRef) -> *const u8;
    fn CFDataGetLength(data: CfTypeRef) -> isize;
    fn CFDictionaryGetValue(dict: CfTypeRef, key: CfTypeRef) -> CfTypeRef;
    fn CFDictionaryCreate(
        alloc: CfAllocatorRef,
        keys: *const CfTypeRef,
        values: *const CfTypeRef,
        count: isize,
        key_callbacks: *const CfCallbacks,
        value_callbacks: *const CfCallbacks,
    ) -> CfTypeRef;
    fn CFArrayGetCount(array: CfTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CfTypeRef, index: isize) -> CfTypeRef;
    fn CFDictionaryCreateMutableCopy(
        alloc: CfAllocatorRef,
        capacity: isize,
        dict: CfTypeRef,
    ) -> CfTypeRef;
    fn CFDictionarySetValue(dict: CfTypeRef, key: CfTypeRef, value: CfTypeRef);
    fn CFArrayCreateMutable(
        alloc: CfAllocatorRef,
        capacity: isize,
        callbacks: *const CfCallbacks,
    ) -> CfTypeRef;
    fn CFArrayAppendValue(array: CfTypeRef, value: CfTypeRef);

    static kCFTypeDictionaryKeyCallBacks: CfCallbacks;
    static kCFTypeDictionaryValueCallBacks: CfCallbacks;
    static kCFTypeArrayCallBacks: CfCallbacks;
}

/// An owned `io_object_t`; released on drop.
pub(crate) struct IoObject(u32);

impl IoObject {
    fn from_raw(raw: u32) -> Option<Self> {
        (raw != 0).then_some(Self(raw))
    }

    pub(crate) fn name(&self) -> Option<String> {
        let mut buf: [c_char; 128] = [0; 128];
        let kr = unsafe {
            // SAFETY: `self.0` is a live registry entry and `buf` is the
            // 128-byte io_name_t the call writes a C string into.
            IORegistryEntryGetName(self.0, buf.as_mut_ptr())
        };
        if kr != 0 {
            return None;
        }
        c_buffer_string(&buf)
    }

    /// Stable, system-unique id of this registry entry. Unlike the port it
    /// survives a rematch, so per-device state can be keyed by it.
    pub(crate) fn registry_id(&self) -> Option<u64> {
        let mut id = 0_u64;
        let kr = unsafe {
            // SAFETY: `self.0` is a live registry entry and `id` a local
            // out-parameter.
            IORegistryEntryGetRegistryEntryID(self.0, &raw mut id)
        };
        (kr == 0 && id != 0).then_some(id)
    }
}

impl Drop for IoObject {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: we own this reference; it came from an iterator or
            // matching call that returned it retained.
            IOObjectRelease(self.0);
        }
    }
}

/// Every registered service whose class is (or inherits from) `class`.
pub(crate) fn services(class: &CStr) -> Vec<IoObject> {
    let matching = unsafe {
        // SAFETY: `class` is a valid C string.
        IOServiceMatching(class.as_ptr())
    };
    if matching.is_null() {
        return Vec::new();
    }
    let mut iter = 0_u32;
    let kr = unsafe {
        // SAFETY: `matching` is a +1 dictionary that this call consumes;
        // `iter` is a local out-parameter.
        IOServiceGetMatchingServices(MAIN_PORT_DEFAULT, matching, &raw mut iter)
    };
    if kr != 0 {
        return Vec::new();
    }
    drain_iterator(iter)
}

/// Direct children of `entry` in the registry `plane` (e.g. `c"IOService"`).
pub(crate) fn children(entry: &IoObject, plane: &CStr) -> Vec<IoObject> {
    let mut iter = 0_u32;
    let kr = unsafe {
        // SAFETY: `entry` is live, `plane` is a valid C string and `iter` is a
        // local out-parameter.
        IORegistryEntryGetChildIterator(entry.0, plane.as_ptr(), &raw mut iter)
    };
    if kr != 0 {
        return Vec::new();
    }
    drain_iterator(iter)
}

/// The first parent of `entry` in the registry `plane` (its provider in
/// `c"IOService"`).
pub(crate) fn parent(entry: &IoObject, plane: &CStr) -> Option<IoObject> {
    let mut parent = 0_u32;
    let kr = unsafe {
        // SAFETY: `entry` is live, `plane` is a valid C string and `parent`
        // is a local out-parameter that receives a +1 reference on success.
        IORegistryEntryGetParentEntry(entry.0, plane.as_ptr(), &raw mut parent)
    };
    if kr != 0 {
        return None;
    }
    IoObject::from_raw(parent)
}

fn drain_iterator(iter: u32) -> Vec<IoObject> {
    let Some(iter) = IoObject::from_raw(iter) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    loop {
        let next = unsafe {
            // SAFETY: `iter` is a live iterator we own until it drops below.
            IOIteratorNext(iter.0)
        };
        match IoObject::from_raw(next) {
            Some(object) => out.push(object),
            None => break,
        }
    }
    out
}

/// One registry property of `entry`, copied without the rest of its
/// property table.
pub(crate) fn property(entry: &IoObject, key: &CfKey) -> Option<CfOwned> {
    let value = unsafe {
        // SAFETY: `entry` and the key string are live; the call returns a +1
        // reference or null.
        IORegistryEntryCreateCFProperty(entry.0, key.0.as_ptr(), ptr::null(), 0)
    };
    // SAFETY: Create-rule result, owned from here on.
    unsafe { CfOwned::from_create(value) }
}

/// An open user-client connection; closed on drop.
pub(crate) struct IoConnect(u32);

/// Plain data a kernel call may overwrite with arbitrary bytes.
///
/// # Safety
/// Implementors must be `repr(C)` and valid for every bit pattern.
pub(crate) unsafe trait PlainData: Copy {}

impl IoConnect {
    pub(crate) fn open(service: &IoObject) -> Option<Self> {
        let mut connect = 0_u32;
        #[allow(deprecated)]
        let task = unsafe {
            // SAFETY: mach_task_self reads a per-task constant.
            libc::mach_task_self()
        };
        let kr = unsafe {
            // SAFETY: `service` is live and `connect` is a local out-parameter.
            IOServiceOpen(service.0, task, 0, &raw mut connect)
        };
        (kr == 0 && connect != 0).then_some(Self(connect))
    }

    /// `IOConnectCallStructMethod` with the same struct type in and out.
    pub(crate) fn call<T: PlainData>(&self, selector: u32, input: &T) -> Option<T> {
        let mut output = *input;
        let mut out_size = size_of::<T>();
        let kr = unsafe {
            // SAFETY: both pointers cover `size_of::<T>()` bytes, and
            // `PlainData` guarantees any bytes the kernel writes form a valid T.
            IOConnectCallStructMethod(
                self.0,
                selector,
                ptr::from_ref(input).cast(),
                size_of::<T>(),
                ptr::from_mut(&mut output).cast(),
                &raw mut out_size,
            )
        };
        (kr == 0).then_some(output)
    }
}

impl Drop for IoConnect {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: we opened this connection and close it exactly once.
            IOServiceClose(self.0);
        }
    }
}

/// An owned (+1) `CoreFoundation` reference; released on drop.
pub(crate) struct CfOwned(NonNull<c_void>);

impl CfOwned {
    /// Takes ownership of a Create- or Copy-rule reference.
    ///
    /// # Safety
    /// `value` must be null or a CF object the caller owns one reference to.
    pub(crate) unsafe fn from_create(value: CfTypeRef) -> Option<Self> {
        NonNull::new(value.cast_mut()).map(Self)
    }

    /// Retains a borrowed value so it outlives its container.
    pub(crate) fn retain(value: CfRef<'_>) -> Self {
        unsafe {
            // SAFETY: `value` is a live CF object; CFRetain returns it +1.
            CFRetain(value.as_ptr());
        }
        Self(value.ptr)
    }

    pub(crate) fn as_ptr(&self) -> CfTypeRef {
        self.0.as_ptr().cast_const()
    }

    pub(crate) fn borrow(&self) -> CfRef<'_> {
        CfRef {
            ptr: self.0,
            _owner: PhantomData,
        }
    }
}

impl Drop for CfOwned {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: we own exactly one reference to this object.
            CFRelease(self.as_ptr());
        }
    }
}

/// A borrowed (Get-rule) `CoreFoundation` value, valid while `'a` lives.
#[derive(Clone, Copy)]
pub(crate) struct CfRef<'a> {
    ptr: NonNull<c_void>,
    _owner: PhantomData<&'a CfOwned>,
}

impl CfRef<'_> {
    pub(crate) fn from_get(value: CfTypeRef) -> Option<Self> {
        NonNull::new(value.cast_mut()).map(|ptr| Self {
            ptr,
            _owner: PhantomData,
        })
    }

    pub(crate) fn as_ptr(self) -> CfTypeRef {
        self.ptr.as_ptr().cast_const()
    }

    fn type_id(self) -> usize {
        unsafe {
            // SAFETY: `self` is a live CF object for `'a`.
            CFGetTypeID(self.as_ptr())
        }
    }
}

/// A `CFString` dictionary or property key, created once and reused.
pub(crate) struct CfKey(CfOwned);

impl CfKey {
    pub(crate) fn new(text: &CStr) -> Option<Self> {
        let value = unsafe {
            // SAFETY: `text` is a valid C string; default allocator.
            CFStringCreateWithCString(ptr::null(), text.as_ptr(), UTF8)
        };
        // SAFETY: Create-rule result.
        unsafe { CfOwned::from_create(value) }.map(Self)
    }

    pub(crate) fn as_ptr(&self) -> CfTypeRef {
        self.0.as_ptr()
    }
}

fn is_kind(value: CfRef<'_>, kind: unsafe extern "C" fn() -> usize) -> bool {
    let expected = unsafe {
        // SAFETY: the CF*GetTypeID functions are constant queries.
        kind()
    };
    value.type_id() == expected
}

pub(crate) fn cf_dict_get<'a>(dict: CfRef<'a>, key: &CfKey) -> Option<CfRef<'a>> {
    if !is_kind(dict, CFDictionaryGetTypeID) {
        return None;
    }
    let value = unsafe {
        // SAFETY: `dict` is a live CFDictionary and `key` a live CFString; the
        // result is borrowed from `dict`.
        CFDictionaryGetValue(dict.as_ptr(), key.0.as_ptr())
    };
    CfRef::from_get(value)
}

/// A `CFNumber` as f64 (integers convert exactly up to 2^53).
pub(crate) fn cf_number(value: CfRef<'_>) -> Option<f64> {
    if !is_kind(value, CFNumberGetTypeID) {
        return None;
    }
    let mut out = 0.0_f64;
    let exact = unsafe {
        // SAFETY: `value` is a CFNumber and `out` a local f64.
        CFNumberGetValue(value.as_ptr(), NUMBER_FLOAT64, (&raw mut out).cast())
    };
    if exact != 0 && out.is_finite() {
        return Some(out);
    }
    cf_i64(value).map(|n| n as f64)
}

/// A `CFNumber` as i64; float values are truncated.
pub(crate) fn cf_i64(value: CfRef<'_>) -> Option<i64> {
    if !is_kind(value, CFNumberGetTypeID) {
        return None;
    }
    let mut out = 0_i64;
    let exact = unsafe {
        // SAFETY: `value` is a CFNumber and `out` a local i64.
        CFNumberGetValue(value.as_ptr(), NUMBER_SINT64, (&raw mut out).cast())
    };
    if exact != 0 {
        return Some(out);
    }
    let mut float = 0.0_f64;
    let ok = unsafe {
        // SAFETY: as above, with a local f64.
        CFNumberGetValue(value.as_ptr(), NUMBER_FLOAT64, (&raw mut float).cast())
    };
    (ok != 0 && float.is_finite()).then_some(float as i64)
}

pub(crate) fn cf_data(value: CfRef<'_>) -> Option<&[u8]> {
    if !is_kind(value, CFDataGetTypeID) {
        return None;
    }
    let len = unsafe {
        // SAFETY: `value` is a live CFData.
        CFDataGetLength(value.as_ptr())
    };
    let bytes = unsafe {
        // SAFETY: as above; the pointer stays valid while the data lives.
        CFDataGetBytePtr(value.as_ptr())
    };
    let len = usize::try_from(len).ok()?;
    if bytes.is_null() || len == 0 {
        return None;
    }
    Some(unsafe {
        // SAFETY: CFData owns `len` readable bytes at `bytes` for `'a`.
        std::slice::from_raw_parts(bytes, len)
    })
}

/// A `CFString`, or a NUL-terminated `CFData` (registry properties use both).
pub(crate) fn cf_string(value: CfRef<'_>) -> Option<String> {
    let text = if is_kind(value, CFStringGetTypeID) {
        let mut buf: [c_char; 256] = [0; 256];
        let ok = unsafe {
            // SAFETY: `value` is a CFString; `buf` is writable for its length.
            CFStringGetCString(value.as_ptr(), buf.as_mut_ptr(), buf.len() as isize, UTF8)
        };
        if ok == 0 {
            return None;
        }
        c_buffer_string(&buf)?
    } else {
        let bytes = cf_data(value)?;
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    };
    (!text.is_empty()).then_some(text)
}

/// A `CFBoolean`; numbers read as `value != 0` (registry flags use both).
pub(crate) fn cf_bool(value: CfRef<'_>) -> Option<bool> {
    if is_kind(value, CFBooleanGetTypeID) {
        let flag = unsafe {
            // SAFETY: `value` is a live CFBoolean.
            CFBooleanGetValue(value.as_ptr())
        };
        return Some(flag != 0);
    }
    cf_i64(value).map(|n| n != 0)
}

/// Every `(key, value)` pair of a `CFDictionary`, borrowed from it.
pub(crate) fn cf_dict_entries(dict: CfRef<'_>) -> Vec<(CfRef<'_>, CfRef<'_>)> {
    if !is_kind(dict, CFDictionaryGetTypeID) {
        return Vec::new();
    }
    let count = unsafe {
        // SAFETY: `dict` is a live CFDictionary.
        CFDictionaryGetCount(dict.as_ptr())
    };
    let Ok(len) = usize::try_from(count) else {
        return Vec::new();
    };
    let mut keys: Vec<CfTypeRef> = vec![ptr::null(); len];
    let mut values: Vec<CfTypeRef> = vec![ptr::null(); len];
    unsafe {
        // SAFETY: both buffers hold `count` pointer slots, as the call
        // requires; the entries are borrowed from `dict`.
        CFDictionaryGetKeysAndValues(dict.as_ptr(), keys.as_mut_ptr(), values.as_mut_ptr());
    }
    keys.into_iter()
        .zip(values)
        .filter_map(|(k, v)| Some((CfRef::from_get(k)?, CfRef::from_get(v)?)))
        .collect()
}

fn c_buffer_string(buf: &[c_char]) -> Option<String> {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn cf_array_items(array: CfRef<'_>) -> Vec<CfRef<'_>> {
    if !is_kind(array, CFArrayGetTypeID) {
        return Vec::new();
    }
    let count = unsafe {
        // SAFETY: `array` is a live CFArray.
        CFArrayGetCount(array.as_ptr())
    };
    (0..count.max(0))
        .filter_map(|i| {
            let item = unsafe {
                // SAFETY: `i` is within 0..count; the item is borrowed from
                // the array.
                CFArrayGetValueAtIndex(array.as_ptr(), i)
            };
            CfRef::from_get(item)
        })
        .collect()
}

fn cf_int(value: i64) -> Option<CfOwned> {
    let number = match i32::try_from(value) {
        Ok(small) => unsafe {
            // SAFETY: `small` is a local i32; default allocator.
            CFNumberCreate(ptr::null(), NUMBER_SINT32, (&raw const small).cast())
        },
        Err(_) => unsafe {
            // SAFETY: `value` is a local i64; default allocator.
            CFNumberCreate(ptr::null(), NUMBER_SINT64, (&raw const value).cast())
        },
    };
    // SAFETY: Create-rule result.
    unsafe { CfOwned::from_create(number) }
}

/// A `{key: integer}` `CFDictionary`, e.g. a HID matching dictionary.
/// Values that fit use `SInt32`, as `IOKit` matching dictionaries do.
pub(crate) fn cf_dict_numbers(pairs: &[(&CStr, i64)]) -> Option<CfOwned> {
    let mut keys = Vec::with_capacity(pairs.len());
    let mut values = Vec::with_capacity(pairs.len());
    for (key, value) in pairs {
        keys.push(CfKey::new(key)?.0);
        values.push(cf_int(*value)?);
    }
    let key_ptrs: Vec<CfTypeRef> = keys.iter().map(CfOwned::as_ptr).collect();
    let value_ptrs: Vec<CfTypeRef> = values.iter().map(CfOwned::as_ptr).collect();
    let dict = unsafe {
        // SAFETY: both arrays hold `pairs.len()` live CF objects, which the
        // dictionary retains through the standard CFType callbacks.
        CFDictionaryCreate(
            ptr::null(),
            key_ptrs.as_ptr(),
            value_ptrs.as_ptr(),
            pairs.len() as isize,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    };
    // SAFETY: Create-rule result.
    unsafe { CfOwned::from_create(dict) }
}

/// A mutable copy of a `CFDictionary` (`IOReport` subscribes from one).
pub(crate) fn cf_mutable_copy(dict: CfRef<'_>) -> Option<CfOwned> {
    if !is_kind(dict, CFDictionaryGetTypeID) {
        return None;
    }
    let copy = unsafe {
        // SAFETY: `dict` is a live CFDictionary; capacity 0 means unbounded.
        CFDictionaryCreateMutableCopy(ptr::null(), 0, dict.as_ptr())
    };
    // SAFETY: Create-rule result.
    unsafe { CfOwned::from_create(copy) }
}

/// A new `CFArray` retaining `items`.
pub(crate) fn cf_array_of(items: &[CfRef<'_>]) -> Option<CfOwned> {
    let array = unsafe {
        // SAFETY: default allocator; the standard CFType callbacks retain.
        CFArrayCreateMutable(ptr::null(), 0, &raw const kCFTypeArrayCallBacks)
    };
    // SAFETY: Create-rule result.
    let array = unsafe { CfOwned::from_create(array) }?;
    for item in items {
        unsafe {
            // SAFETY: `array` is the mutable array created above and `item`
            // is a live CF object, which the array retains.
            CFArrayAppendValue(array.as_ptr(), item.as_ptr());
        }
    }
    Some(array)
}

/// Sets `key` in a dictionary made by [`cf_mutable_copy`].
pub(crate) fn cf_dict_set(dict: &CfOwned, key: &CfKey, value: CfRef<'_>) {
    if !is_kind(dict.borrow(), CFDictionaryGetTypeID) {
        return;
    }
    unsafe {
        // SAFETY: callers pass a mutable dictionary from `cf_mutable_copy`;
        // key and value are live and retained by the dictionary.
        CFDictionarySetValue(dict.as_ptr(), key.0.as_ptr(), value.as_ptr());
    }
}

/// An `IOHIDEventSystemClient` limited to services matching `matching`.
pub(crate) fn hid_client(matching: &CfOwned) -> Option<CfOwned> {
    let client = unsafe {
        // SAFETY: default allocator; returns a +1 client or null.
        IOHIDEventSystemClientCreate(ptr::null())
    };
    // SAFETY: Create-rule result.
    let client = unsafe { CfOwned::from_create(client) }?;
    unsafe {
        // SAFETY: both objects are live; the client copies the matching.
        IOHIDEventSystemClientSetMatching(client.as_ptr(), matching.as_ptr());
    }
    Some(client)
}

/// The client's current services as a `CFArray`.
pub(crate) fn hid_services(client: &CfOwned) -> Option<CfOwned> {
    let services = unsafe {
        // SAFETY: `client` is a live event-system client; Copy rule.
        IOHIDEventSystemClientCopyServices(client.as_ptr())
    };
    // SAFETY: Copy-rule result.
    unsafe { CfOwned::from_create(services) }
}

pub(crate) fn hid_property(service: CfRef<'_>, key: &CfKey) -> Option<String> {
    let value = unsafe {
        // SAFETY: `service` is a live HID service client; Copy rule.
        IOHIDServiceClientCopyProperty(service.as_ptr(), key.0.as_ptr())
    };
    // SAFETY: Copy-rule result.
    let value = unsafe { CfOwned::from_create(value) }?;
    cf_string(value.borrow())
}

/// The service's current temperature event, in °C as reported.
pub(crate) fn hid_temperature(service: CfRef<'_>) -> Option<f64> {
    let event = unsafe {
        // SAFETY: `service` is a live HID service client; Copy rule.
        IOHIDServiceClientCopyEvent(service.as_ptr(), HID_EVENT_TEMPERATURE, ptr::null(), 0)
    };
    // SAFETY: Copy-rule result.
    let event = unsafe { CfOwned::from_create(event) }?;
    let value = unsafe {
        // SAFETY: `event` is a live temperature event.
        IOHIDEventGetFloatValue(event.as_ptr(), HID_FIELD_TEMPERATURE)
    };
    value.is_finite().then_some(value)
}

/// The power-source snapshot blob (`IOPSCopyPowerSourcesInfo`).
pub(crate) fn power_sources_info() -> Option<CfOwned> {
    let blob = unsafe {
        // SAFETY: no arguments; Copy rule, +1 or null.
        IOPSCopyPowerSourcesInfo()
    };
    // SAFETY: Copy-rule result.
    unsafe { CfOwned::from_create(blob) }
}

/// The description dictionaries of every power source in `blob`, each
/// retained so it outlives the list that held it.
pub(crate) fn power_source_descriptions(blob: &CfOwned) -> Vec<CfOwned> {
    let list = unsafe {
        // SAFETY: `blob` is a live power-source blob; Copy rule.
        IOPSCopyPowerSourcesList(blob.as_ptr())
    };
    // SAFETY: Copy-rule result.
    let Some(list) = (unsafe { CfOwned::from_create(list) }) else {
        return Vec::new();
    };
    cf_array_items(list.borrow())
        .into_iter()
        .filter_map(|source| {
            let desc = unsafe {
                // SAFETY: `blob` and `source` are live; Get rule, borrowed
                // from `blob`.
                IOPSGetPowerSourceDescription(blob.as_ptr(), source.as_ptr())
            };
            CfRef::from_get(desc).map(CfOwned::retain)
        })
        .collect()
}

/// `"AC Power"`, `"Battery Power"` or `"UPS Power"`.
pub(crate) fn providing_power_source(blob: &CfOwned) -> Option<String> {
    let kind = unsafe {
        // SAFETY: `blob` is a live power-source blob; Get rule.
        IOPSGetProvidingPowerSourceType(blob.as_ptr())
    };
    cf_string(CfRef::from_get(kind)?)
}

/// Seconds of battery left: -1 unknown, -2 unlimited (on external power).
pub(crate) fn time_remaining_estimate() -> f64 {
    unsafe {
        // SAFETY: a plain query with no arguments.
        IOPSGetTimeRemainingEstimate()
    }
}

/// `{pid: [assertion dict]}` for every process holding a power assertion.
pub(crate) fn assertions_by_process() -> Option<CfOwned> {
    let mut out: CfTypeRef = ptr::null();
    let kr = unsafe {
        // SAFETY: `out` is a local out-parameter that receives a +1 dict.
        IOPMCopyAssertionsByProcess(&raw mut out)
    };
    // SAFETY: Copy-rule result (null on failure).
    let dict = unsafe { CfOwned::from_create(out) };
    if kr != 0 {
        return None;
    }
    dict
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_numbers_round_trip() {
        let key = CfKey::new(c"answer").expect("key");
        let dict = cf_dict_numbers(&[(c"answer", 42), (c"other", -7), (c"big", 4_012_113_920)])
            .expect("dict");
        let value = cf_dict_get(dict.borrow(), &key).expect("value");
        assert_eq!(cf_i64(value), Some(42));
        assert_eq!(cf_number(value), Some(42.0));
        let big = CfKey::new(c"big").expect("key");
        let big = cf_dict_get(dict.borrow(), &big).expect("big");
        assert_eq!(cf_i64(big), Some(4_012_113_920));
        assert!(cf_data(value).is_none());
        assert!(cf_string(value).is_none());
        let missing = CfKey::new(c"missing").expect("key");
        assert!(cf_dict_get(dict.borrow(), &missing).is_none());
        assert!(cf_dict_get(value, &key).is_none(), "a number is not a dict");
    }

    #[test]
    fn retained_values_outlive_their_container() {
        let key = CfKey::new(c"other").expect("key");
        let kept = {
            let dict = cf_dict_numbers(&[(c"other", -7)]).expect("dict");
            CfOwned::retain(cf_dict_get(dict.borrow(), &key).expect("value"))
        };
        assert_eq!(cf_i64(kept.borrow()), Some(-7));
    }

    #[test]
    fn strings_read_back() {
        let key = CfKey::new(c"PerformanceStatistics").expect("key");
        assert_eq!(
            cf_string(key.0.borrow()).as_deref(),
            Some("PerformanceStatistics")
        );
        assert!(cf_array_items(key.0.borrow()).is_empty());
    }

    #[test]
    fn registry_walk_finds_the_platform_expert() {
        let roots = services(c"IOPlatformExpertDevice");
        let Some(root) = roots.first() else {
            return;
        };
        assert!(root.name().is_some());
        assert!(root.registry_id().is_some(), "registry entry id");
        let model = CfKey::new(c"model").expect("key");
        if let Some(value) = property(root, &model) {
            assert!(cf_string(value.borrow()).is_some_and(|m| !m.is_empty()));
        }
        let _ = children(root, c"IOService");
    }
}
