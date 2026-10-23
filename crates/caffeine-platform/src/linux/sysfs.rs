//! Facts read from sysfs/procfs: power supplies, lid switch, init system,
//! distribution and WSL detection. All functions take a root path so they
//! can be tested against a fake filesystem tree.

use std::fs;
use std::path::Path;

use caffeine_core::power::{LidState, PowerSource, PowerSourceKind};

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Aggregate `/sys/class/power_supply`.
pub fn power_source(root: &Path) -> Option<PowerSource> {
    let dir = root.join("sys/class/power_supply");
    let entries = fs::read_dir(&dir).ok()?;
    let mut mains_online = false;
    let mut mains_seen = false;
    let mut batteries = 0u32;
    let mut capacity_sum = 0u32;
    let mut capacity_count = 0u32;
    let mut charging = None;
    let mut discharging = false;
    for entry in entries.flatten() {
        let p = entry.path();
        // Peripheral batteries (mice, keyboards) report scope "Device".
        if read_trim(&p.join("scope")).is_some_and(|s| s.eq_ignore_ascii_case("device")) {
            continue;
        }
        match read_trim(&p.join("type")).as_deref() {
            Some("Mains") | Some("USB") | Some("USB_C") | Some("USB_PD") => {
                mains_seen = true;
                if read_trim(&p.join("online")).as_deref() == Some("1") {
                    mains_online = true;
                }
            }
            Some("Battery") => {
                if read_trim(&p.join("present")).as_deref() == Some("0") {
                    continue;
                }
                batteries += 1;
                if let Some(c) = read_trim(&p.join("capacity")).and_then(|c| c.parse::<u32>().ok()) {
                    capacity_sum += c.min(100);
                    capacity_count += 1;
                }
                match read_trim(&p.join("status")).as_deref() {
                    Some("Charging") => charging = Some(true),
                    Some("Discharging") => {
                        discharging = true;
                        charging.get_or_insert(false);
                    }
                    Some(_) => {
                        charging.get_or_insert(false);
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }
    let kind = if mains_online {
        PowerSourceKind::Ac
    } else if batteries > 0 && (discharging || mains_seen) {
        PowerSourceKind::Battery
    } else {
        // No batteries, or a battery without mains information that is not
        // discharging: we cannot tell.
        PowerSourceKind::Unknown
    };
    Some(PowerSource {
        kind,
        battery_present: batteries > 0,
        battery_percent: (capacity_count > 0).then(|| (capacity_sum / capacity_count) as u8),
        charging: if batteries > 0 { charging } else { None },
    })
}

/// Lid state from `/proc/acpi/button/lid/*/state`, if an ACPI lid exists.
pub fn acpi_lid_state(root: &Path) -> Option<LidState> {
    let dir = root.join("proc/acpi/button/lid");
    for entry in fs::read_dir(dir).ok()?.flatten() {
        if let Some(text) = read_trim(&entry.path().join("state")) {
            let lower = text.to_ascii_lowercase();
            if lower.contains("closed") {
                return Some(LidState::Closed);
            }
            if lower.contains("open") {
                return Some(LidState::Open);
            }
            return Some(LidState::Unknown);
        }
    }
    None
}

/// Whether any input device advertises the `SW_LID` switch (bit 0 of the
/// `sw` capability mask). Covers lids that are not ACPI buttons (ARM laptops).
pub fn input_lid_switch_present(root: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root.join("sys/class/input")) else {
        return false;
    };
    entries.flatten().any(|e| {
        read_trim(&e.path().join("capabilities/sw"))
            .and_then(|mask| {
                // The mask is space-separated hex words, most significant first.
                mask.split_whitespace().last().and_then(|w| u64::from_str_radix(w, 16).ok())
            })
            .is_some_and(|low| low & 1 == 1)
    })
}

pub fn lid_present(root: &Path) -> bool {
    acpi_lid_state(root).is_some() || input_lid_switch_present(root)
}

/// Best-effort init system name.
pub fn init_system(root: &Path) -> String {
    if root.join("run/systemd/system").is_dir() {
        return "systemd".into();
    }
    if root.join("run/openrc").is_dir() {
        return "openrc".into();
    }
    if root.join("run/runit").is_dir() || root.join("etc/runit").is_dir() {
        return "runit".into();
    }
    if root.join("run/s6").is_dir() {
        return "s6".into();
    }
    read_trim(&root.join("proc/1/comm")).unwrap_or_else(|| "unknown".into())
}

/// `PRETTY_NAME` from os-release.
pub fn distribution(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join("etc/os-release"))
        .or_else(|_| fs::read_to_string(root.join("usr/lib/os-release")))
        .ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').to_string())
}

/// Running under Windows Subsystem for Linux?
pub fn is_wsl(root: &Path) -> bool {
    read_trim(&root.join("proc/sys/kernel/osrelease"))
        .is_some_and(|r| r.to_ascii_lowercase().contains("microsoft"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn laptop_on_battery() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        write(r, "sys/class/power_supply/AC/type", "Mains\n");
        write(r, "sys/class/power_supply/AC/online", "0\n");
        write(r, "sys/class/power_supply/BAT0/type", "Battery\n");
        write(r, "sys/class/power_supply/BAT0/capacity", "42\n");
        write(r, "sys/class/power_supply/BAT0/status", "Discharging\n");
        write(r, "sys/class/power_supply/hidpp_battery_0/type", "Battery\n");
        write(r, "sys/class/power_supply/hidpp_battery_0/scope", "Device\n");
        write(r, "sys/class/power_supply/hidpp_battery_0/capacity", "5\n");
        let ps = power_source(r).unwrap();
        assert_eq!(ps.kind, PowerSourceKind::Battery);
        assert_eq!(ps.battery_percent, Some(42), "peripheral batteries are ignored");
        assert_eq!(ps.charging, Some(false));
    }

    #[test]
    fn laptop_charging_on_ac() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        write(r, "sys/class/power_supply/ADP1/type", "Mains");
        write(r, "sys/class/power_supply/ADP1/online", "1");
        write(r, "sys/class/power_supply/BAT1/type", "Battery");
        write(r, "sys/class/power_supply/BAT1/capacity", "80");
        write(r, "sys/class/power_supply/BAT1/status", "Charging");
        let ps = power_source(r).unwrap();
        assert_eq!(ps.kind, PowerSourceKind::Ac);
        assert_eq!(ps.charging, Some(true));
    }

    #[test]
    fn desktop_without_supplies() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("sys/class/power_supply")).unwrap();
        let ps = power_source(t.path()).unwrap();
        assert_eq!(ps.kind, PowerSourceKind::Unknown);
        assert!(!ps.battery_present);
        assert!(power_source(&t.path().join("nowhere")).is_none());
    }

    #[test]
    fn lid_detection() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        assert!(!lid_present(r));
        write(r, "proc/acpi/button/lid/LID0/state", "state:      closed\n");
        assert_eq!(acpi_lid_state(r), Some(LidState::Closed));
        assert!(lid_present(r));

        let t2 = tempfile::tempdir().unwrap();
        write(t2.path(), "sys/class/input/input3/capabilities/sw", "1\n");
        write(t2.path(), "sys/class/input/input4/capabilities/sw", "0\n");
        assert!(input_lid_switch_present(t2.path()));
    }

    #[test]
    fn system_facts() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        write(r, "etc/os-release", "NAME=Fedora\nPRETTY_NAME=\"Fedora Linux 41 (Workstation Edition)\"\n");
        write(r, "proc/sys/kernel/osrelease", "6.6.87.2-microsoft-standard-WSL2\n");
        write(r, "proc/1/comm", "init\n");
        assert_eq!(distribution(r).unwrap(), "Fedora Linux 41 (Workstation Edition)");
        assert!(is_wsl(r));
        assert_eq!(init_system(r), "init");
        fs::create_dir_all(r.join("run/systemd/system")).unwrap();
        assert_eq!(init_system(r), "systemd");
    }
}
