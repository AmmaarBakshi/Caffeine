//! Serve a [`Plugin`] over the subprocess protocol.
//!
//! A third-party plugin's `main` is usually just:
//!
//! ```no_run
//! # use caffeine_plugin_api::{Plugin, Manifest, Invocation, Host, PluginError};
//! # struct Hello;
//! # impl Plugin for Hello {
//! #     fn manifest(&self) -> Manifest { unimplemented!() }
//! #     fn invoke(&mut self, _: &Invocation, _: &mut dyn Host) -> Result<i32, PluginError> { Ok(0) }
//! # }
//! fn main() -> std::process::ExitCode {
//!     caffeine_plugin_api::sdk::serve(Hello)
//! }
//! ```

use std::io::{BufRead, Write};
use std::process::{ExitCode, Stdio};

use serde_json::Value;

use crate::PROTOCOL_VERSION;
use crate::plugin::{Host, Initialize, LogLevel, Plugin, PluginError};
use crate::protocol::{HostMessage, MAX_LINE_BYTES, PluginMessage, encode};

/// Serve `plugin` on stdin/stdout until the host sends `shutdown` or
/// closes stdin.
pub fn serve<P: Plugin>(plugin: P) -> ExitCode {
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    match serve_io(plugin, stdin, stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("plugin protocol error: {e}");
            ExitCode::from(70)
        }
    }
}

/// [`serve`] over arbitrary streams; useful for tests.
pub fn serve_io<P: Plugin, R: BufRead, W: Write>(
    mut plugin: P,
    mut reader: R,
    mut writer: W,
) -> Result<(), String> {
    let mut initialized = false;
    let mut line = String::new();
    let result = loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break Ok(()),
            Ok(n) if n > MAX_LINE_BYTES => break Err("request line too long".to_string()),
            Ok(_) => {}
            Err(e) => break Err(format!("reading from the host: {e}")),
        }
        if line.trim().is_empty() {
            continue;
        }
        let message: HostMessage = match serde_json::from_str(line.trim()) {
            Ok(m) => m,
            Err(e) => {
                send(
                    &mut writer,
                    &PluginMessage::Failed {
                        error: PluginError::new(format!("malformed request: {e}")),
                    },
                )?;
                continue;
            }
        };
        match message {
            HostMessage::Initialize {
                protocol,
                host,
                settings,
                permissions,
                data_dir,
            } => {
                if protocol.major != PROTOCOL_VERSION.major {
                    send(
                        &mut writer,
                        &PluginMessage::Failed {
                            error: PluginError::new(format!(
                                "host speaks protocol {protocol}, plugin speaks {PROTOCOL_VERSION}"
                            )),
                        },
                    )?;
                    break Ok(());
                }
                let init = Initialize {
                    host,
                    settings,
                    permissions,
                    data_dir,
                };
                match plugin.initialize(&init) {
                    Ok(()) => {
                        initialized = true;
                        let manifest = plugin.manifest();
                        send(
                            &mut writer,
                            &PluginMessage::Ready {
                                protocol: PROTOCOL_VERSION,
                                name: manifest.plugin.name,
                                version: manifest.plugin.version,
                            },
                        )?;
                    }
                    Err(error) => send(&mut writer, &PluginMessage::Failed { error })?,
                }
            }
            HostMessage::Invoke(_) | HostMessage::Diagnose if !initialized => send(
                &mut writer,
                &PluginMessage::Failed {
                    error: PluginError::new("request received before initialize"),
                },
            )?,
            HostMessage::Invoke(invocation) => {
                let mut host = StdioHost {
                    writer: &mut writer,
                    error: None,
                };
                let outcome = plugin.invoke(&invocation, &mut host);
                if let Some(e) = host.error {
                    break Err(e);
                }
                match outcome {
                    Ok(exit_code) => send(&mut writer, &PluginMessage::Result { exit_code })?,
                    Err(error) => send(&mut writer, &PluginMessage::Failed { error })?,
                }
            }
            HostMessage::Diagnose => {
                let checks = plugin.diagnose();
                send(&mut writer, &PluginMessage::Diagnostics { checks })?;
            }
            HostMessage::Shutdown => break Ok(()),
        }
    };
    plugin.shutdown();
    result
}

fn send<W: Write>(writer: &mut W, message: &PluginMessage) -> Result<(), String> {
    writer
        .write_all(encode(message).as_bytes())
        .and_then(|_| writer.flush())
        .map_err(|e| format!("writing to the host: {e}"))
}

struct StdioHost<'a, W: Write> {
    writer: &'a mut W,
    error: Option<String>,
}

