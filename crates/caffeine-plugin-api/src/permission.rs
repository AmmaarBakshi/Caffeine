//! Permissions a plugin declares in its manifest.
//!
//! Third-party plugins run as separate processes with the user's privileges;
//! Caffeine cannot sandbox them. Declarations exist so that the user can
//! review what a plugin intends to do before trusting it, and so that the
//! host can withhold the capabilities it *does* mediate:
//!
//! | Permission      | Enforced by the host                                        |
//! |-----------------|-------------------------------------------------------------|
//! | `env.inherit`   | yes: without it the plugin gets a minimal environment       |
//! | `power.inhibit` | yes: `keep_awake` commands are refused without it           |
//! | `process.spawn` | no: declaration only                                        |
//! | `fs.read`       | no: declaration only                                        |
//! | `fs.write`      | no: declaration only                                        |
//! | `network`       | no: declaration only                                        |

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A declared permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Permission {
    /// Start other programs (build tools, compilers, ...).
    #[serde(rename = "process.spawn")]
    ProcessSpawn,
    /// Read files outside the plugin's own directory.
    #[serde(rename = "fs.read")]
    FsRead,
    /// Create, modify or delete files outside the plugin's own directory.
    #[serde(rename = "fs.write")]
    FsWrite,
    /// Make network connections.
    #[serde(rename = "network")]
    Network,
    /// Receive the full environment of the `caffeine` process instead of a
    /// minimal allow-list.
    #[serde(rename = "env.inherit")]
    EnvInherit,
    /// Ask the host to keep the system awake while a command runs.
    #[serde(rename = "power.inhibit")]
    PowerInhibit,
}

impl Permission {
    pub const ALL: [Permission; 6] = [
        Permission::ProcessSpawn,
        Permission::FsRead,
        Permission::FsWrite,
        Permission::Network,
        Permission::EnvInherit,
        Permission::PowerInhibit,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Permission::ProcessSpawn => "process.spawn",
            Permission::FsRead => "fs.read",
            Permission::FsWrite => "fs.write",
            Permission::Network => "network",
            Permission::EnvInherit => "env.inherit",
            Permission::PowerInhibit => "power.inhibit",
        }
    }

    /// One-line explanation shown when a user reviews a plugin.
    pub fn description(self) -> &'static str {
        match self {
            Permission::ProcessSpawn => "start other programs",
            Permission::FsRead => "read files outside its own directory",
            Permission::FsWrite => "create, modify or delete files outside its own directory",
            Permission::Network => "make network connections",
            Permission::EnvInherit => "see all environment variables (which may contain secrets)",
            Permission::PowerInhibit => "keep the system awake while one of its commands runs",
        }
    }

    /// Whether the host can withhold this permission from a plugin that has
    /// not been granted it. The others are declarations the user must trust.
    pub fn is_enforced(self) -> bool {
        matches!(self, Permission::EnvInherit | Permission::PowerInhibit)
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Permission {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Permission::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| {
                let known: Vec<_> = Permission::ALL.iter().map(|p| p.as_str()).collect();
                format!("unknown permission \"{s}\" (known: {})", known.join(", "))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for p in Permission::ALL {
            assert_eq!(p.as_str().parse::<Permission>().unwrap(), p);
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
        }
        assert!("root".parse::<Permission>().is_err());
    }
}
