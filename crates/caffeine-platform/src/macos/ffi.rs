//! Minimal CoreFoundation / IOKit bindings used by the macOS backend.
//!
//! Ownership follows the CoreFoundation "Create/Copy rule": objects returned
//! by `*Create*` / `*Copy*` functions are owned and released through
//! [`CfOwned`]; objects from `*Get*` functions are borrowed.

#![allow(non_upper_case_globals, non_snake_case, non_camel_case_types)]

use std::ffi::{CString, c_char, c_void};

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFMutableDictionaryRef = *mut c_void;
pub type CFArrayRef = *const c_void;
pub type CFIndex = isize;
pub type CFTypeID = usize;
pub type Boolean = u8;
pub type IOReturn = i32;
pub type IOPMAssertionID = u32;
pub type io_object_t = u32;
pub type mach_port_t = u32;

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt32Type: CFIndex = 3;
pub const kIOPMAssertionLevelOn: u32 = 255;
pub const kIOReturnSuccess: IOReturn = 0;
pub const kIOReturnNotPrivileged: IOReturn = 0xe000_02c1_u32 as i32;
pub const kIOReturnNotPermitted: IOReturn = 0xe000_02e2_u32 as i32;
pub const kIOMainPortDefault: mach_port_t = 0;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        cstr: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    pub fn CFStringGetCString(
        s: CFStringRef,
        buf: *mut c_char,
        size: CFIndex,
        encoding: u32,
    ) -> Boolean;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFBooleanGetTypeID() -> CFTypeID;
    pub fn CFBooleanGetValue(b: CFTypeRef) -> Boolean;
    pub fn CFNumberGetTypeID() -> CFTypeID;
    pub fn CFNumberGetValue(n: CFTypeRef, t: CFIndex, out: *mut c_void) -> Boolean;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFArrayGetCount(a: CFArrayRef) -> CFIndex;
    pub fn CFArrayGetValueAtIndex(a: CFArrayRef, i: CFIndex) -> *const c_void;
    pub fn CFDictionaryGetValue(d: CFDictionaryRef, key: *const c_void) -> *const c_void;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    pub fn IOPMAssertionCreateWithName(
        assertion_type: CFStringRef,
        level: u32,
        name: CFStringRef,
        id: *mut IOPMAssertionID,
    ) -> IOReturn;
    pub fn IOPMAssertionRelease(id: IOPMAssertionID) -> IOReturn;
    pub fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    pub fn IOServiceGetMatchingService(
        main_port: mach_port_t,
        matching: CFDictionaryRef,
    ) -> io_object_t;
    pub fn IORegistryEntryCreateCFProperty(
        entry: io_object_t,
        key: CFStringRef,
        alloc: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
    pub fn IOObjectRelease(object: io_object_t) -> IOReturn;
    pub fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
    pub fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFArrayRef;
    pub fn IOPSGetPowerSourceDescription(blob: CFTypeRef, ps: CFTypeRef) -> CFDictionaryRef;
    pub fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFStringRef;
}

/// An owned CoreFoundation object, released on drop.
pub struct CfOwned(CFTypeRef);

impl CfOwned {
    /// Take ownership of a +1 reference. Returns `None` for null.
    pub fn new(ptr: CFTypeRef) -> Option<CfOwned> {
        (!ptr.is_null()).then_some(CfOwned(ptr))
    }

    pub fn as_ptr(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for CfOwned {
    fn drop(&mut self) {
        // SAFETY: we own exactly one reference.
        unsafe { CFRelease(self.0) }
    }
}

/// Create a CFString from a Rust string.
pub fn cf_string(s: &str) -> Option<CfOwned> {
    let c = CString::new(s).ok()?;
    // SAFETY: `c` is a valid NUL-terminated string for the call.
    CfOwned::new(unsafe {
        CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), kCFStringEncodingUTF8)
    })
}

/// Convert a borrowed CFString to a Rust string.
pub fn to_string(s: CFStringRef) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: type check before treating the object as a CFString.
    if unsafe { CFGetTypeID(s) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = vec![0 as c_char; 512];
    // SAFETY: buffer length is passed correctly.
    let ok = unsafe {
        CFStringGetCString(
            s,
            buf.as_mut_ptr(),
            buf.len() as CFIndex,
            kCFStringEncodingUTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    // SAFETY: CFStringGetCString NUL-terminated the buffer.
    Some(
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// Interpret a borrowed CF object as a boolean.
pub fn to_bool(v: CFTypeRef) -> Option<bool> {
    // SAFETY: type check before reading.
    (!v.is_null() && unsafe { CFGetTypeID(v) == CFBooleanGetTypeID() })
        .then(|| unsafe { CFBooleanGetValue(v) != 0 })
}

/// Interpret a borrowed CF object as an i32 number.
pub fn to_i32(v: CFTypeRef) -> Option<i32> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFNumberGetTypeID() } {
        return None;
    }
    let mut out: i32 = 0;
    // SAFETY: `out` matches kCFNumberSInt32Type.
    let ok = unsafe { CFNumberGetValue(v, kCFNumberSInt32Type, (&mut out as *mut i32).cast()) };
    (ok != 0).then_some(out)
}

/// Look up a borrowed value in a CFDictionary by string key.
pub fn dict_get(dict: CFDictionaryRef, key: &str) -> CFTypeRef {
    let Some(k) = cf_string(key) else {
        return std::ptr::null();
    };
    // SAFETY: dict is a valid CFDictionary; the key is a valid CFString.
    unsafe { CFDictionaryGetValue(dict, k.as_ptr()) }
}

/// Read a property of the `IOPMrootDomain` registry entry.
pub fn root_domain_property(key: &str) -> Option<CfOwned> {
    let name = CString::new("IOPMrootDomain").ok()?;
    // SAFETY: IOServiceGetMatchingService consumes the matching dictionary.
    let service = unsafe {
        IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching(name.as_ptr()))
    };
    if service == 0 {
        return None;
    }
    let key = cf_string(key)?;
    // SAFETY: valid service and key; the result follows the Create rule.
    let value =
        unsafe { IORegistryEntryCreateCFProperty(service, key.as_ptr(), std::ptr::null(), 0) };
    // SAFETY: releasing the service object obtained above.
    unsafe { IOObjectRelease(service) };
    CfOwned::new(value)
}