impl<W: Write> StdioHost<'_, W> {
    fn emit(&mut self, message: PluginMessage) {
        if self.error.is_none()
            && let Err(e) = send(self.writer, &message)
        {
            self.error = Some(e);
        }
    }
}

impl<W: Write> Host for StdioHost<'_, W> {
    fn log(&mut self, level: LogLevel, message: &str) {
        self.emit(PluginMessage::Log {
            level,
            message: message.to_string(),
        });
    }

    fn print(&mut self, text: &str) {
        self.emit(PluginMessage::Output {
            text: text.to_string(),
        });
    }

    fn data(&mut self, value: Value) {
        self.emit(PluginMessage::Data { value });
    }
}

/// A `Stdio` for the stdout of programs a plugin starts: the plugin's own
/// stderr, which the host passes through to the terminal. Using it keeps
/// child output from corrupting the protocol channel.
pub fn child_stdout() -> Stdio {
    Stdio::from(std::io::stderr())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;
    use crate::plugin::{Invocation, PluginError};

    struct Echo {
        shut_down: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Plugin for Echo {
        fn manifest(&self) -> Manifest {
            Manifest::from_toml(
                r#"
[plugin]
name = "echo"
version = "1.2.3"
description = "Echoes"
api = "^1"
[[commands]]
name = "echo"
summary = "Echo arguments"
"#,
            )
            .unwrap()
        }

        fn invoke(&mut self, inv: &Invocation, host: &mut dyn Host) -> Result<i32, PluginError> {
            if inv.args.first().map(String::as_str) == Some("fail") {
                return Err(PluginError::new("asked to fail"));
            }
            host.log(LogLevel::Info, "echoing");
            host.print(&inv.args.join(" "));
            host.data(Value::from(inv.args.len()));
            Ok(0)
        }

        fn shutdown(&mut self) {
            self.shut_down
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn run(requests: &[&str]) -> (Vec<PluginMessage>, bool) {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let input = requests.join("\n");
        let mut output = Vec::new();
        serve_io(
            Echo {
                shut_down: flag.clone(),
            },
            input.as_bytes(),
            &mut output,
        )
        .unwrap();
        let messages = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (messages, flag.load(std::sync::atomic::Ordering::SeqCst))
    }

    const INIT: &str = r#"{"type":"initialize","protocol":"1.0.0","host":{"name":"caffeine","version":"0.1.0","os":"linux","arch":"x86_64"},"settings":{},"permissions":[],"data_dir":"/tmp"}"#;

    #[test]
    fn full_lifecycle() {
        let (msgs, shut_down) = run(&[
            INIT,
            r#"{"type":"invoke","command":"echo","args":["a","b"],"cwd":"/","json":false}"#,
            r#"{"type":"shutdown"}"#,
        ]);
        assert!(shut_down);
        assert!(matches!(&msgs[0], PluginMessage::Ready { name, .. } if name == "echo"));
        assert!(matches!(
            &msgs[1],
            PluginMessage::Log {
                level: LogLevel::Info,
                ..
            }
        ));
        assert_eq!(msgs[2], PluginMessage::Output { text: "a b".into() });
        assert_eq!(
            msgs[3],
            PluginMessage::Data {
                value: Value::from(2)
            }
        );
        assert_eq!(msgs[4], PluginMessage::Result { exit_code: 0 });
        assert_eq!(msgs.len(), 5);
    }

    #[test]
    fn errors_are_reported_not_fatal() {
        let (msgs, _) = run(&[
            r#"{"type":"invoke","command":"echo","args":[],"cwd":"/","json":false}"#,
            "not json",
            INIT,
            r#"{"type":"invoke","command":"echo","args":["fail"],"cwd":"/","json":false}"#,
        ]);
        assert!(
            matches!(&msgs[0], PluginMessage::Failed { error } if error.message.contains("before initialize"))
        );
        assert!(
            matches!(&msgs[1], PluginMessage::Failed { error } if error.message.contains("malformed"))
        );
        assert!(matches!(&msgs[2], PluginMessage::Ready { .. }));
        assert!(
            matches!(&msgs[3], PluginMessage::Failed { error } if error.message == "asked to fail")
        );
    }

    #[test]
    fn refuses_other_major_protocol() {
        let init = INIT.replace("\"1.0.0\"", "\"2.0.0\"");
        let (msgs, _) = run(&[&init]);
        assert!(matches!(&msgs[0], PluginMessage::Failed { .. }));
        assert_eq!(msgs.len(), 1);
    }
}
