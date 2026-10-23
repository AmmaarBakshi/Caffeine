//! Human-friendly duration parsing and formatting.
//!
//! Accepted input forms (case-insensitive, whitespace between parts allowed):
//!
//! * `90s`, `45m`, `2h`, `1d`, `1w`
//! * compound forms such as `1h30m` or `2h 15m 10s`
//! * long unit names: `30min`, `2hours`, `1 day`
//! * `indefinite`, `infinite`, `forever`, `none` for "no time limit"
//!
//! A bare number without a unit is rejected because it is ambiguous.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// Parse a duration string. Returns an error for indefinite keywords; use
/// [`SessionLength::from_str`] when "no limit" is a valid answer.
pub fn parse_duration(input: &str) -> Result<Duration> {
    let s = input.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err(Error::usage("empty duration"));
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut total: u64 = 0;
    let mut parts = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let num_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if num_start == i {
            return Err(invalid(input, "expected a number"));
        }
        let value: u64 = s[num_start..i]
            .parse()
            .map_err(|_| invalid(input, "number is too large"))?;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let unit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let unit = &s[unit_start..i];
        if unit.is_empty() {
            return Err(invalid(
                input,
                "missing unit (use s, m, h, d or w, e.g. \"30m\" or \"2h\")",
            ));
        }
        let multiplier = match unit {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
            "d" | "day" | "days" => 86_400,
            "w" | "week" | "weeks" => 604_800,
            other => return Err(invalid(input, &format!("unknown unit \"{other}\""))),
        };
        total = value
            .checked_mul(multiplier)
            .and_then(|v| total.checked_add(v))
            .ok_or_else(|| invalid(input, "duration is too large"))?;
        parts += 1;
    }
    if parts == 0 {
        return Err(invalid(input, "expected a number"));
    }
    if total == 0 {
        return Err(invalid(input, "duration must be greater than zero"));
    }
    Ok(Duration::from_secs(total))
}

fn invalid(input: &str, why: &str) -> Error {
    Error::usage(format!("invalid duration \"{input}\": {why}"))
}

/// Format a duration compactly, e.g. `1h 30m`, `45s`, `2d 3h`.
///
/// Seconds are omitted once the duration exceeds one hour so that status
/// output stays readable.
pub fn format_duration(d: Duration) -> String {
    let mut secs = d.as_secs();
    if secs == 0 {
        return "0s".to_string();
    }
    let show_seconds = secs < 3_600;
    let days = secs / 86_400;
    secs %= 86_400;
    let hours = secs / 3_600;
    secs %= 3_600;
    let minutes = secs / 60;
    secs %= 60;
    let mut out = Vec::new();
    if days > 0 {
        out.push(format!("{days}d"));
    }
    if hours > 0 {
        out.push(format!("{hours}h"));
    }
    if minutes > 0 {
        out.push(format!("{minutes}m"));
    }
    if show_seconds && secs > 0 {
        out.push(format!("{secs}s"));
    }
    if out.is_empty() {
        // e.g. exactly 1h 0m 30s with seconds hidden
        out.push("0m".to_string());
    }
    out.join(" ")
}

/// Format a duration in the canonical, re-parseable form used in config
/// files, e.g. `1h30m`.
pub fn format_duration_canonical(d: Duration) -> String {
    let mut secs = d.as_secs();
    let mut out = String::new();
    for (unit, size) in [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)] {
        let n = secs / size;
        if n > 0 {
            out.push_str(&format!("{n}{unit}"));
            secs %= size;
        }
    }
    if out.is_empty() {
        out.push_str("0s");
    }
    out
}

/// A session length: either a fixed duration or no time limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionLength {
    #[default]
    Indefinite,
    Fixed(Duration),
}

impl SessionLength {
    pub fn as_duration(&self) -> Option<Duration> {
        match self {
            SessionLength::Indefinite => None,
            SessionLength::Fixed(d) => Some(*d),
        }
    }

    pub fn is_indefinite(&self) -> bool {
        matches!(self, SessionLength::Indefinite)
    }
}

impl From<Option<Duration>> for SessionLength {
    fn from(d: Option<Duration>) -> Self {
        d.map_or(SessionLength::Indefinite, SessionLength::Fixed)
    }
}

const INDEFINITE_WORDS: &[&str] = &["indefinite", "infinite", "forever", "none", "unlimited"];

impl FromStr for SessionLength {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let t = s.trim().to_ascii_lowercase();
        if INDEFINITE_WORDS.contains(&t.as_str()) {
            Ok(SessionLength::Indefinite)
        } else {
            parse_duration(s).map(SessionLength::Fixed)
        }
    }
}

impl fmt::Display for SessionLength {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionLength::Indefinite => f.write_str("indefinite"),
            SessionLength::Fixed(d) => f.write_str(&format_duration_canonical(*d)),
        }
    }
}

impl Serialize for SessionLength {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SessionLength {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(|e: Error| serde::de::Error::custom(e.message().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: &str) -> u64 {
        parse_duration(s).unwrap().as_secs()
    }

    #[test]
    fn parses_simple_units() {
        assert_eq!(secs("45s"), 45);
        assert_eq!(secs("30m"), 1_800);
        assert_eq!(secs("2h"), 7_200);
        assert_eq!(secs("1d"), 86_400);
        assert_eq!(secs("1w"), 604_800);
    }

    #[test]
    fn parses_compound_and_long_forms() {
        assert_eq!(secs("1h30m"), 5_400);
        assert_eq!(secs("2h 15m 10s"), 8_110);
        assert_eq!(secs("90 minutes"), 5_400);
        assert_eq!(secs("1 Day 2 HOURS"), 93_600);
        assert_eq!(secs("  10min "), 600);
    }

    #[test]
    fn rejects_ambiguous_or_invalid_input() {
        for bad in ["", "10", "h", "5x", "1h30", "-5m", "0s", "1.5h", "99999999999999999999h"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn session_length_keywords() {
        assert_eq!("indefinite".parse::<SessionLength>().unwrap(), SessionLength::Indefinite);
        assert_eq!("Forever".parse::<SessionLength>().unwrap(), SessionLength::Indefinite);
        assert_eq!(
            "2h".parse::<SessionLength>().unwrap(),
            SessionLength::Fixed(Duration::from_secs(7_200))
        );
    }

    #[test]
    fn formatting() {
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
        assert_eq!(format_duration(Duration::from_secs(5_400)), "1h 30m");
        assert_eq!(format_duration(Duration::from_secs(3_630)), "1h");
        assert_eq!(format_duration(Duration::from_secs(93_600)), "1d 2h");
        assert_eq!(format_duration_canonical(Duration::from_secs(5_410)), "1h30m10s");
    }

    #[test]
    fn canonical_format_round_trips() {
        for s in [1, 59, 60, 61, 3_599, 3_600, 86_399, 90_061, 1_000_000] {
            let d = Duration::from_secs(s);
            assert_eq!(parse_duration(&format_duration_canonical(d)).unwrap(), d);
        }
    }
}
