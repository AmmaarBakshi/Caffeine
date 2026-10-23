//! D-Bus helpers and inhibitor guards.
//!
//! * **logind** (`org.freedesktop.login1`, system bus; systemd-logind or
//!   elogind): `Inhibit(what, who, why, "block")` returns a file descriptor.
//!   The inhibitor lasts exactly as long as the descriptor is open, so the
//!   kernel releases it when the process dies.
//! * **Desktop session** (session bus): GNOME's `org.gnome.SessionManager`,
//!   or the freedesktop `PowerManagement.Inhibit` / `ScreenSaver` interfaces
//!   used by KDE Plasma, Xfce, LXQt, MATE and others. These return a cookie
//!   and are tied to the D-Bus connection, which the guard keeps open.

use std::os::fd::AsRawFd;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedFd, OwnedValue};

use caffeine_core::power::{ActiveAssertion, AwakeGuard, Capability};
use caffeine_core::{Error, Result};

pub const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

/// Convert a zbus error, classifying authorization failures.
pub fn dbus_error(context: &str, err: zbus::Error) -> Error {
    if let zbus::Error::MethodError(name, message, _) = &err {
        let name = name.as_str();
        let detail = message.clone().unwrap_or_default();
        let text = format!("{context}: {name}: {detail}");
        if name.ends_with("AccessDenied")
            || name.ends_with("InteractiveAuthorizationRequired")
            || name.ends_with("NotAuthorized")
        {
            return Error::permission(text).with_hint(
                "polkit denied the request; lid-switch inhibitors are only granted to the active \
                 local session (not SSH or background sessions) by default",
            );
        }
        if name.ends_with("ServiceUnknown")
            || name.ends_with("NameHasNoOwner")
            || name.ends_with("UnknownMethod")
        {
            return Error::unsupported(text);
        }
        return Error::failure(text);
    }
    Error::failure(format!("{context}: {err}"))
}

pub fn system_bus() -> Result<Connection> {
    Connection::system().map_err(|e| {
        dbus_error("connecting to the D-Bus system bus", e)
            .with_hint("is dbus running? Containers and minimal systems may lack a system bus")
    })
}

pub fn session_bus() -> Result<Connection> {
    Connection::session().map_err(|e| dbus_error("connecting to the D-Bus session bus", e))
}

pub fn name_has_owner(conn: &Connection, name: &str) -> bool {
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "NameHasOwner",
        &name,
    )
    .ok()
    .and_then(|m| m.body().deserialize::<bool>().ok())
    .unwrap_or(false)
}

pub fn get_property(
    conn: &Connection,
    dest: &str,
    path: &str,
    iface: &str,
    prop: &str,
) -> Result<OwnedValue> {
    let reply = conn
        .call_method(
            Some(dest),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(iface, prop),
        )
        .map_err(|e| dbus_error(&format!("reading {iface}.{prop}"), e))?;
    reply
        .body()
        .deserialize::<OwnedValue>()
        .map_err(|e| Error::failure(format!("decoding {iface}.{prop}: {e}")))
}

pub fn logind_string(conn: &Connection, prop: &str) -> Result<String> {
    let v = get_property(conn, LOGIND, LOGIND_PATH, LOGIND_MANAGER, prop)?;
    String::try_from(v).map_err(|e| Error::failure(format!("decoding {prop}: {e}")))
}

pub fn logind_bool(conn: &Connection, prop: &str) -> Result<bool> {
    let v = get_property(conn, LOGIND, LOGIND_PATH, LOGIND_MANAGER, prop)?;
    bool::try_from(v).map_err(|e| Error::failure(format!("decoding {prop}: {e}")))
}

/// One entry of logind's `ListInhibitors`.
#[derive(Debug, Clone)]
pub struct InhibitorInfo {
    pub what: String,
    pub who: String,
    pub why: String,
    pub mode: String,
    pub pid: u32,
}

pub fn list_inhibitors(conn: &Connection) -> Result<Vec<InhibitorInfo>> {
    let reply = conn
        .call_method(
            Some(LOGIND),
            LOGIND_PATH,
            Some(LOGIND_MANAGER),
            "ListInhibitors",
            &(),
        )
        .map_err(|e| dbus_error("logind ListInhibitors", e))?;
    let rows: Vec<(String, String, String, String, u32, u32)> = reply
        .body()
        .deserialize()
        .map_err(|e| Error::failure(format!("decoding ListInhibitors: {e}")))?;
    Ok(rows
        .into_iter()
        .map(|(what, who, why, mode, _uid, pid)| InhibitorInfo {
            what,
            who,
            why,
            mode,
            pid,
        })
        .collect())
}

/// A logind inhibitor; released when the descriptor is closed.
pub struct LogindInhibitor {
    fd: Option<OwnedFd>,
    what: &'static str,
    capability: Capability,
}

impl LogindInhibitor {
    pub fn acquire(
        conn: &Connection,
        what: &'static str,
        why: &str,
        capability: Capability,
    ) -> Result<LogindInhibitor> {
        let reply = conn
            .call_method(
                Some(LOGIND),
                LOGIND_PATH,
                Some(LOGIND_MANAGER),
                "Inhibit",
                &(what, "caffeine", why, "block"),
            )
            .map_err(|e| dbus_error(&format!("logind Inhibit(\"{what}\")"), e))?;
        let fd: OwnedFd = reply
            .body()
            .deserialize()
            .map_err(|e| Error::failure(format!("decoding logind inhibitor descriptor: {e}")))?;
        log::debug!("logind inhibitor {what} held on fd {}", fd.as_raw_fd());
        Ok(LogindInhibitor {
            fd: Some(fd),
            what,
            capability,
        })
    }
}

