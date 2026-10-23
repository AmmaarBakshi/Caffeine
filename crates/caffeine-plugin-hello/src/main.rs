//! A Caffeine plugin.
//!
//! Build with `cargo build --release`, check with
//! `caffeine plugin validate .`, then `caffeine plugin install .` and
//! `caffeine plugin enable hello`. Its commands are then available as
//! `caffeine hello ...`.
//!
//! The process speaks the plugin protocol on stdin/stdout, so never print to
//! stdout directly: use `host.print` for output and `host.data` for
//! machine-readable results.

use std::process::ExitCode;
use std::time::Duration;

use caffeine_plugin_api::plugin::Initialize;
use caffeine_plugin_api::{
    Check, CheckStatus, Host, Invocation, LogLevel, Manifest, Plugin, PluginError,
};
use serde_json::json;

/// The manifest is compiled in so the executable always describes itself
/// exactly like the `caffeine-plugin.toml` installed next to it.
const MANIFEST: &str = include_str!("../caffeine-plugin.toml");

#[derive(Default)]
struct Hello {
    greeting: String,
}

impl Plugin for Hello {
    fn manifest(&self) -> Manifest {
        Manifest::from_toml(MANIFEST).expect("bundled manifest is valid")
    }

    fn initialize(&mut self, init: &Initialize) -> Result<(), PluginError> {
        self.greeting = init
            .settings
            .get("greeting")
            .and_then(|v| v.as_str())
            .unwrap_or("Hi")
            .to_string();
        Ok(())
    }

    fn invoke(&mut self, inv: &Invocation, host: &mut dyn Host) -> Result<i32, PluginError> {
        match inv.command.as_str() {
            "hello" => {
                let names = if inv.args.is_empty() {
                    vec!["world".to_string()]
                } else {
                    inv.args.clone()
                };
                for name in &names {
                    host.print(&format!("{}, {name}!", self.greeting));
                }
                host.data(json!({ "greeting": self.greeting, "names": names }));
                Ok(0)
            }
            "hello-wait" => {
                let secs: u64 = inv
                    .args
                    .first()
                    .ok_or_else(|| {
                        PluginError::new("missing SECONDS").with_hint("usage: hello-wait SECONDS")
                    })?
                    .parse()
                    .map_err(|_| PluginError::new("SECONDS must be a whole number"))?;
                host.log(LogLevel::Info, &format!("waiting {secs}s"));
                host.print(&format!(
                    "{}! Waiting {secs}s while the system stays awake.",
                    self.greeting
                ));
                std::thread::sleep(Duration::from_secs(secs));
                host.data(json!({ "waited_secs": secs }));
                Ok(0)
            }
            other => Err(PluginError::new(format!("unknown command {other}"))),
        }
    }

    fn diagnose(&mut self) -> Vec<Check> {
        if self.greeting.trim().is_empty() {
            vec![
                Check::new("hello.greeting", CheckStatus::Warn, "the greeting is empty")
                    .with_hint("set plugins.settings.hello.greeting"),
            ]
        } else {
            vec![Check::new(
                "hello.greeting",
                CheckStatus::Ok,
                format!("greeting is \"{}\"", self.greeting),
            )]
        }
    }
}

fn main() -> ExitCode {
    caffeine_plugin_api::sdk::serve(Hello::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_valid() {
        let manifest = Hello::default().manifest();
        assert!(manifest.validate().is_empty(), "{:?}", manifest.validate());
    }
}
