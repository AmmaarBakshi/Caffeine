//! The subprocess protocol, version 1.
//!
//! Transport: JSON Lines. The host writes [`HostMessage`]s to the plugin's
//! stdin and reads [`PluginMessage`]s from its stdout, one compact JSON
//! object per line, each with a `type` field. The plugin's stderr is passed
//! through to the user's terminal unchanged.
//!
//! Because stdout is the protocol channel, a plugin must not print to it
//! directly, and programs it starts must not inherit it: send their output
//! to stderr (see [`crate::sdk::child_stdout`]) or capture it.
//!
//! ```text
//! host                                   plugin
//!  | -- initialize{protocol,host,...} --> |
//!  | <------------ ready{protocol,...} -- |   (or failed{error})
//!  | -- invoke{command,args,cwd,json} --> |
//!  | <-------- log / output / data ...  - |
//!  | <------------ result{exit_code} ---- |   (or failed{error})
//!  | -- shutdown ------------------------> |   plugin exits with status 0
//! ```
//!
//! `diagnose` may be sent instead of `invoke`; it is answered with
//! `diagnostics{checks}`. Lines longer than [`MAX_LINE_BYTES`] are a
//! protocol violation. Hosts ignore unknown message types and unknown
//! fields within the same major version.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::permission::Permission;
use crate::plugin::{Check, HostInfo, Initialize, Invocation, LogLevel, PluginError};

/// Upper bound on the length of one protocol line.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Messages sent by the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum HostMessage {
    Initialize {
        protocol: semver::Version,
        host: HostInfo,
        settings: Map<String, Value>,
        permissions: Vec<Permission>,
        data_dir: std::path::PathBuf,
    },
    Invoke(Invocation),
    Diagnose,
    Shutdown,
}

impl HostMessage {
    pub fn initialize(protocol: semver::Version, init: Initialize) -> HostMessage {
        HostMessage::Initialize {
            protocol,
            host: init.host,
            settings: init.settings,
            permissions: init.permissions,
            data_dir: init.data_dir,
        }
    }
}

/// Messages sent by the plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PluginMessage {
    /// Answer to `initialize`.
    Ready {
        protocol: semver::Version,
        name: String,
        version: semver::Version,
    },
    /// A request could not be handled.
    Failed {
        error: PluginError,
    },
    Log {
        level: LogLevel,
        message: String,
    },
    /// A line of human-readable output.
    Output {
        text: String,
    },
    /// A machine-readable result fragment.
    Data {
        value: Value,
    },
    /// Final answer to `invoke`.
    Result {
        exit_code: i32,
    },
    /// Final answer to `diagnose`.
    Diagnostics {
        checks: Vec<Check>,
    },
}

/// Encode a message as one protocol line (including the trailing newline).
pub fn encode<T: Serialize>(message: &T) -> String {
    let mut line = serde_json::to_string(message).expect("protocol messages serialize");
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_stable() {
        let msg = PluginMessage::Result { exit_code: 3 };
        assert_eq!(encode(&msg), "{\"type\":\"result\",\"exit_code\":3}\n");
        let msg: HostMessage = serde_json::from_str(
            r#"{"type":"invoke","command":"greet","args":["x"],"cwd":"/tmp","json":false}"#,
        )
        .unwrap();
        assert!(matches!(msg, HostMessage::Invoke(ref i) if i.command == "greet"));
        let msg: HostMessage = serde_json::from_str(r#"{"type":"shutdown"}"#).unwrap();
        assert_eq!(msg, HostMessage::Shutdown);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let msg: PluginMessage =
            serde_json::from_str(r#"{"type":"output","text":"hi","color":"red"}"#).unwrap();
        assert_eq!(msg, PluginMessage::Output { text: "hi".into() });
    }
}