impl AwakeGuard for LogindInhibitor {
    fn assertions(&self) -> Vec<ActiveAssertion> {
        vec![ActiveAssertion {
            capability: self.capability,
            mechanism: format!("logind Inhibit({}, block)", self.what),
            detail: "listed by `systemd-inhibit --list`".into(),
        }]
    }

    fn release(mut self: Box<Self>) -> Result<()> {
        // Closing the descriptor releases the inhibitor; close errors on a
        // pipe are not actionable.
        self.fd.take();
        Ok(())
    }
}

/// Which desktop interface holds an inhibitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopApi {
    /// `org.gnome.SessionManager.Inhibit` with the given flags
    /// (4 = suspend, 8 = idle).
    GnomeSession { flags: u32 },
    /// `org.freedesktop.PowerManagement.Inhibit` (suspend on idle).
    FreedesktopPowerManagement,
    /// `org.freedesktop.ScreenSaver.Inhibit` (screen blanking / idle).
    FreedesktopScreenSaver,
}

impl DesktopApi {
    fn bus_name(self) -> &'static str {
        match self {
            DesktopApi::GnomeSession { .. } => "org.gnome.SessionManager",
            DesktopApi::FreedesktopPowerManagement => "org.freedesktop.PowerManagement",
            DesktopApi::FreedesktopScreenSaver => "org.freedesktop.ScreenSaver",
        }
    }

    fn path(self) -> &'static str {
        match self {
            DesktopApi::GnomeSession { .. } => "/org/gnome/SessionManager",
            DesktopApi::FreedesktopPowerManagement => "/org/freedesktop/PowerManagement/Inhibit",
            DesktopApi::FreedesktopScreenSaver => "/org/freedesktop/ScreenSaver",
        }
    }

    fn interface(self) -> &'static str {
        match self {
            DesktopApi::GnomeSession { .. } => "org.gnome.SessionManager",
            DesktopApi::FreedesktopPowerManagement => "org.freedesktop.PowerManagement.Inhibit",
            DesktopApi::FreedesktopScreenSaver => "org.freedesktop.ScreenSaver",
        }
    }

    pub fn describe(self) -> String {
        match self {
            DesktopApi::GnomeSession { flags } => {
                format!("org.gnome.SessionManager.Inhibit(flags={flags})")
            }
            DesktopApi::FreedesktopPowerManagement => {
                "org.freedesktop.PowerManagement.Inhibit".into()
            }
            DesktopApi::FreedesktopScreenSaver => "org.freedesktop.ScreenSaver.Inhibit".into(),
        }
    }

    pub fn available(self, session: &Connection) -> bool {
        name_has_owner(session, self.bus_name())
    }
}

/// A cookie-based desktop inhibitor bound to an open session-bus connection.
pub struct DesktopInhibitor {
    conn: Connection,
    api: DesktopApi,
    cookie: u32,
    capability: Capability,
    released: bool,
}

impl DesktopInhibitor {
    pub fn acquire(
        conn: Connection,
        api: DesktopApi,
        reason: &str,
        capability: Capability,
    ) -> Result<DesktopInhibitor> {
        let reply = match api {
            DesktopApi::GnomeSession { flags } => conn.call_method(
                Some(api.bus_name()),
                api.path(),
                Some(api.interface()),
                "Inhibit",
                &("caffeine", 0u32, reason, flags),
            ),
            _ => conn.call_method(
                Some(api.bus_name()),
                api.path(),
                Some(api.interface()),
                "Inhibit",
                &("caffeine", reason),
            ),
        }
        .map_err(|e| dbus_error(&api.describe(), e))?;
        let cookie: u32 = reply
            .body()
            .deserialize()
            .map_err(|e| Error::failure(format!("decoding {} cookie: {e}", api.describe())))?;
        Ok(DesktopInhibitor {
            conn,
            api,
            cookie,
            capability,
            released: false,
        })
    }

    fn uninhibit(&mut self) -> Result<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        let method = match self.api {
            DesktopApi::GnomeSession { .. } => "Uninhibit",
            _ => "UnInhibit",
        };
        self.conn
            .call_method(
                Some(self.api.bus_name()),
                self.api.path(),
                Some(self.api.interface()),
                method,
                &self.cookie,
            )
            .map(|_| ())
            .map_err(|e| dbus_error(&format!("{} {method}", self.api.describe()), e))
    }
}

impl AwakeGuard for DesktopInhibitor {
    fn assertions(&self) -> Vec<ActiveAssertion> {
        vec![ActiveAssertion {
            capability: self.capability,
            mechanism: self.api.describe(),
            detail: format!("cookie {}", self.cookie),
        }]
    }

    fn release(mut self: Box<Self>) -> Result<()> {
        self.uninhibit()
    }
}

impl Drop for DesktopInhibitor {
    fn drop(&mut self) {
        // The desktop also drops the inhibitor when our connection closes,
        // so a failure here is logged rather than escalated.
        if let Err(e) = self.uninhibit() {
            log::warn!("{e}");
        }
    }
}
