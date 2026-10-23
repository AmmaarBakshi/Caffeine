//! The Caffeine plugin contract.
//!
//! A plugin is described by a [`Manifest`] (`caffeine-plugin.toml`) that
//! declares its identity, the protocol versions it speaks, the permissions
//! it needs, the commands it provides and its configuration schema.
//!
//! Plugins implement the [`Plugin`] trait. Built-in plugins are compiled
//! into the `caffeine` binary and called in-process; third-party plugins
//! are separate executables that call [`sdk::serve`], which speaks the
//! [`protocol`] (JSON Lines over stdin/stdout) on their behalf. The trait is
//! the same either way, so a plugin can start life as a third-party
//! executable and later be linked in without changes.
//!
//! Caffeine never loads dynamic libraries: Rust has no stable ABI, and a
//! process boundary is the only portable way to isolate a crash in
//! third-party code from the session that keeps the machine awake.

pub mod manifest;
pub mod permission;

use semver::Version;

pub use manifest::{CommandSpec, ExecutableSpec, MANIFEST_FILE, Manifest, Problem, SettingSpec};
pub use permission::Permission;

/// Version of the plugin protocol implemented by this crate. A plugin
/// whose manifest `api` requirement does not match it is not run.
///
/// Compatibility rules: a new minor version only adds optional message
/// fields and message types that older peers may ignore; a new major
/// version may change or remove anything.
pub const PROTOCOL_VERSION: Version = Version::new(1, 0, 0);

/// Whether `name` is a valid plugin or command name: lowercase ASCII
/// letters, digits and single hyphens, starting with a letter and not
/// ending with a hyphen, at most 64 bytes.
pub fn is_valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && !name.ends_with('-')
        && !name.contains("--")
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

#[cfg(test)]
mod tests {
    use super::is_valid_name;

    #[test]
    fn names() {
        for good in ["build", "my-plugin", "a1"] {
            assert!(is_valid_name(good));
        }
        for bad in ["", "Build", "1a", "a--b", "a-", "a_b", "a b"] {
            assert!(!is_valid_name(bad), "{bad}");
        }
    }
}
