//! The environment given to plugin processes.
//!
//! Without the `env.inherit` permission a plugin receives only the
//! variables below (when set), plus `CAFFEINE_*` variables describing the
//! plugin. These are what programs commonly need to locate tools, the home
//! directory, temporary storage and the locale; tokens and credentials that
//! often live in the environment are not passed on.

use std::ffi::OsString;
use std::path::Path;

/// Variables passed through without `env.inherit`.
pub const ALLOWED: &[&str] = &[
    // Common
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "TZ",
    "TMPDIR",
    "SHELL",
    // XDG base directories
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    // Windows
    "USERNAME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "OS",
];

/// Whether `name` passes the allow-list. Case-insensitive, because Windows
/// environment variable names are.
pub fn is_allowed(name: &str) -> bool {
    ALLOWED.iter().any(|a| a.eq_ignore_ascii_case(name))
}

/// Everything the plugin process should see, from `vars` (normally
/// [`std::env::vars_os`]).
pub fn build(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    inherit: bool,
    plugin: &str,
    plugin_dir: &Path,
    data_dir: &Path,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = vars
        .into_iter()
        .filter(|(k, _)| {
            let name = k.to_string_lossy();
            // The host's own variables are replaced below.
            !name.to_ascii_uppercase().starts_with("CAFFEINE_") && (inherit || is_allowed(&name))
        })
        .collect();
    env.push(("CAFFEINE_PLUGIN".into(), plugin.into()));
    env.push(("CAFFEINE_PLUGIN_DIR".into(), plugin_dir.into()));
    env.push(("CAFFEINE_PLUGIN_DATA_DIR".into(), data_dir.into()));
    env.push((
        "CAFFEINE_PROTOCOL_VERSION".into(),
        caffeine_plugin_api::PROTOCOL_VERSION.to_string().into(),
    ));
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vec<(OsString, OsString)> {
        [
            ("PATH", "/bin"),
            ("Path", "C:\\bin"),
            ("GITHUB_TOKEN", "secret"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
            ("CAFFEINE_HOME", "/x"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }

    fn names(env: &[(OsString, OsString)]) -> Vec<String> {
        env.iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn minimal_environment_drops_secrets() {
        let env = build(vars(), false, "p", Path::new("/p"), Path::new("/d"));
        let names = names(&env);
        assert!(names.contains(&"PATH".to_string()));
        assert!(names.contains(&"Path".to_string()));
        assert!(
            !names
                .iter()
                .any(|n| n.contains("TOKEN") || n.contains("SECRET"))
        );
        assert!(!names.contains(&"CAFFEINE_HOME".to_string()));
        assert!(names.contains(&"CAFFEINE_PLUGIN_DIR".to_string()));
    }

    #[test]
    fn inherit_passes_everything_but_host_variables() {
        let env = build(vars(), true, "p", Path::new("/p"), Path::new("/d"));
        let names = names(&env);
        assert!(names.contains(&"GITHUB_TOKEN".to_string()));
        assert!(!names.contains(&"CAFFEINE_HOME".to_string()));
    }
}
