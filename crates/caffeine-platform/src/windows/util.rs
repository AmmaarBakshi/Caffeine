//! Small Win32 helpers.

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, GetLastError, WIN32_ERROR};
use windows_sys::core::GUID;

use caffeine_core::Error;

/// NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Decode a NUL-terminated (or full) UTF-16 buffer.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Build an error from a Win32 error code, classifying access denial.
pub fn win_error(context: &str, code: WIN32_ERROR) -> Error {
    let message = format!("{context} failed: {}", std::io::Error::from_raw_os_error(code as i32));
    let err = if code == ERROR_ACCESS_DENIED {
        Error::permission(message).with_hint(
            "Windows denied the change; it may be restricted by group policy or require an \
             elevated (Administrator) terminal",
        )
    } else {
        Error::failure(message)
    };
    err.with_os_code(i64::from(code))
}

/// Error from `GetLastError()` after a failed call.
pub fn last_error(context: &str) -> Error {
    // SAFETY: GetLastError has no preconditions.
    let code = unsafe { GetLastError() };
    win_error(context, code)
}

pub fn guid_to_u128(g: &GUID) -> u128 {
    (u128::from(g.data1) << 96)
        | (u128::from(g.data2) << 80)
        | (u128::from(g.data3) << 64)
        | u128::from(u64::from_be_bytes(g.data4))
}

pub fn guid_to_string(g: &GUID) -> String {
    let v = format!("{:032x}", guid_to_u128(g));
    format!("{}-{}-{}-{}-{}", &v[0..8], &v[8..12], &v[12..16], &v[16..20], &v[20..32])
}

pub fn guid_from_string(s: &str) -> Option<GUID> {
    let hex: String = s.trim_matches(|c| c == '{' || c == '}').chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_round_trip() {
        let g = GUID::from_u128(0x5ca83367_6e45_459f_a27b_476b1d01c936);
        let s = guid_to_string(&g);
        assert_eq!(s, "5ca83367-6e45-459f-a27b-476b1d01c936");
        assert_eq!(guid_to_u128(&guid_from_string(&s).unwrap()), guid_to_u128(&g));
        assert!(guid_from_string("nope").is_none());
    }

    #[test]
    fn wide_strings() {
        let w = wide("hé");
        assert_eq!(w.last(), Some(&0));
        assert_eq!(from_wide(&w), "hé");
    }
}
