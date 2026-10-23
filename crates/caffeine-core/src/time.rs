//! Wall-clock helpers.
//!
//! Session deadlines are stored as UNIX timestamps (wall-clock time) rather
//! than monotonic instants. Monotonic clocks on Linux and macOS stop while the
//! machine is suspended, so a deadline based on them would silently extend a
//! session by however long the machine slept. Wall-clock deadlines expire
//! correctly after resume.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds since the UNIX epoch. Clamps to 0 if the clock is before 1970.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format a UNIX timestamp as an RFC 3339 UTC string, e.g.
/// `2026-10-23T06:58:12Z`.
pub fn format_rfc3339(ts: u64) -> String {
    let days = (ts / 86_400) as i64;
    let rem = ts % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Convert days since 1970-01-01 to a (year, month, day) civil date.
/// Algorithm from Howard Hinnant's `chrono`-compatible date library.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Describe `ts` relative to `now`, e.g. `5m ago` or `in 1h 30m`.
pub fn format_relative(ts: u64, now: u64) -> String {
    if ts <= now {
        let d = now - ts;
        if d < 5 {
            "just now".to_string()
        } else {
            format!("{} ago", crate::duration::format_duration(Duration::from_secs(d)))
        }
    } else {
        format!(
            "in {}",
            crate::duration::format_duration(Duration::from_secs(ts - now))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_rfc3339(1_792_738_692), "2026-10-23T06:58:12Z");
    }

    #[test]
    fn relative_formatting() {
        assert_eq!(format_relative(100, 100), "just now");
        assert_eq!(format_relative(40, 100), "1m ago");
        assert_eq!(format_relative(100 + 5_400, 100), "in 1h 30m");
    }
}
